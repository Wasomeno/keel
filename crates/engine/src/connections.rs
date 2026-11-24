//! Connected services (GitLab, Dotify): which hosts are connected and as
//! whom, in `{data_dir}/connections.json`, with each token in its own file
//! under `{data_dir}/connections/`, readable only by the user (0600, in a
//! 0700 folder). Not the Keychain: its items trust only the build that made
//! them, so every rebuild or second copy of Keel asked for the login
//! password. Tokens never leave the engine except to the host they belong to.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const VERIFY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// "gitlab".
    pub kind: String,
    /// `https://gitlab.com` or a self-hosted origin, no trailing slash.
    pub host: String,
    pub username: String,
    pub name: String,
}

/// `gitlab.example.com`, `https://gitlab.example.com/` and
/// `https://gitlab.example.com/some/path` all become
/// `https://gitlab.example.com`. Plain `http://` is kept only when typed.
pub fn normalize_host(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("enter a GitLab address".into());
    }
    let (scheme, rest) = match input.split_once("://") {
        Some((scheme, rest))
            if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") =>
        {
            (scheme.to_ascii_lowercase(), rest)
        }
        Some(_) => return Err("the address must start with https://".into()),
        None => ("https".to_string(), input),
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if authority.is_empty() || authority.contains(char::is_whitespace) || authority.contains('@') {
        return Err("that doesn't look like a host name".into());
    }
    Ok(format!("{scheme}://{authority}"))
}

/// Ask the host who the token belongs to (`GET /api/v4/user`).
pub fn verify_gitlab(host: &str, token: &str) -> Result<(String, String), String> {
    let response = ureq::get(&format!("{host}/api/v4/user"))
        .set("PRIVATE-TOKEN", token)
        .timeout(VERIFY_TIMEOUT)
        .call();
    let body: serde_json::Value = match response {
        Ok(ok) => ok
            .into_json()
            .map_err(|err| format!("unexpected answer from {host}: {err}"))?,
        Err(ureq::Error::Status(401, _)) => return Err("GitLab rejected the token (401)".into()),
        Err(ureq::Error::Status(403, _)) => {
            return Err("the token lacks the read_user or api scope (403)".into());
        }
        Err(ureq::Error::Status(code, _)) => return Err(format!("{host} answered {code}")),
        Err(err) => return Err(format!("couldn't reach {host}: {err}")),
    };
    let username = body["username"]
        .as_str()
        .ok_or("that host didn't answer like GitLab")?
        .to_string();
    let name = body["name"].as_str().unwrap_or(&username).to_string();
    Ok((username, name))
}

/// `GET {host}/api/v4/{path}` with the token kept for `host`. Only hosts
/// connected in Keel (`connections.json`) or authenticated in `glab` CLI
/// (`~/.config/glab-cli/config.yml`) are asked, so a token never goes elsewhere.
pub fn gitlab_get(data_dir: &Path, host: &str, path: &str) -> Result<serde_json::Value, String> {
    let token = resolve_gitlab_token(data_dir, host)?;
    match ureq::get(&format!("{host}/api/v4/{path}"))
        .set("PRIVATE-TOKEN", &token)
        .timeout(VERIFY_TIMEOUT)
        .call()
    {
        Ok(ok) => ok
            .into_json()
            .map_err(|err| format!("unexpected answer from {host}: {err}")),
        Err(ureq::Error::Status(401, _)) => {
            Err("GitLab rejected the token (401); reconnect in Settings".into())
        }
        Err(ureq::Error::Status(403, _)) => {
            Err("the token can't read this (403); it needs the read_api or api scope".into())
        }
        Err(ureq::Error::Status(404, _)) => Err("not found (404)".into()),
        Err(ureq::Error::Status(code, _)) => Err(format!("{host} answered {code}")),
        Err(err) => Err(format!("couldn't reach {host}: {err}")),
    }
}

