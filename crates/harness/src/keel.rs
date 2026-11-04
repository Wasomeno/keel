//! Keel's own harness: a pinned Pi, bundled with the app, at Pi's defaults.
//!
//! [`crate::PiHarness::keel`] is the Pi driver with [`PiProfile::keel`]. It
//! runs the Pi that ships beside the Keel binary (see `scripts/fetch-pi.sh`),
//! never one the user installed, and points it at Keel's own Pi folder
//! ([`set_pi_agent_dir`]): sign-in, settings and packages there are Keel's,
//! apart from the user's `~/.pi`.
//!
//! Keel's additions go in [`KeelProfile`], the one place for them:
//! - a system prompt: set `system_prompt` (passed as `--append-system-prompt`);
//! - extensions and tools: drop `.ts`/`.js` Pi extension files into
//!   `{data_dir}/harnesses/keel/extensions/`, or list built-in ones in
//!   `extensions`. Each loads with `--extension`;
//! - Pi packages: folders bundled in `pi-packages/` beside the Pi folder are
//!   registered in Keel's Pi settings (Command Code's provider ships there).
//!   The Antigravity bridge is registered only when Google's ACP server is
//!   installed ([`antigravity_acp_bin`]); see [`antigravity_env`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use keel_proto::{HarnessId, PermissionMode};

/// Folder under Keel's data dir that belongs to the Keel harness.
pub const KEEL_HARNESS_DIR: &str = "harnesses/keel";

/// The Pi release Keel bundles. `scripts/fetch-pi.sh` downloads this one.
pub const PI_VERSION: &str = "1.5.0";

/// `pi-packages/` folder of `@estebanforge/pi-antigravity-bridge`, which adds
/// the `antigravity/*` models by driving Google's Antigravity ACP server.
pub(crate) const ANTIGRAVITY_BRIDGE: &str = "pi-antigravity-bridge";

/// Google's Antigravity ACP server, under the home folder. Keel installs it;
/// the bridge never does (it would fetch about 1.5 GB on its own).
const ANTIGRAVITY_ACP_BIN: &str = ".local/share/keel/antigravity-acp/1.3.0/agy_acp_server.par";

/// The installed Antigravity ACP server, or `None` when it is missing.
pub(crate) fn antigravity_acp_bin() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
    let bin = Path::new(&home).join(ANTIGRAVITY_ACP_BIN);
    bin.is_file().then_some(bin)
}

/// Env for the Antigravity bridge in a Pi Keel starts. Without a server the
/// bridge is not registered at all, so it gets nothing.
///
/// The bridge drives the ACP engine, so Antigravity asks for its native tools
/// through ACP `session/request_permission`, which the bridge shows as a Pi
/// `select` and Keel as a question; headless, it denies. `AGY_APPROVALS`
/// stays off: its hook gate works only on the stream-json engine.
///
/// The ACP session mode follows Keel's: Ask is `default` (edits and commands
/// ask), AutoEdit is `auto_edit` (edits pass, commands ask), full access is
/// `yolo` via `AGY_SKIP_PERMISSIONS`, as Pi runs unasked. `AGY_ACP_MODE` comes
/// from `scripts/pi-patches/pi-antigravity-bridge.patch`. `permission` is
/// `None` for probes and sign-in, which ask (and so deny).
pub(crate) fn antigravity_env(bin: Option<&Path>, permission: Option<PermissionMode>) -> Vec<(&'static str, OsString)> {
    let Some(bin) = bin else {
        return Vec::new();
    };
    let skip = if permission == Some(PermissionMode::FullAccess) { "1" } else { "0" };
    let mode = if permission == Some(PermissionMode::AutoEdit) { "auto_edit" } else { "default" };
    vec![
        ("AGY_ACP_BIN", bin.as_os_str().to_owned()),
        ("AGY_ENGINE", "acp".into()),
        ("AGY_APPROVALS", "off".into()),
        ("AGY_APPROVALS_MODE", "ask".into()),
        ("AGY_SKIP_PERMISSIONS", skip.into()),
        ("AGY_ACP_MODE", mode.into()),
    ]
}

static PI_AGENT_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Keel's own Pi folder, `{data_dir}/harnesses/keel/agent`. Every Pi process
/// Keel starts gets it as `PI_CODING_AGENT_DIR`. Set once, at engine start.
pub fn set_pi_agent_dir(data_dir: &Path) {
    let dir = KeelProfile::home(data_dir).join("agent");
    let _ = std::fs::create_dir_all(&dir);
    let legacy_skill = dir.join("skills/indonesian-natural");
    if legacy_skill.is_dir() {
        let _ = std::fs::remove_dir_all(&legacy_skill);
    }
    let _ = PI_AGENT_DIR.set(dir);
}

