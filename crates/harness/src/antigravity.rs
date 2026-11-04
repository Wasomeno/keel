//! Antigravity in Settings → Accounts, beside Pi's own providers.
//!
//! The bridge (see [`crate::keel::antigravity_env`]) drives Google's ACP
//! server, which keeps its own sign-in: `auth.type` in
//! `~/.gemini/antigravity-acp/settings.json`, then either a token file there
//! or the bridge's verified marker, `acp-auth-verified.json`, for a server
//! that signs in from older `~/.gemini` credentials and writes no token file.
//! Without one of the two the bridge warns "Run /agy auth" on every start.
//!
//! [`sign_in`] does what the bridge's `/agy auth` does: start the server,
//! send ACP `authenticate` with `oauth-personal`, and when no token file
//! lands, prove the credentials with a `session/new`. Then it writes the
//! marker. The server opens its Google page through `BROWSER`, which points
//! at a script that only records the URL, so Keel opens it like the others.
//! Nothing here reads a credential: the token file is only checked for, and
//! `settings.json` is read for `auth.type` alone.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{AuthAccount, HarnessError, SignInEvent};

/// The row's id, and the model prefix the bridge registers.
pub(crate) const PROVIDER: &str = "antigravity";
const NAME: &str = "Antigravity";
const HINT: &str = "Google AI plan, through Antigravity";

/// The bridge's `/agy auth` waits as long for the browser round trip.
const LIMIT: Duration = Duration::from_secs(10 * 60);
/// How long a token file may lag `authenticate`'s reply.
const TOKEN_GRACE: Duration = Duration::from_secs(5);

/// Where the server and the bridge keep their sign-in state.
pub(crate) struct Paths {
    /// `~/.gemini/antigravity-acp`: `settings.json` and `acp_token.json`.
    gemini: PathBuf,
    /// The bridge's `defaultAcpAuthVerifiedPath()`.
    marker: PathBuf,
}

impl Paths {
    pub(crate) fn under(home: &Path) -> Self {
        Self {
            gemini: home.join(".gemini/antigravity-acp"),
            marker: home.join(".pi/agent/antigravity-bridge/acp-auth-verified.json"),
        }
    }

    pub(crate) fn of_home() -> Option<Self> {
        std::env::var_os("HOME").filter(|home| !home.is_empty()).map(|home| Self::under(Path::new(&home)))
    }

    fn settings(&self) -> PathBuf {
        self.gemini.join("settings.json")
    }

    /// Checked for, never opened: it holds the credential.
    fn has_token(&self) -> bool {
        self.gemini.join("acp_token.json").exists()
    }

    fn verified(&self) -> bool {
        std::fs::read_to_string(&self.marker)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|v| v["verified"] == true)
    }

    /// `auth.type` from `settings.json`, the only field read there.
    fn auth_type(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.settings()).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        value["auth"]["type"].as_str().map(str::to_string)
    }
}

/// The Accounts row, by the bridge's own test (`ensureAcpReady`): an API key
/// needs no login; otherwise a token file or the verified marker. `detail`
/// is `oauth` for the subscription, so Settings says "Subscription".
pub(crate) fn account(paths: &Paths) -> AuthAccount {
    let kind = paths.auth_type();
    let token = paths.has_token();
    let detail = match kind.as_deref() {
        Some("gemini-api-key") => Some("api_key"),
        None if !token => None,
        _ => (token || paths.verified()).then_some("oauth"),
    };
    AuthAccount {
        id: PROVIDER.to_string(),
        name: NAME.to_string(),
        hint: HINT.to_string(),
        signed_in: detail.is_some(),
        detail: detail.unwrap_or("not signed in").to_string(),
    }
}

/// Sign the ACP server in with the Google account's plan and record it for
/// the bridge. `scratch` holds the browser script and the URL it records.
pub(crate) async fn sign_in(
    bin: &Path,
    paths: &Paths,
    scratch: &Path,
    on: &(dyn Fn(SignInEvent) + Send + Sync),
) -> Result<(), HarnessError> {
    tokio::time::timeout(LIMIT, run(bin, paths, scratch, on))
        .await
        .unwrap_or_else(|_| Err(failed("sign-in timed out")))
}

