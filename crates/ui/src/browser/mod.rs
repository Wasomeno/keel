//! Browser pane: what the address bar accepts, which pages may load, and
//! which keys a focused page hands back to Keel. The browser engine is
//! CEF (Chromium Embedded Framework) in `cef`.

pub mod agent;
pub mod cef;
pub use self::cef::{BACKGROUND_SIZE, NativeEvent, NativePage, Sender};


/// What the toolbar shows, read from the page after each change.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageState {
    pub url: Option<String>,
    pub title: String,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
    /// 0..1 while loading.
    pub progress: f32,
}

/// Pages and frames may load web content only: no `file:`, no app schemes.
pub fn allowed_navigation(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["http://", "https://", "about:", "blob:", "data:"].iter().any(|s| lower.starts_with(s))
}

/// Typed address → URL. Loopback hosts get `http://`, other hosts
/// `https://`, and anything that isn't an address becomes a search.
pub fn normalize_address(input: &str) -> Option<String> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }
    if let Some((scheme, _)) = text.split_once("://") {
        return matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https").then(|| text.to_string());
    }
    let host = text.split(['/', '?', '#']).next().unwrap_or(text);
    let bare = host.rsplit_once(':').map_or(host, |(h, port)| if port.chars().all(|c| c.is_ascii_digit()) { h } else { host });
    let loopback = bare == "localhost" || bare.ends_with(".localhost") || bare.starts_with("127.") || bare == "[::1]" || bare == "0.0.0.0";
    if loopback {
        return Some(format!("http://{text}"));
    }
    if !text.contains(char::is_whitespace) && bare.contains('.') && !bare.starts_with('.') && !bare.ends_with('.') {
        return Some(format!("https://{text}"));
    }
    Some(format!("https://duckduckgo.com/?q={}", encode_query(text)))
}

fn encode_query(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// A key typed into a focused page that Keel takes instead: ⌘ shortcuts
/// other than the page's own editing and find keys, plus ⌃` and ⌃Tab.
pub fn forwarded_key(cmd: bool, ctrl: bool, key: &str) -> bool {
    if cmd {
        !matches!(key, "c" | "v" | "x" | "a" | "z" | "f" | "g")
    } else {
        ctrl && matches!(key, "`" | "tab")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_pick_a_scheme_or_search() {
        assert_eq!(normalize_address("localhost:3000").as_deref(), Some("http://localhost:3000"));
        assert_eq!(normalize_address("127.0.0.1:8080/x").as_deref(), Some("http://127.0.0.1:8080/x"));
        assert_eq!(normalize_address("app.localhost").as_deref(), Some("http://app.localhost"));
        assert_eq!(normalize_address("example.com/a?b").as_deref(), Some("https://example.com/a?b"));
        assert_eq!(normalize_address("HTTP://x.dev").as_deref(), Some("HTTP://x.dev"));
        assert_eq!(normalize_address("rust gpui").as_deref(), Some("https://duckduckgo.com/?q=rust+gpui"));
        assert_eq!(normalize_address("c++").as_deref(), Some("https://duckduckgo.com/?q=c%2B%2B"));
        assert_eq!(normalize_address("file:///etc/passwd"), None);
        assert_eq!(normalize_address("  "), None);
    }

    #[test]
    fn only_web_content_loads() {
        assert!(allowed_navigation("https://a.b"));
        assert!(allowed_navigation("about:blank"));
        assert!(!allowed_navigation("file:///etc/hosts"));
        assert!(!allowed_navigation("vscode://open"));
        assert!(!allowed_navigation("javascript:alert(1)"));
    }

    #[test]
    fn editing_keys_stay_with_the_page() {
        assert!(!forwarded_key(true, false, "c"));
        assert!(!forwarded_key(true, false, "f"));
        assert!(forwarded_key(true, false, "l"));
        assert!(forwarded_key(true, false, "b"));
        assert!(forwarded_key(false, true, "`"));
        assert!(!forwarded_key(false, true, "a"));
        assert!(!forwarded_key(false, false, "l"));
    }
}