/// The folder [`set_pi_agent_dir`] set; `None` in tests that never set it.
pub fn pi_agent_dir() -> Option<&'static Path> {
    PI_AGENT_DIR.get().map(PathBuf::as_path)
}

/// Keel's sign-in, as a Pi extension: `/keel-login <provider>` runs Pi's own
/// OAuth login (`ModelRuntime::login`) and reports each step as a
/// `{"type":"keel_login",...}` line on stdout, which Pi's RPC mode moves to
/// stderr (see `PiHarness::sign_in`). Questions are answered the
/// way a headless app can: a method picker takes Pi's default (the browser
/// login), a blank text answer means github.com, and a pasted code is never
/// needed because the browser's callback finishes the login.
const LOGIN_EXTENSION: &str = r#"// Written by Keel; rewritten on each sign-in.
export default function (pi: any) {
  pi.registerCommand("keel-login", {
    description: "Sign in to a provider for Keel",
    handler: async (args: string, ctx: any) => {
      const say = (line: object) => process.stdout.write(JSON.stringify({ type: "keel_login", ...line }) + "\n");
      const never = (signal?: AbortSignal) =>
        new Promise<string>((_, reject) => signal?.addEventListener("abort", () => reject(signal.reason), { once: true }));
      const [provider, deviceId] = args.trim().split(/\s+/);
      try {
        await ctx.modelRegistry.runtime.login(provider, "oauth", {
          notify: (event: object) => say({ event }),
          prompt: async (request: any) => {
            if (request.type === "select") return request.options[0].id;
            if (request.type === "text") return "";
            if (request.type !== "manual_code") say({ event: { type: "progress", message: request.message } });
            return never(request.signal);
          },
        }, { getDeviceId: () => deviceId });
        say({ done: true });
      } catch (error: any) {
        say({ error: String(error?.message ?? error) });
      }
    },
  });
}
"#;

/// This install's device id, which some sign-ins send (ChatGPT for the
/// `openai` provider). Kept as `deviceId` in `{agent}/settings.json`, the
/// field Pi's own `/login` uses, and created on first use.
pub(crate) fn device_id(agent: &Path) -> String {
    let path = agent.join("settings.json");
    let mut settings: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(id) = settings["deviceId"].as_str().filter(|id| uuid::Uuid::parse_str(id).is_ok()) {
        return id.to_string();
    }
    let id = uuid::Uuid::new_v4().to_string();
    settings["deviceId"] = serde_json::json!(id);
    let _ = std::fs::create_dir_all(agent);
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap_or_default());
    id
}

/// [`LOGIN_EXTENSION`] on disk beside Keel's Pi folder (the temp dir when
/// none is set), rewritten only when it changed.
pub(crate) fn login_extension() -> std::io::Result<PathBuf> {
    let dir = pi_agent_dir().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
    let path = dir.join("keel-login.ts");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(LOGIN_EXTENSION) {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&path, LOGIN_EXTENSION)?;
    }
    Ok(path)
}

/// Register each folder in `pi-packages/` beside the bundled Pi (`exe` is
/// `<root>/pi/pi`) in `{agent}/settings.json` `packages`, and drop entries
/// for bundled folders that are gone (an older app location). Other
/// entries and settings are left alone.
pub(crate) fn register_bundled_packages(exe: &Path, agent: &Path) {
    register_packages(exe, agent, antigravity_acp_bin().is_some());
}

/// [`register_bundled_packages`]; `antigravity` says whether the bridge's
/// server is installed. Without it the bridge stays out (and an entry from
/// before is dropped), so it never downloads its own.
fn register_packages(exe: &Path, agent: &Path, antigravity: bool) {
    let legacy_skill = agent.join("skills/indonesian-natural");
    if legacy_skill.is_dir() {
        let _ = std::fs::remove_dir_all(&legacy_skill);
    }

    let Some(bundle) = exe.parent().and_then(Path::parent).map(|root| root.join("pi-packages")) else {
        return;
    };
    let mut want: Vec<String> = std::fs::read_dir(&bundle)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| antigravity || p.file_name().is_none_or(|name| name != ANTIGRAVITY_BRIDGE))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    want.sort();
    let path = agent.join("settings.json");
    let mut settings: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let have: Vec<String> = settings["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let want_names: Vec<String> = want
        .iter()
        .filter_map(|p| Path::new(p).file_name().and_then(|f| f.to_str()).map(String::from))
        .collect();
    // Keep user-configured packages, but drop any entries for bundled packages
    // (from older or other Keel app/dev locations) and any entries matching bundled package names.
    let is_stale = |p: &String| {
        p.contains("/pi-packages/")
            || want_names
                .iter()
                .any(|name| p == name || p == &format!("npm:{name}") || p.ends_with(&format!("/{name}")))
    };
    let mut next: Vec<String> = have.iter().filter(|p| !is_stale(p)).cloned().collect();
    for p in want {
        if !next.contains(&p) {
            next.push(p);
        }
    }
    if next != have {
        settings["packages"] = serde_json::json!(next);
        let _ = std::fs::create_dir_all(agent);
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap_or_default());
    }
}