async fn run(
    bin: &Path,
    paths: &Paths,
    scratch: &Path,
    on: &(dyn Fn(SignInEvent) + Send + Sync),
) -> Result<(), HarnessError> {
    // The server reads its auth type at startup.
    if paths.auth_type().is_none() {
        set_auth_type(&paths.settings()).map_err(io)?;
    }
    on(SignInEvent::Progress("Starting Antigravity".into()));
    std::fs::create_dir_all(scratch).map_err(io)?;
    let urls = scratch.join("antigravity-sign-in.url");
    let _ = std::fs::remove_file(&urls);
    let browser = browser_script(scratch, &urls).map_err(io)?;

    let mut cmd = tokio::process::Command::new(bin);
    cmd.env("BROWSER", &browser)
        .current_dir(scratch)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(io)?;
    let stdin = child.stdin.take().ok_or_else(|| failed("the server has no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| failed("the server has no stdout"))?;
    let mut rpc = Rpc { stdin, lines: BufReader::new(stdout).lines(), next: 0 };

    let mut shown = String::new();
    let mut watch = || {
        let Some(url) = std::fs::read_to_string(&urls)
            .ok()
            .and_then(|text| text.lines().rev().map(str::trim).find(|l| l.starts_with("https://")).map(str::to_string))
        else {
            return;
        };
        if url != shown {
            on(SignInEvent::Browser { url: url.clone() });
            shown = url;
        }
    };
    let out = async {
        let caps = json!({"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false});
        rpc.request("initialize", json!({"protocolVersion": 1, "clientCapabilities": caps}), &mut watch).await?;
        rpc.request("authenticate", json!({"methodId": "oauth-personal"}), &mut watch).await?;
        let deadline = tokio::time::Instant::now() + TOKEN_GRACE;
        while !paths.has_token() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if !paths.has_token() {
            // Signed in from older credentials, or not at all: a session tells.
            let cwd = scratch.to_string_lossy();
            rpc.request("session/new", json!({"cwd": cwd, "mcpServers": []}), &mut watch)
                .await
                .map_err(|_| failed("Antigravity has no usable sign-in yet. Finish it in your browser, then try again"))?;
        }
        write_marker(&paths.marker).map_err(io)
    }
    .await;
    let _ = child.kill().await;
    let _ = std::fs::remove_file(&urls);
    out
}

/// Newline-delimited JSON-RPC to the ACP server.
struct Rpc {
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next: u64,
}

impl Rpc {
    /// Send `method` and wait for its answer, calling `tick` every 300 ms
    /// meanwhile. Requests from the server are refused: sign-in needs none.
    async fn request(&mut self, method: &str, params: Value, tick: &mut (dyn FnMut() + Send)) -> Result<Value, HarnessError> {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await?;
        let mut every = tokio::time::interval(Duration::from_millis(300));
        loop {
            let line = tokio::select! {
                line = self.lines.next_line() => line.map_err(io)?,
                _ = every.tick() => { tick(); continue; }
            };
            let Some(line) = line else {
                return Err(failed("the Antigravity server stopped"));
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if msg.get("method").is_some() {
                if let Some(their) = msg.get("id") {
                    let refuse = json!({"jsonrpc": "2.0", "id": their, "error": {"code": -32601, "message": "not supported"}});
                    self.send(&refuse).await?;
                }
                continue;
            }
            if msg["id"] != json!(id) {
                continue;
            }
            tick();
            if let Some(error) = msg.get("error") {
                let why = error["message"].as_str().unwrap_or("the server refused");
                return Err(failed(&format!("{method}: {why}")));
            }
            return Ok(msg["result"].clone());
        }
    }

    async fn send(&mut self, msg: &Value) -> Result<(), HarnessError> {
        let mut line = msg.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.map_err(io)?;
        self.stdin.flush().await.map_err(io)
    }
}

/// `{"auth":{"type":"oauth-personal"}}`, keeping whatever else is there.
fn set_auth_type(file: &Path) -> std::io::Result<()> {
    let mut settings: Value = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if !settings["auth"].is_object() {
        settings["auth"] = json!({});
    }
    settings["auth"]["type"] = json!("oauth-personal");
    write_private(file, &format!("{}\n", serde_json::to_string_pretty(&settings).unwrap_or_default()))
}

/// The bridge's verified marker: a flag and a time, no credential.
fn write_marker(file: &Path) -> std::io::Result<()> {
    let at = iso_now();
    write_private(file, &format!("{}\n", serde_json::to_string_pretty(&json!({"verified": true, "at": at})).unwrap_or_default()))
}

/// Owner-only, like the bridge writes it.
fn write_private(file: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(file)?.write_all(text.as_bytes())
}

/// A `BROWSER` that records the URL and opens nothing.
fn browser_script(dir: &Path, urls: &Path) -> std::io::Result<PathBuf> {
    let path = dir.join("antigravity-browser.sh");
    let quoted = urls.to_string_lossy().replace('\'', r"'\''");
    std::fs::write(&path, format!("#!/bin/sh\n# Written by Keel: Keel opens the sign-in page itself.\nprintf '%s\\n' \"$@\" >> '{quoted}'\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

/// Now as `2026-11-04T10:00:00Z`.
fn iso_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

fn failed(why: &str) -> HarnessError {
    HarnessError::Failed(why.to_string())
}

fn io(err: std::io::Error) -> HarnessError {
    HarnessError::Failed(err.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::{Paths, account, iso_now, sign_in};
    use crate::SignInEvent;

    fn tmp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keel-agy-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn signed_in_takes_a_token_file_or_the_bridge_marker() {
        let home = tmp("account");
        let paths = Paths::under(&home);
        let row = || account(&paths);
        assert_eq!((row().signed_in, row().detail.as_str()), (false, "not signed in"));
        assert_eq!((row().id.as_str(), row().name.as_str()), ("antigravity", "Antigravity"));

        // The auth type alone is the state before the browser round trip.
        write(&home.join(".gemini/antigravity-acp/settings.json"), r#"{"auth":{"type":"oauth-personal"}}"#);
        assert!(!row().signed_in);

        let marker = home.join(".pi/agent/antigravity-bridge/acp-auth-verified.json");
        write(&marker, r#"{"verified":false}"#);
        assert!(!row().signed_in);
        write(&marker, r#"{"verified":true,"at":"2026-11-04T10:00:00Z"}"#);
        assert_eq!((row().signed_in, row().detail.as_str()), (true, "oauth"));

        std::fs::remove_file(&marker).unwrap();
        write(&home.join(".gemini/antigravity-acp/acp_token.json"), "{}");
        assert_eq!((row().signed_in, row().detail.as_str()), (true, "oauth"));

        write(&home.join(".gemini/antigravity-acp/settings.json"), r#"{"auth":{"type":"gemini-api-key"}}"#);
        assert_eq!((row().signed_in, row().detail.as_str()), (true, "api_key"));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A stand-in ACP server: answers `initialize` and `authenticate`,
    /// opens `BROWSER` on the way, then answers `session/new` as told.
    fn fake_server(dir: &Path, session: &str) -> PathBuf {
        let bin = dir.join("fake-acp.sh");
        let script = format!(
            "#!/bin/sh\nread l; echo '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{}}}}'\nread l; \"$BROWSER\" 'https://accounts.example/o?state=1'; sleep 1\n\
             echo '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{}}}}'\necho '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{}}}}'\nread l; echo '{session}'\nsleep 5\n"
        );
        std::fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[tokio::test]
    async fn sign_in_writes_the_marker_and_hands_over_the_page() {
        let home = tmp("sign-in");
        let paths = Paths::under(&home);
        let bin = fake_server(&home, r#"{"jsonrpc":"2.0","id":3,"result":{"sessionId":"s"}}"#);
        let events = Mutex::new(Vec::new());
        let on = |e: SignInEvent| events.lock().unwrap().push(e);
        sign_in(&bin, &paths, &home.join("scratch"), &on).await.unwrap();
        let events = events.into_inner().unwrap();
        assert!(events.contains(&SignInEvent::Browser { url: "https://accounts.example/o?state=1".into() }), "{events:?}");
        assert!(account(&paths).signed_in);
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".gemini/antigravity-acp/settings.json")).unwrap()).unwrap();
        assert_eq!(settings["auth"]["type"], "oauth-personal");
        let marker = home.join(".pi/agent/antigravity-bridge/acp-auth-verified.json");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn a_hollow_sign_in_leaves_no_marker() {
        let home = tmp("hollow");
        let paths = Paths::under(&home);
        write(&home.join(".gemini/antigravity-acp/settings.json"), r#"{"auth":{"type":"oauth-personal"},"queuedMessages":"x"}"#);
        let bin = fake_server(&home, r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32000,"message":"Authentication required"}}"#);
        let err = sign_in(&bin, &paths, &home.join("scratch"), &|_| {}).await.unwrap_err();
        assert!(err.to_string().contains("no usable sign-in"), "{err}");
        assert!(!account(&paths).signed_in);
        let settings = std::fs::read_to_string(home.join(".gemini/antigravity-acp/settings.json")).unwrap();
        assert!(settings.contains("queuedMessages"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn iso_now_is_a_utc_timestamp() {
        let at = iso_now();
        assert_eq!(at.len(), 20, "{at}");
        assert!(at.starts_with("20") && at.ends_with('Z') && at.as_bytes()[10] == b'T', "{at}");
    }
}
