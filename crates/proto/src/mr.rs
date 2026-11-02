//! GitLab merge request references, shared by the engine (generative UI)
//! and the window (the MR monitor).

use serde::{Deserialize, Serialize};

/// A merge request by where it lives.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MrRef {
    /// `https://gitlab.com`: scheme and host, no trailing slash.
    pub host: String,
    /// `group/project`.
    pub project: String,
    pub iid: u64,
}

impl MrRef {
    pub fn key(&self) -> String {
        format!("{}/{}!{}", self.host, self.project, self.iid)
    }

    pub fn web_url(&self) -> String {
        format!("{}/{}/-/merge_requests/{}", self.host, self.project, self.iid)
    }

    pub fn label(&self) -> String {
        format!("!{}", self.iid)
    }
}

/// GitLab MR links in `text`: `https://host/group/proj/-/merge_requests/42`.
pub fn find_mr_urls(text: &str) -> Vec<MrRef> {
    const MARK: &str = "/-/merge_requests/";
    let mut found: Vec<MrRef> = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(MARK).map(|i| from + i) {
        from = at + MARK.len();
        let digits: String = text[from..].chars().take_while(char::is_ascii_digit).collect();
        let Ok(iid) = digits.parse::<u64>() else {
            continue;
        };
        let head = &text[..at];
        let Some(start) = head.rfind("https://").or_else(|| head.rfind("http://")) else {
            continue;
        };
        if let Some(mr) = parse_url(&format!("{}{MARK}{iid}", &text[start..at]))
            && !found.contains(&mr)
        {
            found.push(mr);
        }
    }
    found
}

/// One MR URL (anything after the number is ignored).
pub fn parse_url(url: &str) -> Option<MrRef> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    let (authority, path) = rest.split_once('/')?;
    let (project, tail) = path.split_once("/-/merge_requests/")?;
    let iid: u64 = tail.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()?;
    let valid = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/');
    if authority.is_empty() || !authority.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':')) {
        return None;
    }
    if project.is_empty() || !project.chars().all(valid) {
        return None;
    }
    Some(MrRef { host: format!("{}://{}", scheme.to_ascii_lowercase(), authority.to_ascii_lowercase()), project: project.to_string(), iid })
}

/// A git remote as (host origin, project): `git@gitlab.com:g/p.git`,
/// `ssh://git@host:22/g/p.git`, `https://host/g/p.git`.
pub fn parse_remote(remote: &str) -> Option<(String, String)> {
    let remote = remote.trim();
    let (host, path) = if let Some((scheme, rest)) = remote.split_once("://") {
        let rest = rest.split_once('/').map_or(rest.to_string(), |(auth, path)| {
            let auth = auth.rsplit_once('@').map_or(auth, |(_, a)| a);
            format!("{auth}/{path}")
        });
        let (authority, path) = rest.split_once('/')?;
        let host = if scheme.starts_with("http") { authority } else { authority.split(':').next()? };
        (host.to_string(), path.to_string())
    } else {
        let rest = remote.rsplit_once('@').map_or(remote, |(_, r)| r);
        let (host, path) = rest.split_once(':')?;
        (host.to_string(), path.to_string())
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git").to_string();
    (!host.is_empty() && path.contains('/')).then(|| (format!("https://{}", host.to_ascii_lowercase()), path))
}

/// `!42`-style MR mentions in `text`, in order: a `!` at a word start, then
/// digits, then not a letter or digit.
pub fn find_bang_iids(text: &str) -> Vec<u64> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    for (at, _) in text.match_indices('!') {
        if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'/') {
            continue;
        }
        let digits: String = text[at + 1..].chars().take_while(char::is_ascii_digit).collect();
        let next = text[at + 1 + digits.len()..].chars().next();
        if let Ok(iid) = digits.parse::<u64>()
            && !next.is_some_and(char::is_alphanumeric)
        {
            found.push(iid);
        }
    }
    found
}