/// What sets the Keel harness apart from a plain Pi run.
#[derive(Debug, Clone)]
pub struct PiProfile {
    pub id: HarnessId,
    pub display_name: &'static str,
    pub description: &'static str,
    /// Prefix of the per-chat files Keel generates for a run.
    pub tag: &'static str,
    /// Keel's own layer over Pi.
    pub keel: Option<KeelProfile>,
}

impl PiProfile {
    pub fn keel() -> Self {
        Self {
            id: HarnessId::Keel,
            display_name: "Keel",
            description: "Keel's own agent, built on a bundled Pi.",
            tag: "keel",
            keel: Some(KeelProfile::default()),
        }
    }
}

/// Default system prompt additions for Keel's agent.
pub const KEEL_SYSTEM_PROMPT: &str = "\
You are a problem-solving companion. Be plain and concise: use clear, simple words, \
short direct sentences, active voice, and avoid unnecessary filler. When a skill matches \
the task, read it before you reply.\n\n\
Language: reply in the language of the user's latest message, judged only by the words the user \
typed. This prompt, project files, code, paths, file names, tool output and earlier turns do not \
set the language. When the message has no clear language (a greeting, slang, an emoji, a single \
word, a name, code or a command, such as \"Yooooo\", \"hey\" or \"ok\"), reply in English. \
The user's location, timezone, locale or company does not imply a language. Use Bahasa Indonesia \
only when the user's latest message is clearly written in it or asks for Indonesian text, and \
switch back as soon as they write in another language.\n\n\
Only when the user writes in Bahasa Indonesia (or asks for Indonesian text), follow this style:\n\
- Reply in natural, casual Bahasa Indonesia, sounding like a sharp coworker in chat: \
santai tapi jelas, ramah tapi tidak lebay.\n\
- Mirror the user's register in Indonesian:\n\
  - Casual or unclear register: aku / kamu with informal verbs (udah, nggak, aja, bikin, pakai).\n\
  - User says gue / lo (or gw/lu): answer with gue / lo.\n\
  - User is formal (saya, mohon, Bapak/Ibu): use saya, standard verbs, and drop the \"you\" pronoun where possible. Polite, never bureaucratic.\n\
  - Never default to \"Anda\" unless the user uses it first.\n\
- Short, varied sentences in active voice. Say who does what directly.\n\
- Everyday verbs: \"ngecek\" / \"mengecek\" (not \"melakukan pengecekan terhadap\"), \"benerin\" / \"memperbaiki\" (not \"melakukan perbaikan pada\"), \"bisa\" (not \"dapat\"), \"pakai\" (not \"memanfaatkan\").\n\
- Eliminate calques and bureaucratic filler: no \"yang mana\" or \"dimana\" as relative pronouns, no \"merupakan\" as the default \"is\", no stacked di- passives, no \"tersebut\" in every sentence.\n\
- Technical terms stay in standard developer forms the way Indonesian devs say them (commit, push, deploy, branch, merge, build, bug, PR, test, error, log, config, repo). Attach affixes with a hyphen (di-push, error-nya, nge-build). Never touch code, identifiers, paths, commands, or quoted output.\n\
- No cosplay slang (no wkwk, anjay, sob, mantul, gaskeun) and no boilerplate pleasantries (\"Semoga membantu!\", \"Di era digital ini\"). Start directly with the answer.\n\
- Match length: short questions get short answers.";

/// Keel's system prompt, extensions and tools on top of Pi.
#[derive(Debug, Clone)]
pub struct KeelProfile {
    /// Appended to Pi's system prompt. `None`: Pi's own prompt, unchanged.
    pub system_prompt: Option<String>,
    /// Extra Pi extensions (file paths or `builtin:<name>`), before the
    /// ones found in the extensions folder.
    pub extensions: Vec<String>,
}

