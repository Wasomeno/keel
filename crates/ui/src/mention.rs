//! `@` references in the composer: finding the token being typed and
//! ranking the chat folder's paths, and the picked repo's issues, against it.

/// The `@` token the caret is in: the byte offset of its `@` and the text
/// typed after it. Only at the start of the text or after whitespace (so an
/// email address doesn't open the menu), and only until whitespace.
pub fn mention_query(text: &str, cursor: usize) -> Option<(usize, &str)> {
    let before = text.get(..cursor)?;
    let start = before.rfind(char::is_whitespace).map_or(0, |ix| {
        ix + before[ix..].chars().next().map_or(1, char::len_utf8)
    });
    let token = &before[start..];
    let query = token.strip_prefix('@')?;
    Some((start, query))
}

/// The `/` command being typed: a word starting with `/`, at the start of
/// the text or after whitespace, up to the caret. Returns the byte offset of
/// its `/` and what follows it. A second `/` means a path (`/usr/bin`), not a
/// command; command names nest with `:`.
pub fn slash_query(text: &str, cursor: usize) -> Option<(usize, &str)> {
    let before = text.get(..cursor)?;
    let start = before.rfind(char::is_whitespace).map_or(0, |ix| {
        ix + before[ix..].chars().next().map_or(1, char::len_utf8)
    });
    let query = before[start..].strip_prefix('/')?;
    (!query.contains('/')).then_some((start, query))
}

/// Up to `limit` of `commands` (name, description) for `query`, best first:
/// name starts with it, then name contains it, then the description does.
/// Ties keep their given order. An empty query lists the first `limit`.
pub fn rank_commands(commands: &[(&str, &str)], query: &str, limit: usize) -> Vec<usize> {
    let query = query.to_lowercase();
    let mut scored: Vec<(u8, usize)> = commands
        .iter()
        .enumerate()
        .filter_map(|(ix, (name, description))| {
            let name = name.to_lowercase();
            let score = if query.is_empty() || name.starts_with(&query) {
                0
            } else if name.contains(&query) {
                1
            } else if description.to_lowercase().contains(&query) {
                2
            } else {
                return None;
            };
            Some((score, ix))
        })
        .collect();
    scored.sort();
    scored.into_iter().take(limit).map(|(_, ix)| ix).collect()
}

/// Up to `limit` paths for `query`, best first: file name starts with it,
/// then contains it, then the path contains it, then its letters appear in
/// order. Ties go to shorter paths. An empty query lists the shallowest.
/// A query with a `/` looks inside that folder: `src/ma` ranks `src/`'s
/// contents against `ma`.
pub fn rank<'a>(paths: &'a [String], query: &str, limit: usize) -> Vec<&'a str> {
    let query = query.to_lowercase();
    let (scope, query) = match query.rsplit_once('/') {
        Some((dir, rest)) => (format!("{dir}/"), rest.to_string()),
        None => (String::new(), query),
    };
    let mut scored: Vec<(u8, usize, &str)> = paths
        .iter()
        .filter_map(|path| {
            let full = path.to_lowercase();
            let lower = full
                .strip_prefix(&scope)
                .filter(|rest| !rest.is_empty())?
                .to_string();
            let name = lower
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(&lower);
            let score = if query.is_empty() {
                lower.trim_end_matches('/').matches('/').count().min(9) as u8
            } else if name.starts_with(&query) {
                0
            } else if name.contains(&query) {
                1
            } else if lower.contains(&query) {
                2
            } else if in_order(&lower, &query) {
                3
            } else {
                return None;
            };
            Some((score, path.len(), path.as_str()))
        })
        .collect();
    scored.sort();
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, path)| path)
        .collect()
}

