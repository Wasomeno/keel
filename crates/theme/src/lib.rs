//! Paint tokens for the Keel window.

pub mod schema;

pub use keel_proto::ThemeChoice;
pub use schema::{SyntaxPaintMap, Tokens, contrast};

/// Resolve theme choice against system appearance.
///
/// If `choice` is absent (`None`), follows `system_dark`.
pub fn resolve_theme(choice: Option<ThemeChoice>, system_dark: bool) -> Tokens {
    match choice {
        Some(ThemeChoice::Light) => Tokens::light(),
        Some(ThemeChoice::Dark) => Tokens::dark(),
        None => {
            if system_dark {
                Tokens::dark()
            } else {
                Tokens::light()
            }
        }
    }
}

/// Resolve syntax color paint map against system appearance.
pub fn resolve_syntax_theme(choice: Option<ThemeChoice>, system_dark: bool) -> SyntaxPaintMap {
    match choice {
        Some(ThemeChoice::Light) => SyntaxPaintMap::light(),
        Some(ThemeChoice::Dark) => SyntaxPaintMap::dark(),
        None => {
            if system_dark {
                SyntaxPaintMap::dark()
            } else {
                SyntaxPaintMap::light()
            }
        }
    }
}

/// Reduced motion follows OS setting unless `ui-settings.json` overrides it.
pub fn resolve_reduced_motion(override_val: Option<bool>, os_val: bool) -> bool {
    override_val.unwrap_or(os_val)
}

/// Query the macOS NSWorkspace accessibility reduce-motion setting.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
pub fn os_reduced_motion() -> bool {
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let workspace: *mut objc::runtime::Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let reduce: bool = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduce
    }
}

#[cfg(not(target_os = "macos"))]
pub fn os_reduced_motion() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_tokens_match_semantic_rules() {
        let light = Tokens::light();
        let dark = Tokens::dark();
        assert_ne!(light.window, dark.window);
        assert_ne!(light.text, dark.text);

        // Explicit choice overrides system appearance
        assert_eq!(resolve_theme(Some(ThemeChoice::Light), true), light);
        assert_eq!(resolve_theme(Some(ThemeChoice::Light), false), light);
        assert_eq!(resolve_theme(Some(ThemeChoice::Dark), true), dark);
        assert_eq!(resolve_theme(Some(ThemeChoice::Dark), false), dark);

        // Absent choice follows system appearance
        assert_eq!(resolve_theme(None, false), light);
        assert_eq!(resolve_theme(None, true), dark);
    }

    #[test]
    fn reduced_motion_override_and_injection() {
        // Tests inject the OS value and the override without depending on current Mac appearance
        assert!(!resolve_reduced_motion(Some(false), true));
        assert!(resolve_reduced_motion(Some(true), false));
        assert!(resolve_reduced_motion(None, true));
        assert!(!resolve_reduced_motion(None, false));
    }
}