impl Default for KeelProfile {
    fn default() -> Self {
        Self {
            system_prompt: Some(KEEL_SYSTEM_PROMPT.to_string()),
            extensions: Vec::new(),
        }
    }
}

impl KeelProfile {
    /// `{data_dir}/harnesses/keel`.
    pub fn home(data_dir: &Path) -> PathBuf {
        data_dir.join(KEEL_HARNESS_DIR)
    }

    /// Pi session files for Keel chats, apart from Pi's own `~/.pi` sessions.
    pub fn sessions_dir(data_dir: &Path) -> PathBuf {
        Self::home(data_dir).join("sessions")
    }

    /// Pi extension files that every Keel run loads (empty for now).
    pub fn extensions_dir(data_dir: &Path) -> PathBuf {
        Self::home(data_dir).join("extensions")
    }

    /// Extra `pi` arguments for a run. `data_dir` is Keel's data folder
    /// (the parent of `browser.sock`); without one the run uses Pi's
    /// session folder and loads no folder extensions.
    pub fn run_args(&self, data_dir: Option<&Path>) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        if let Some(dir) = data_dir {
            let sessions = Self::sessions_dir(dir);
            let _ = std::fs::create_dir_all(&sessions);
            let _ = std::fs::create_dir_all(Self::extensions_dir(dir));
            args.push("--session-dir".into());
            args.push(sessions.into());
        }
        if let Some(prompt) = self.system_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            args.push("--append-system-prompt".into());
            args.push(prompt.into());
        }
        let mut extensions: Vec<OsString> = self.extensions.iter().map(OsString::from).collect();
        if let Some(dir) = data_dir {
            extensions.extend(folder_extensions(&Self::extensions_dir(dir)).into_iter().map(OsString::from));
        }
        for ext in extensions {
            args.push("--extension".into());
            args.push(ext);
        }
        args
    }
}