/// Up to `limit` of `issues` (number, title) for `query`, in their given
/// order: `#12` or `12` matches numbers starting with it, anything else a
/// title containing it. An empty query lists the first `limit`.
pub fn rank_issues(issues: &[(u64, &str)], query: &str, limit: usize) -> Vec<usize> {
    let query = query.to_lowercase();
    let number = query.strip_prefix('#').unwrap_or(&query);
    let by_number = !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit());
    issues
        .iter()
        .enumerate()
        .filter(|(_, (iid, title))| {
            query.is_empty()
                || (by_number && iid.to_string().starts_with(number))
                || title.to_lowercase().contains(&query)
        })
        .map(|(ix, _)| ix)
        .take(limit)
        .collect()
}

fn in_order(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|n| chars.any(|h| h == n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_token_under_the_caret() {
        assert_eq!(mention_query("@", 1), Some((0, "")));
        assert_eq!(mention_query("look at @src/ma", 15), Some((8, "src/ma")));
        assert_eq!(
            mention_query("look at @src/ma now", 15),
            Some((8, "src/ma"))
        );
        assert_eq!(mention_query("mail me@x.com", 13), None);
        assert_eq!(mention_query("@done now", 9), None);
        assert_eq!(mention_query("é @x", 5), Some((3, "x")));
    }

    #[test]
    fn slash_starts_any_word_but_not_a_path() {
        assert_eq!(slash_query("/", 1), Some((0, "")));
        assert_eq!(slash_query("/rev", 4), Some((0, "rev")));
        assert_eq!(slash_query("/review the diff", 7), Some((0, "review")));
        assert_eq!(slash_query("/review the diff", 12), None);
        // Mid-sentence too.
        assert_eq!(slash_query("please /rev", 11), Some((7, "rev")));
        assert_eq!(
            slash_query("use /skill:brand now", 16),
            Some((4, "skill:brand"))
        );
        // Inside a word, or a path: not a command.
        assert_eq!(slash_query("and/or", 6), None);
        assert_eq!(slash_query("open /usr/bin", 13), None);
        assert_eq!(slash_query("", 0), None);
    }

    #[test]
    fn ranks_commands_by_name_then_description() {
        let commands = [
            ("deploy", "Ship to staging"),
            ("review", "Review the diff"),
            ("pr-review", "Open a PR"),
            ("lint", "review style"),
        ];
        assert_eq!(rank_commands(&commands, "rev", 5), [1, 2, 3]);
        assert_eq!(rank_commands(&commands, "", 2), [0, 1]);
        assert_eq!(rank_commands(&commands, "STAG", 5), [0]);
        assert!(rank_commands(&commands, "zzz", 5).is_empty());
    }

    #[test]
    fn matches_issues_by_number_or_title() {
        let issues = [
            (12, "Fix login crash"),
            (120, "Dark mode"),
            (7, "Login page copy"),
        ];
        assert_eq!(rank_issues(&issues, "", 2), [0, 1]);
        assert_eq!(rank_issues(&issues, "#12", 5), [0, 1]);
        assert_eq!(rank_issues(&issues, "7", 5), [2]);
        assert_eq!(rank_issues(&issues, "login", 5), [0, 2]);
        assert!(rank_issues(&issues, "src/", 5).is_empty());
    }

    #[test]
    fn ranks_names_before_paths_before_loose_matches() {
        let paths: Vec<String> = [
            "README.md",
            "src/",
            "src/main.rs",
            "src/ui/",
            "src/ui/main_pane.rs",
            "docs/domain.md",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            rank(&paths, "main", 5),
            ["src/main.rs", "src/ui/main_pane.rs", "docs/domain.md"]
        );
        assert_eq!(rank(&paths, "ui", 5), ["src/ui/", "src/ui/main_pane.rs"]);
        assert_eq!(rank(&paths, "", 3), ["src/", "README.md", "src/ui/"]);
        assert!(rank(&paths, "zzz", 5).is_empty());
        // Inside a folder: its contents, ranked by what follows the slash.
        assert_eq!(
            rank(&paths, "src/", 3),
            ["src/ui/", "src/main.rs", "src/ui/main_pane.rs"]
        );
        assert_eq!(rank(&paths, "src/ui/ma", 3), ["src/ui/main_pane.rs"]);
    }
}
