//! Dotify: the workplace clock-in / clock-out service, behind the session
//! timer pill. One connection at a time, kept like GitLab's (host and user in
//! `connections.json`, the token in a 0600 file). The token is only ever
//! sent to the Dotify base URL it was saved for, and never logged.
//! Wire types in `keel_proto::dotify`.

use std::path::Path;
use std::time::Duration;

use keel_proto::dotify::{self as wire, DotifyError, Entry, Project, Status, Summary};
use serde_json::{Value, json};

use crate::connections::{self, Connection};

const TIMEOUT: Duration = Duration::from_secs(15);
pub const KIND: &str = "dotify";

pub struct Client {
    base: String,
    token: String,
}

impl Client {
    pub fn new(base: &str, token: &str) -> Self {
        Client {
            base: base.trim_end_matches('/').to_string(),
            token: wire::normalize_token(token),
        }
    }

    /// One call; `Ok` is the envelope's `data`.
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, DotifyError> {
        let url = format!("{}/{}", self.base, path.trim_start_matches('/'));
        let request = ureq::request(method, &url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json")
            .set("User-Agent", "Keel")
            .timeout(TIMEOUT);
        let result = match body {
            Some(body) => request
                .set("Content-Type", "application/json")
                .send_json(body.clone()),
            None => request.call(),
        };
        match result {
            Ok(ok) => {
                let text = ok
                    .into_string()
                    .map_err(|e| DotifyError::Server(format!("unreadable answer: {e}")))?;
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                let body: Value = serde_json::from_str(&text).map_err(|_| {
                    DotifyError::Server("Dotify answered something that isn't JSON".into())
                })?;
                Ok(wire::envelope_data(&body).clone())
            }
            Err(ureq::Error::Status(code, response)) => {
                let body: Value = response
                    .into_string()
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or(Value::Null);
                Err(DotifyError::from_status(code, &body))
            }
            Err(ureq::Error::Transport(t)) => {
                Err(DotifyError::Offline(scrub(&t.to_string(), &self.token)))
            }
        }
    }

    pub fn status(&self) -> Result<Status, DotifyError> {
        self.call("GET", "tracker/check-status", None)
            .map(|d| Status::parse(&d))
    }

    pub fn summary(&self) -> Result<Summary, DotifyError> {
        self.call("GET", "time-entities/summary", None)
            .map(|d| Summary::parse(&d))
    }

    pub fn entries(&self) -> Result<Vec<Entry>, DotifyError> {
        self.call("GET", "time-entities", None)
            .map(|d| wire::parse_entries(&d))
    }

    pub fn projects(&self, search: Option<&str>) -> Result<Vec<Project>, DotifyError> {
        let path = match search.map(str::trim).filter(|s| !s.is_empty()) {
            Some(q) => format!("projects?search={}", encode_query(q)),
            None => "projects".to_string(),
        };
        self.call("GET", &path, None)
            .map(|d| wire::parse_projects(&d))
    }

    pub fn favorites(&self) -> Result<Vec<Project>, DotifyError> {
        self.call("GET", "projects/favorite", None)
            .map(|d| wire::parse_projects(&d))
    }

    /// Star (`POST`) or unstar (`DELETE`) `/projects/:id/favorite`.
    pub fn favorite(&self, project_id: i64, on: bool) -> Result<(), DotifyError> {
        self.call(
            if on { "POST" } else { "DELETE" },
            &format!("projects/{project_id}/favorite"),
            None,
        )
        .map(|_| ())
    }

    pub fn clock_in(&self, project_id: i64, description: &str) -> Result<Entry, DotifyError> {
        let data = self.call(
            "POST",
            "tracker/clock-in",
            Some(&json!({ "projectId": project_id, "description": description })),
        )?;
        Entry::parse(&data)
            .ok_or_else(|| DotifyError::Server("clock-in answered without an entry".into()))
    }

    pub fn clock_out(&self) -> Result<Entry, DotifyError> {
        let data = self.call("POST", "tracker/clock-out", None)?;
        Entry::parse(&data)
            .ok_or_else(|| DotifyError::Server("clock-out answered without an entry".into()))
    }

    /// `PUT /time-entities/:id`. Keel only edits stopped entries, and sends
    /// their end unchanged (`startDate` can't be changed at all).
    pub fn update_entry(
        &self,
        id: i64,
        project_id: i64,
        description: &str,
        end_ms: i64,
    ) -> Result<Entry, DotifyError> {
        let body = json!({ "projectId": project_id, "description": description, "endDate": wire::format_utc(end_ms) });
        let data = self.call("PUT", &format!("time-entities/{id}"), Some(&body))?;
        Entry::parse(&data)
            .ok_or_else(|| DotifyError::Server("update answered without an entry".into()))
    }
}

/// Never let the token reach a message.
fn scrub(text: &str, token: &str) -> String {
    if token.len() >= 8 {
        text.replace(token, "<token>")
    } else {
        text.to_string()
    }
}

fn encode_query(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The stored Dotify connection, if any.
pub fn connection(data_dir: &Path) -> Option<Connection> {
    connections::list(data_dir)
        .into_iter()
        .find(|c| c.kind == KIND)
}

/// A client for the stored connection.
pub fn client(data_dir: &Path) -> Result<Client, DotifyError> {
    let conn =
        connection(data_dir).ok_or_else(|| DotifyError::Auth("Dotify isn't connected".into()))?;
    // The token stays filed under the host it was saved with (so one saved
    // against the retired development API is still found); calls go to
    // where that host resolves now: production.
    let token = connections::load_token(data_dir, &connections::account(KIND, &conn.host))
        .ok_or_else(|| {
            DotifyError::Auth("the Dotify token is missing; connect again in Settings".into())
        })?;
    Ok(Client::new(wire::resolve_base(&conn.host), &token))
}

/// Check a token (a check-status that answers 200) and keep it, replacing
/// any earlier Dotify connection. Name and email come from the token.
pub fn connect(data_dir: &Path, base: &str, raw_token: &str) -> Result<Connection, DotifyError> {
    let token = wire::normalize_token(raw_token);
    if token.is_empty() {
        return Err(DotifyError::Rejected("paste your Dotify token".into()));
    }
    let client = Client::new(base, &token);
    client.status()?;
    let (name, email) =
        wire::token_identity(&token).unwrap_or_else(|| ("Dotify user".into(), String::new()));
    let conn = Connection {
        kind: KIND.into(),
        host: client.base.clone(),
        username: email,
        name,
    };
    for old in connections::list(data_dir)
        .into_iter()
        .filter(|c| c.kind == KIND && c.host != conn.host)
    {
        let _ = connections::remove(data_dir, KIND, &old.host);
    }
    connections::save(data_dir, conn.clone(), &token).map_err(DotifyError::Server)?;
    Ok(conn)
}

/// One `dotify` RPC: `op` and its params → the reply body.
pub fn run_op(data_dir: &Path, params: &Value) -> Result<Value, DotifyError> {
    let op = params["op"].as_str().unwrap_or_default();
    let c = client(data_dir)?;
    let entry = |e: Entry| json!({ "entry": e });
    match op {
        "status" => c.status().map(|s| json!(s)),
        "summary" => c.summary().map(|s| json!(s)),
        "entries" => c.entries().map(|e| json!({ "entries": e })),
        "projects" => c
            .projects(params["search"].as_str())
            .map(|p| json!({ "projects": p })),
        "favorites" => c.favorites().map(|p| json!({ "projects": p })),
        "clock_in" => {
            let project = params["project_id"]
                .as_i64()
                .ok_or_else(|| DotifyError::Rejected("pick a project".into()))?;
            c.clock_in(project, params["description"].as_str().unwrap_or_default())
                .map(entry)
        }
        "clock_out" => c.clock_out().map(entry),
        "favorite" => {
            let project = params["project_id"]
                .as_i64()
                .ok_or_else(|| DotifyError::Rejected("project_id is required".into()))?;
            let on = params["on"].as_bool().unwrap_or(true);
            c.favorite(project, on)
                .map(|_| json!({ "project_id": project, "on": on }))
        }
        "update_entry" => {
            let (Some(id), Some(project), Some(end)) = (
                params["id"].as_i64(),
                params["project_id"].as_i64(),
                params["end_ms"].as_i64(),
            ) else {
                return Err(DotifyError::Rejected(
                    "id, project_id and end_ms are required".into(),
                ));
            };
            c.update_entry(
                id,
                project,
                params["description"].as_str().unwrap_or_default(),
                end,
            )
            .map(entry)
        }
        other => Err(DotifyError::Rejected(format!("unknown op {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A request the mock saw.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        path: String,
        auth: String,
        body: String,
    }

    /// A one-thread HTTP server answering each request from `answer`.
    fn mock(
        answer: impl Fn(&str, &str) -> (u16, String) + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let (method, path) = (
                    parts.next().unwrap_or("").to_string(),
                    parts.next().unwrap_or("").to_string(),
                );
                let (mut len, mut auth) = (0usize, String::new());
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    let lower = h.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    if lower.starts_with("authorization:") {
                        auth = h["authorization:".len()..].trim().to_string();
                    }
                }
                let mut body = vec![0; len];
                let _ = reader.read_exact(&mut body);
                let path = path.trim_start_matches("/api/v1/").to_string();
                let (code, text) = answer(&method, &path);
                log.lock().unwrap().push(Seen {
                    method,
                    path,
                    auth,
                    body: String::from_utf8_lossy(&body).into(),
                });
                let _ = write!(
                    stream,
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        (base, seen)
    }

    const RUNNING: &str = r#"{"message":"Check status successfully","data":{"is_start_timer":true,"is_over_time":true,
        "time_entity":{"id":191356,"description":"Task","project":{"id":222,"name":"DLTX Solutions"},
        "start_date":"2026-10-08T02:06:41.000Z","end_date":null}}}"#;
    const CLOCKED_IN: &str = r#"{"message":"Start timer successfully","data":{"id":191356,"description":"feat(x): y","duration":0,
        "start_date":"2026-10-08T03:15:00.000Z","end_date":null,"project":{"id":222,"name":"DLTX Solutions"}}}"#;
    const STOPPED: &str = r#"{"message":"Stop timer successfully","data":{"id":191357,"description":"feat","duration":4.25,
        "start_date":"2026-10-08T03:15:00.000Z","end_date":"2026-10-08T07:30:00.000Z","project":{"id":222,"name":"DLTX Solutions"}}}"#;

    #[test]
    fn reads_parse_through_the_envelope_with_a_clean_bearer() {
        let (base, seen) = mock(|_, path| {
            match path {
            "tracker/check-status" => (200, RUNNING.into()),
            "time-entities/summary" => (200, r#"{"message":"ok","data":{"employee_week_hours":42.62,"this_week_normal_hours":40}}"#.into()),
            p if p.starts_with("projects?search=") => (200, r#"{"message":"ok","data":[{"id":222,"name":"DLTX Solutions"}]}"#.into()),
            "projects/favorite" => (200, r#"{"message":"ok","data":[{"id":200,"name":"Dotify 2026"}]}"#.into()),
            _ => (404, r#"{"message":"Not Found","data":null}"#.into()),
        }
        });
        let c = Client::new(&base, "Bearer tok.en.x");
        let status = c.status().unwrap();
        assert!(status.overtime);
        assert_eq!(
            status.running.unwrap().project.unwrap().name,
            "DLTX Solutions"
        );
        assert_eq!(c.summary().unwrap().week_h, 42.62);
        assert_eq!(c.projects(Some("dl tx")).unwrap()[0].id, 222);
        assert_eq!(c.favorites().unwrap()[0].name, "Dotify 2026");
        let seen = seen.lock().unwrap();
        assert!(seen.iter().all(|s| s.auth == "Bearer tok.en.x"), "{seen:?}");
        assert!(seen.iter().any(|s| s.path == "projects?search=dl%20tx"));
    }

    #[test]
    fn mutations_send_the_documented_bodies() {
        let (base, seen) = mock(|method, path| match (method, path) {
            ("POST", "tracker/clock-in") => (201, CLOCKED_IN.into()),
            ("POST", "tracker/clock-out") => (200, STOPPED.into()),
            ("PUT", "time-entities/191357") => (200, STOPPED.into()),
            _ => (404, "{}".into()),
        });
        let c = Client::new(&base, "tok");
        let started = c.clock_in(222, "feat(x): y").unwrap();
        assert_eq!(started.id, 191356);
        let stopped = c.clock_out().unwrap();
        assert_eq!(
            (stopped.duration_h, wire::hours_label(stopped.duration_h)),
            (4.25, "4h 15m".to_string())
        );
        c.update_entry(191357, 222, "better", stopped.end_ms.unwrap())
            .unwrap();
        let seen = seen.lock().unwrap();
        let body = |i: usize| serde_json::from_str::<Value>(&seen[i].body).unwrap_or(Value::Null);
        assert_eq!(
            body(0),
            json!({"projectId": 222, "description": "feat(x): y"})
        );
        assert_eq!(seen[1].body, "");
        assert_eq!(
            body(2),
            json!({"projectId": 222, "description": "better", "endDate": "2026-10-08T07:30:00.000Z"})
        );
    }

    #[test]
    fn favorite_toggle_posts_and_deletes() {
        let (base, seen) = mock(|method, path| match (method, path) {
            ("POST", "projects/222/favorite") => {
                (201, r#"{"message":"Project favorited","data":null}"#.into())
            }
            ("DELETE", "projects/222/favorite") => (
                200,
                r#"{"message":"Project unfavorited","data":null}"#.into(),
            ),
            ("POST", "projects/9/favorite") => {
                (404, r#"{"message":"Project not found","data":null}"#.into())
            }
            _ => (500, "{}".into()),
        });
        let c = Client::new(&base, "tok");
        c.favorite(222, true).unwrap();
        c.favorite(222, false).unwrap();
        assert_eq!(
            c.favorite(9, true).unwrap_err(),
            DotifyError::Rejected("Project not found".into())
        );
        let seen = seen.lock().unwrap();
        let calls: Vec<(String, String)> = seen
            .iter()
            .map(|s| (s.method.clone(), s.path.clone()))
            .collect();
        assert_eq!(
            calls[0],
            ("POST".to_string(), "projects/222/favorite".to_string())
        );
        assert_eq!(
            calls[1],
            ("DELETE".to_string(), "projects/222/favorite".to_string())
        );
        assert!(
            seen.iter()
                .all(|s| s.body.is_empty() && s.auth == "Bearer tok")
        );
    }

    #[test]
    fn errors_are_told_apart() {
        let (base, _) = mock(|_, path| {
            match path {
            "tracker/check-status" => (401, r#"{"message":"Unauthorized","statusCode":401}"#.into()),
            "time-entities/1" => (400, r#"{"message":"Cannot edit time entity from a confirmed attendance period","data":null}"#.into()),
            _ => (500, "oops".into()),
        }
        });
        let c = Client::new(&base, "tok");
        assert_eq!(
            c.status().unwrap_err(),
            DotifyError::Auth("Unauthorized".into())
        );
        assert_eq!(
            c.update_entry(1, 2, "d", 0).unwrap_err(),
            DotifyError::Rejected(
                "Cannot edit time entity from a confirmed attendance period".into()
            )
        );
        assert!(matches!(c.summary().unwrap_err(), DotifyError::Server(_)));
        // Nothing listening: offline, and the token never shows up.
        let dead = Client::new("http://127.0.0.1:9/api/v1", "secret-token-123");
        let err = dead.status().unwrap_err();
        assert!(matches!(err, DotifyError::Offline(_)), "{err:?}");
        assert!(!err.text().contains("secret-token-123"));
    }

    #[test]
    fn connect_verifies_then_keeps_one_connection() {
        // SAFETY: tests in this process share the env; nothing reads it concurrently here.
        unsafe { std::env::set_var("KEEL_KEYCHAIN_DISABLE", "1") };
        let dir = std::env::temp_dir().join(format!("keel-dotify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (base, _) = mock(|_, _| (200, RUNNING.into()));
        // {"name":"Ada L","email":"ada@example.com","exp":2000000000}
        let token = "Bearer x.eyJuYW1lIjoiQWRhIEwiLCJlbWFpbCI6ImFkYUBleGFtcGxlLmNvbSIsImV4cCI6MjAwMDAwMDAwMH0.s";
        let conn = connect(&dir, &base, token).unwrap();
        assert_eq!(
            (conn.name.as_str(), conn.username.as_str()),
            ("Ada L", "ada@example.com")
        );
        assert_eq!(connection(&dir).unwrap().host, base);
        assert!(run_op(&dir, &json!({"op": "status"})).unwrap()["running"]["id"] == 191356);
        let (bad, _) = mock(|_, _| (401, r#"{"message":"Unauthorized"}"#.into()));
        assert!(matches!(
            connect(&dir, &bad, "nope"),
            Err(DotifyError::Auth(_))
        ));
        // The failed connect left the good one alone.
        assert_eq!(connection(&dir).unwrap().host, base);
        connections::remove(&dir, KIND, &base).unwrap();
        assert!(matches!(
            run_op(&dir, &json!({"op": "status"})),
            Err(DotifyError::Auth(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_connection_saved_against_development_calls_production_with_its_token() {
        // SAFETY: tests in this process share the env; nothing reads it concurrently here.
        unsafe { std::env::set_var("KEEL_KEYCHAIN_DISABLE", "1") };
        let dir = std::env::temp_dir().join(format!("keel-dotify-dev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dev = "https://api.dotify-dev.dot.co.id/api/v1";
        let conn = Connection {
            kind: KIND.into(),
            host: dev.into(),
            username: "ada@example.com".into(),
            name: "Ada L".into(),
        };
        connections::save(&dir, conn, "test-token-value").unwrap();
        // Still found under the old host, and sent to production (no call made).
        let c = client(&dir).unwrap();
        assert_eq!(c.base, wire::PROD_BASE);
        assert_eq!(c.token, "test-token-value");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live, read-only: `KEEL_DOTIFY_LIVE=1 cargo test -p keel-engine live_dotify -- --ignored --nocapture`
    /// reads the token Keel stored and calls only GET endpoints.
    #[test]
    #[ignore]
    fn live_dotify_read_only() {
        let data_dir =
            std::path::PathBuf::from(std::env::var("KEEL_DOTIFY_DATA").expect("KEEL_DOTIFY_DATA"));
        let c = client(&data_dir).expect("connected");
        let status = c.status().expect("status");
        println!(
            "check-status: running={} overtime={}",
            status.running.is_some(),
            status.overtime
        );
        let s = c.summary().expect("summary");
        println!(
            "summary: week {:.2}/{:.0}h period {:.2}/{:.0}h",
            s.week_h, s.week_normal_h, s.period_h, s.period_normal_h
        );
        println!("favorites: {}", c.favorites().expect("favorites").len());
        println!("projects: {}", c.projects(None).expect("projects").len());
        println!(
            "projects?search=dot: {}",
            c.projects(Some("dot")).expect("search").len()
        );
        let entries = c.entries().expect("entries");
        println!(
            "time-entities: {} entries, {} locked",
            entries.len(),
            entries.iter().filter(|e| e.locked).count()
        );
    }
}