/// `.ts` and `.js` files directly in `dir`, by name.
fn folder_extensions(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file() && matches!(path.extension().and_then(|e| e.to_str()), Some("ts" | "js")))
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keel-profile-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_profile_moves_the_session_folder_and_sets_prompt() {
        let data = tmp("default");
        let args = KeelProfile::default().run_args(Some(&data));
        let sessions = data.join("harnesses/keel/sessions");
        assert_eq!(
            args,
            vec![
                OsString::from("--session-dir"),
                sessions.clone().into(),
                OsString::from("--append-system-prompt"),
                OsString::from(KEEL_SYSTEM_PROMPT),
            ]
        );
        assert!(sessions.is_dir() && data.join("harnesses/keel/extensions").is_dir());
        assert_eq!(
            KeelProfile::default().run_args(None),
            vec![
                OsString::from("--append-system-prompt"),
                OsString::from(KEEL_SYSTEM_PROMPT),
            ]
        );
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn default_prompt_does_not_force_a_language() {
        let p = KEEL_SYSTEM_PROMPT;
        assert!(p.contains("concise"), "{p}");
        // The reply follows the user's latest message; unclear input gets English.
        assert!(p.contains("reply in the language of the user's latest message"), "{p}");
        assert!(p.contains("no clear language") && p.contains("reply in English"), "{p}");
        assert!(p.contains("\"Yooooo\""), "{p}");
        assert!(p.contains("does not imply a language"), "{p}");
        // The language rule comes before the Indonesian style guide, and the
        // guide only applies when the user writes Indonesian.
        let rule = p.find("Language:").expect("language rule");
        let guide = p.find("Only when the user writes in Bahasa Indonesia").expect("gated guide");
        assert!(rule < guide, "{p}");
        // Nothing may read as "default to Indonesian".
        assert!(!p.contains("Default (casual or unclear)"), "{p}");
    }

    #[test]
    fn device_id_is_created_once_and_kept() {
        let agent = tmp("device");
        std::fs::write(agent.join("settings.json"), r#"{"packages":["a"]}"#).unwrap();
        let first = device_id(&agent);
        assert!(uuid::Uuid::parse_str(&first).is_ok());
        assert_eq!(device_id(&agent), first);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(agent.join("settings.json")).unwrap()).unwrap();
        assert_eq!((v["deviceId"].as_str(), &v["packages"]), (Some(first.as_str()), &serde_json::json!(["a"])));
        let _ = std::fs::remove_dir_all(&agent);
    }

    #[test]
    fn fetch_script_pins_the_same_pi() {
        let script = include_str!("../../../scripts/fetch-pi.sh");
        assert!(script.contains(&format!("PI_VERSION={PI_VERSION}\n")), "scripts/fetch-pi.sh and PI_VERSION disagree");
    }

    #[test]
    fn bundled_packages_are_registered_once_and_stale_ones_dropped() {
        let root = tmp("packages");
        let exe = root.join("pi/pi");
        let pkg = root.join("pi-packages/pi-commandcode-provider");
        std::fs::create_dir_all(&pkg).unwrap();
        let agent = root.join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        let gone = "/old/Keel.app/Contents/Resources/pi-packages/x";
        let other_existing = root.join("other-debug/pi-packages/pi-commandcode-provider");
        std::fs::create_dir_all(&other_existing).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            format!(
                r#"{{"theme":"dark","packages":["npm:a","{gone}","{}"]}}"#,
                other_existing.to_string_lossy()
            ),
        )
        .unwrap();
        let legacy_skill = agent.join("skills/indonesian-natural");
        std::fs::create_dir_all(&legacy_skill).unwrap();
        assert!(legacy_skill.is_dir());
        register_bundled_packages(&exe, &agent);
        register_bundled_packages(&exe, &agent);
        assert!(!legacy_skill.exists());
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(agent.join("settings.json")).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["packages"], serde_json::json!(["npm:a", pkg.to_string_lossy()]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn antigravity_bridge_needs_its_server() {
        let root = tmp("agy");
        let exe = root.join("pi/pi");
        let bridge = root.join("pi-packages").join(ANTIGRAVITY_BRIDGE);
        let other = root.join("pi-packages/pi-web-access");
        std::fs::create_dir_all(&bridge).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let agent = root.join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        let packages = || {
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(agent.join("settings.json")).unwrap()).unwrap();
            v["packages"].clone()
        };
        register_packages(&exe, &agent, true);
        assert_eq!(packages(), serde_json::json!([bridge.to_string_lossy(), other.to_string_lossy()]));
        // Server gone: the bridge drops out, so it cannot fetch its own.
        register_packages(&exe, &agent, false);
        assert_eq!(packages(), serde_json::json!([other.to_string_lossy()]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn antigravity_env_follows_the_permission_mode() {
        assert!(antigravity_env(None, Some(PermissionMode::FullAccess)).is_empty());
        let bin = Path::new("/h/agy_acp_server.par");
        let env = |permission| -> Vec<(&str, String)> {
            antigravity_env(Some(bin), permission).into_iter().map(|(k, v)| (k, v.to_string_lossy().into_owned())).collect()
        };
        let asking = [
            ("AGY_ACP_BIN", "/h/agy_acp_server.par".to_string()),
            ("AGY_ENGINE", "acp".into()),
            ("AGY_APPROVALS", "off".into()),
            ("AGY_APPROVALS_MODE", "ask".into()),
            ("AGY_SKIP_PERMISSIONS", "0".into()),
            ("AGY_ACP_MODE", "default".into()),
        ];
        assert_eq!(env(None), asking);
        assert_eq!(env(Some(PermissionMode::Ask)), asking);
        let auto_edit = env(Some(PermissionMode::AutoEdit));
        assert_eq!((&auto_edit[..4], &auto_edit[4..]), (&asking[..4], &[("AGY_SKIP_PERMISSIONS", "0".to_string()), ("AGY_ACP_MODE", "auto_edit".into())][..]));
        assert_eq!(env(Some(PermissionMode::FullAccess))[4], ("AGY_SKIP_PERMISSIONS", "1".to_string()));
    }

    #[test]
    fn prompt_and_folder_extensions_are_the_hook() {
        let data = tmp("hook");
        let ext = KeelProfile::extensions_dir(&data);
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(ext.join("b.ts"), "").unwrap();
        std::fs::write(ext.join("a.js"), "").unwrap();
        std::fs::write(ext.join("notes.md"), "").unwrap();
        let profile = KeelProfile { system_prompt: Some("Be brief.".into()), extensions: vec!["builtin:x".into()] };
        let args: Vec<String> = profile.run_args(Some(&data)).into_iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let tail = &args[2..];
        assert_eq!(
            tail,
            [
                "--append-system-prompt".to_string(),
                "Be brief.".into(),
                "--extension".into(),
                "builtin:x".into(),
                "--extension".into(),
                ext.join("a.js").to_string_lossy().into_owned(),
                "--extension".into(),
                ext.join("b.ts").to_string_lossy().into_owned(),
            ]
        );
        let _ = std::fs::remove_dir_all(&data);
    }
}