/// Origins authenticated either in Keel (`connections.json`) or in `glab` CLI.
pub fn gitlab_hosts(data_dir: &Path) -> Vec<String> {
    let mut hosts: Vec<String> = list(data_dir)
        .into_iter()
        .filter(|c| c.kind == "gitlab")
        .map(|c| c.host)
        .collect();
    for (host, _) in load_glab_hosts() {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    hosts
}

fn resolve_gitlab_token(data_dir: &Path, host: &str) -> Result<String, String> {
    if list(data_dir)
        .iter()
        .any(|c| c.kind == "gitlab" && c.host == host)
    {
        return load_token(data_dir, &account("gitlab", host))
            .ok_or_else(|| "the token for this host is missing; reconnect in Settings".to_string());
    }
    if let Some((_, token)) = load_glab_hosts().into_iter().find(|(h, _)| h == host) {
        return Ok(token);
    }
    Err(format!("{host} isn't connected"))
}

fn glab_config_path() -> Option<PathBuf> {
    if std::env::var_os("KEEL_GLAB_DISABLE").is_some_and(|v| v == "1") {
        return None;
    }
    if let Some(dir) = std::env::var_os("GLAB_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("config.yml"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("glab-cli").join("config.yml"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".config/glab-cli/config.yml"))
}

fn load_glab_hosts() -> Vec<(String, String)> {
    let Some(path) = glab_config_path() else {
        return Vec::new();
    };
    let Ok(yaml) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_glab_hosts(&yaml)
}

fn parse_glab_hosts(yaml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_hosts = false;
    let mut hosts_indent = 0usize;
    let mut current_host: Option<String> = None;
    let mut host_indent = 0usize;
    let mut protocol = "https".to_string();
    let mut token: Option<String> = None;

    let flush = |out: &mut Vec<(String, String)>,
                 current_host: &mut Option<String>,
                 protocol: &mut String,
                 token: &mut Option<String>| {
        if let (Some(h), Some(tok)) = (current_host.take(), token.take())
            && !tok.is_empty()
        {
            let proto = if protocol.eq_ignore_ascii_case("http") {
                "http"
            } else {
                "https"
            };
            out.push((format!("{proto}://{}", h.to_ascii_lowercase()), tok));
        }
        *protocol = "https".to_string();
    };

    for raw in yaml.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        if !in_hosts {
            if trimmed == "hosts:" {
                in_hosts = true;
                hosts_indent = indent;
            }
            continue;
        }
        if indent <= hosts_indent {
            flush(&mut out, &mut current_host, &mut protocol, &mut token);
            break;
        }
        if current_host.is_none() || indent <= host_indent {
            flush(&mut out, &mut current_host, &mut protocol, &mut token);
            if let Some(host_name) = trimmed.strip_suffix(':') {
                let host_name = host_name.trim().trim_matches(['"', '\'']);
                if !host_name.is_empty() {
                    current_host = Some(host_name.to_string());
                    host_indent = indent;
                }
            }
            continue;
        }
        if let Some((key, val)) = trimmed.split_once(':') {
            let val = val
                .split(" #")
                .next()
                .unwrap_or(val)
                .trim()
                .trim_matches(['"', '\'']);
            match key.trim() {
                "api_protocol" if !val.is_empty() => protocol = val.to_string(),
                "token" if !val.is_empty() => token = Some(val.to_string()),
                _ => {}
            }
        }
    }
    flush(&mut out, &mut current_host, &mut protocol, &mut token);
    out
}

/// Projects the user is a member of, most recently active first, trimmed to
/// what the list shows.
// ponytail: first 100 only, filtered client-side; page or search server-side if that runs short.
pub fn gitlab_projects(data_dir: &Path, host: &str) -> Result<Vec<serde_json::Value>, String> {
    let rows = gitlab_get(
        data_dir,
        host,
        "projects?membership=true&simple=true&order_by=last_activity_at&per_page=100",
    )?;
    Ok(rows
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|p| {
                    serde_json::json!({
                        "id": p["id"],
                        "name": p["name"],
                        "path": p["path_with_namespace"],
                        "web_url": p["web_url"],
                        "last_activity_at": p["last_activity_at"],
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// What a project's issue board needs: the label lists of its first board
/// (in board order, with colors; empty when it has none), open issues, and
/// the most recently closed ones.
// ponytail: first 100 open / 20 closed; page if boards outgrow that.
pub fn gitlab_board(
    data_dir: &Path,
    host: &str,
    project_id: u64,
) -> Result<serde_json::Value, String> {
    let boards = gitlab_get(data_dir, host, &format!("projects/{project_id}/boards"))?;
    let mut lists: Vec<(i64, serde_json::Value)> = boards[0]["lists"]
        .as_array()
        .map(|lists| {
            lists
                .iter()
                .filter(|l| l["label"]["name"].is_string())
                .map(|l| {
                    (l["position"].as_i64().unwrap_or(0), serde_json::json!({ "label": l["label"]["name"], "color": l["label"]["color"] }))
                })
                .collect()
        })
        .unwrap_or_default();
    lists.sort_by_key(|(position, _)| *position);
    let issues = |state: &str, n: u32| {
        gitlab_get(
            data_dir,
            host,
            &format!("projects/{project_id}/issues?state={state}&order_by=updated_at&per_page={n}"),
        )
        .map(|rows| issue_rows(&rows))
    };
    Ok(serde_json::json!({
        "lists": lists.into_iter().map(|(_, l)| l).collect::<Vec<_>>(),
        "opened": issues("opened", 100)?,
        "closed": issues("closed", 20)?,
    }))
}

/// One merge request, trimmed to what a session's MR monitor and card show.
/// Fetches the MR record plus best-effort diff stats (`files_changed`,
/// `additions`, `deletions`) from `/diffs`. `project` is `group/project` (or a
/// numeric id).
pub fn gitlab_mr(
    data_dir: &Path,
    host: &str,
    project: &str,
    iid: u64,
) -> Result<serde_json::Value, String> {
    let encoded = encode_segment(project);
    let mr = gitlab_get(
        data_dir,
        host,
        &format!("projects/{encoded}/merge_requests/{iid}"),
    )?;
    let pipeline = if mr["head_pipeline"]["status"].is_string() {
        &mr["head_pipeline"]["status"]
    } else {
        &mr["pipeline"]["status"]
    };
    let diff_stats = gitlab_mr_diff_stats(data_dir, host, &encoded, iid);
    let changes_count = parse_changes_count(&mr["changes_count"]);
    let files_changed = match (diff_stats, changes_count) {
        (Some((files, _, _)), Some(count)) => Some(files.max(count)),
        (Some((files, _, _)), None) => Some(files),
        (None, Some(count)) => Some(count),
        (None, None) => None,
    };
    let additions = diff_stats.map(|(_, add, _)| add);
    let deletions = diff_stats.map(|(_, _, del)| del);
    Ok(serde_json::json!({
        "iid": mr["iid"],
        "title": mr["title"],
        "state": mr["state"],
        "draft": mr["draft"].as_bool().or(mr["work_in_progress"].as_bool()).unwrap_or(false),
        "detailed_merge_status": mr["detailed_merge_status"],
        "merge_status": mr["merge_status"],
        "has_conflicts": mr["has_conflicts"].as_bool().unwrap_or(false),
        "pipeline": pipeline,
        "source_branch": mr["source_branch"],
        "target_branch": mr["target_branch"],
        "created_at": mr["created_at"],
        "merged_at": mr["merged_at"],
        "closed_at": mr["closed_at"],
        "updated_at": mr["updated_at"],
        "files_changed": files_changed,
        "additions": additions,
        "deletions": deletions,
        "web_url": mr["web_url"],
    }))
}

fn gitlab_mr_diff_stats(
    data_dir: &Path,
    host: &str,
    encoded_project: &str,
    iid: u64,
) -> Option<(u64, u64, u64)> {
    const PER_PAGE: usize = 100;
    const MAX_PAGES: usize = 3;
    let mut files = 0u64;
    let mut additions = 0u64;
    let mut deletions = 0u64;
    let mut any_page = false;
    for page in 1..=MAX_PAGES {
        let Ok(rows) = gitlab_get(
            data_dir,
            host,
            &format!("projects/{encoded_project}/merge_requests/{iid}/diffs?per_page={PER_PAGE}&page={page}"),
        ) else {
            break;
        };
        let Some(items) = rows.as_array() else {
            break;
        };
        any_page = true;
        let (f, a, d) = count_diff_stats(items);
        files = files.saturating_add(f);
        additions = additions.saturating_add(a);
        deletions = deletions.saturating_add(d);
        if items.len() < PER_PAGE {
            break;
        }
    }
    any_page.then_some((files, additions, deletions))
}

fn count_diff_stats(diffs: &[serde_json::Value]) -> (u64, u64, u64) {
    let mut additions = 0u64;
    let mut deletions = 0u64;
    for entry in diffs {
        if let Some(patch) = entry["diff"].as_str() {
            for line in patch.lines() {
                if line.starts_with('+') && !line.starts_with("+++") {
                    additions = additions.saturating_add(1);
                } else if line.starts_with('-') && !line.starts_with("---") {
                    deletions = deletions.saturating_add(1);
                }
            }
        }
    }
    (diffs.len() as u64, additions, deletions)
}

fn parse_changes_count(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_str()
            .and_then(|raw| raw.trim().trim_end_matches('+').parse::<u64>().ok())
    })
}

/// Merge an MR, after checking it live: open, not a draft, no conflicts,
/// pipeline (if any) passed, and GitLab itself calls it mergeable. The merge
/// names the head commit it checked, so a push in between fails rather than
/// merging code nobody looked at. Returns the MR afterwards, as [`gitlab_mr`].
pub fn gitlab_merge(
    data_dir: &Path,
    host: &str,
    project: &str,
    iid: u64,
) -> Result<serde_json::Value, String> {
    let path = format!("projects/{}/merge_requests/{iid}", encode_segment(project));
    let mr = gitlab_get(data_dir, host, &path)?;
    merge_blocker(&mr).map_or(Ok(()), Err)?;
    let sha = mr["sha"]
        .as_str()
        .ok_or("GitLab didn't say which commit the MR is at")?;
    let token = resolve_gitlab_token(data_dir, host)?;
    match ureq::put(&format!("{host}/api/v4/{path}/merge"))
        .set("PRIVATE-TOKEN", &token)
        .timeout(VERIFY_TIMEOUT)
        .send_json(serde_json::json!({ "sha": sha }))
    {
        Ok(_) => gitlab_mr(data_dir, host, project, iid),
        Err(ureq::Error::Status(401, _)) => {
            Err("GitLab rejected the token (401); reconnect in Settings".into())
        }
        Err(ureq::Error::Status(403, _)) => Err(
            "the token can't merge (403); it needs the api scope, and you need merge rights".into(),
        ),
        Err(ureq::Error::Status(405 | 406 | 422, _)) => {
            Err("GitLab won't merge it right now; open it to see why".into())
        }
        Err(ureq::Error::Status(409, _)) => {
            Err("the branch changed since the check; nothing was merged".into())
        }
        Err(ureq::Error::Status(code, _)) => Err(format!("{host} answered {code}")),
        Err(err) => Err(format!("couldn't reach {host}: {err}")),
    }
}

/// Why `mr` (GitLab's MR JSON) can't be merged now, if anything.
pub(crate) fn merge_blocker(mr: &serde_json::Value) -> Option<String> {
    let iid = mr["iid"].as_u64().unwrap_or_default();
    let state = mr["state"].as_str().unwrap_or_default();
    let pipeline = mr["head_pipeline"]["status"]
        .as_str()
        .or(mr["pipeline"]["status"].as_str());
    let status = mr["detailed_merge_status"].as_str().unwrap_or_default();
    if state != "opened" {
        Some(format!("!{iid} is {state}"))
    } else if mr["draft"].as_bool().unwrap_or(false) {
        Some(format!("!{iid} is a draft"))
    } else if mr["has_conflicts"].as_bool().unwrap_or(false) {
        Some(format!("!{iid} has conflicts"))
    } else if let Some(pipeline) = pipeline.filter(|p| *p != "success") {
        Some(format!("the pipeline is {pipeline}"))
    } else if status != "mergeable" {
        Some(match status {
            "not_approved" => "it still needs approval".to_string(),
            "discussions_not_resolved" => "it has unresolved threads".to_string(),
            "need_rebase" => "it needs a rebase".to_string(),
            other => format!(
                "GitLab says it can't merge yet ({})",
                other.replace('_', " ")
            ),
        })
    } else {
        None
    }
}

/// A path as one URL segment: `group/sub/proj` → `group%2Fsub%2Fproj`.
fn encode_segment(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn issue_rows(rows: &serde_json::Value) -> Vec<serde_json::Value> {
    rows.as_array()
        .map(|rows| {
            rows.iter()
                .map(|i| {
                    serde_json::json!({
                        "iid": i["iid"],
                        "title": i["title"],
                        "state": i["state"],
                        "labels": i["labels"],
                        "author": i["author"]["username"],
                        "assignees": i["assignees"].as_array().map(|a| a.iter().map(|u| u["username"].clone()).collect::<Vec<_>>()),
                        "comments": i["user_notes_count"],
                        "updated_at": i["updated_at"],
                        "web_url": i["web_url"],
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn list(data_dir: &Path) -> Vec<Connection> {
    std::fs::read(file(data_dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Remember `connection` (replacing one for the same host) and its token.
pub fn save(data_dir: &Path, connection: Connection, token: &str) -> Result<(), String> {
    store_token(
        data_dir,
        &account(&connection.kind, &connection.host),
        token,
    )?;
    let mut all = list(data_dir);
    all.retain(|c| !(c.kind == connection.kind && c.host == connection.host));
    all.push(connection);
    write(data_dir, &all)
}

/// Forget a connection and delete its token.
pub fn remove(data_dir: &Path, kind: &str, host: &str) -> Result<(), String> {
    delete_token(data_dir, &account(kind, host));
    let mut all = list(data_dir);
    all.retain(|c| !(c.kind == kind && c.host == host));
    write(data_dir, &all)
}

fn file(data_dir: &Path) -> PathBuf {
    data_dir.join("connections.json")
}

fn write(data_dir: &Path, all: &[Connection]) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
    std::fs::write(file(data_dir), bytes).map_err(|e| e.to_string())
}

pub(crate) fn account(kind: &str, host: &str) -> String {
    format!("{kind}:{host}")
}

fn token_file(data_dir: &Path, account: &str) -> PathBuf {
    let safe: String = account
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    data_dir.join("connections").join(format!("{safe}.token"))
}

fn store_token(data_dir: &Path, account: &str, token: &str) -> Result<(), String> {
    let path = token_file(data_dir, account);
    let dir = path.parent().expect("token dir");
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        // Created 0600, so the token is never readable by others, not even briefly.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| e.to_string())?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        file.write_all(token.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, token).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn load_token(data_dir: &Path, account: &str) -> Option<String> {
    std::fs::read_to_string(token_file(data_dir, account)).ok()
}

fn delete_token(data_dir: &Path, account: &str) {
    let _ = std::fs::remove_file(token_file(data_dir, account));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_is_blocked_unless_everything_is_green() {
        let ready = serde_json::json!({ "iid": 42, "state": "opened", "draft": false, "has_conflicts": false,
            "head_pipeline": { "status": "success" }, "detailed_merge_status": "mergeable" });
        assert_eq!(merge_blocker(&ready), None);
        let with = |key: &str, value: serde_json::Value| {
            let mut mr = ready.clone();
            mr[key] = value;
            merge_blocker(&mr)
        };
        assert_eq!(
            with("state", "merged".into()).as_deref(),
            Some("!42 is merged")
        );
        assert_eq!(
            with("draft", true.into()).as_deref(),
            Some("!42 is a draft")
        );
        assert_eq!(
            with("has_conflicts", true.into()).as_deref(),
            Some("!42 has conflicts")
        );
        assert_eq!(
            with("head_pipeline", serde_json::json!({ "status": "running" })).as_deref(),
            Some("the pipeline is running")
        );
        assert_eq!(
            with("detailed_merge_status", "not_approved".into()).as_deref(),
            Some("it still needs approval")
        );
        let no_pipeline = {
            let mut mr = ready.clone();
            mr["head_pipeline"] = serde_json::Value::Null;
            mr
        };
        assert_eq!(merge_blocker(&no_pipeline), None);
    }

    #[test]
    fn project_paths_encode_as_one_segment() {
        assert_eq!(encode_segment("group/sub/my.proj"), "group%2Fsub%2Fmy.proj");
        assert_eq!(encode_segment("42"), "42");
    }

    #[test]
    fn hosts_normalize_to_an_origin() {
        assert_eq!(normalize_host("gitlab.com").unwrap(), "https://gitlab.com");
        assert_eq!(
            normalize_host(" https://GitLab.Example.com/group/proj ").unwrap(),
            "https://gitlab.example.com"
        );
        assert_eq!(
            normalize_host("http://gitlab.local:8080/").unwrap(),
            "http://gitlab.local:8080"
        );
        assert!(normalize_host("ftp://x").is_err());
        assert!(normalize_host("").is_err());
        assert!(normalize_host("user@host").is_err());
    }

    #[test]
    fn save_replaces_by_host_and_remove_forgets() {
        // SAFETY: tests here run in one process; nothing else reads it concurrently.
        unsafe { std::env::set_var("KEEL_KEYCHAIN_DISABLE", "1") };
        let dir = std::env::temp_dir().join(format!("keel-conn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let conn = |user: &str| Connection {
            kind: "gitlab".into(),
            host: "https://gitlab.com".into(),
            username: user.into(),
            name: user.into(),
        };
        save(&dir, conn("a"), "t1").unwrap();
        save(&dir, conn("b"), "t2").unwrap();
        assert_eq!(list(&dir), vec![conn("b")]);
        assert_eq!(
            load_token(&dir, "gitlab:https://gitlab.com").as_deref(),
            Some("t2")
        );
        // A host that isn't connected is never asked.
        assert!(
            gitlab_get(&dir, "https://evil.example", "user")
                .unwrap_err()
                .contains("isn't connected")
        );
        remove(&dir, "gitlab", "https://gitlab.com").unwrap();
        assert!(list(&dir).is_empty());
        assert!(!token_file(&dir, "gitlab:https://gitlab.com").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_stats_and_changes_count_parse_cleanly() {
        let diffs = vec![
            serde_json::json!({
                "new_path": "a.ts",
                "diff": "--- a/a.ts\n+++ b/a.ts\n@@ -1,3 +1,4 @@\n line\n-old\n+new1\n+new2\n"
            }),
            serde_json::json!({
                "new_path": "b.ts",
                "diff": "--- a/b.ts\n+++ b/b.ts\n@@ -1,2 +1,1 @@\n-gone\n keep\n"
            }),
        ];
        assert_eq!(count_diff_stats(&diffs), (2, 2, 2));
        assert_eq!(parse_changes_count(&serde_json::json!("14")), Some(14));
        assert_eq!(parse_changes_count(&serde_json::json!("1000+")), Some(1000));
        assert_eq!(parse_changes_count(&serde_json::json!(7)), Some(7));
        assert_eq!(parse_changes_count(&serde_json::Value::Null), None);
    }

    #[test]
    fn glab_cli_hosts_and_tokens_parse() {
        let yaml = r#"
host: gitlab.com
hosts:
    gitlab.com:
        api_protocol: https
        token: ""
    gitlab.dot.co.id:
        api_protocol: https
        user: arndev
        token: glpat-test-123
    local.gitlab.test:
        api_protocol: http
        token: 'glpat-local'
last_seen_version: v1.108.0
"#;
        let parsed = parse_glab_hosts(yaml);
        assert_eq!(
            parsed,
            vec![
                ("https://gitlab.dot.co.id".into(), "glpat-test-123".into()),
                ("http://local.gitlab.test".into(), "glpat-local".into()),
            ]
        );
    }
}
