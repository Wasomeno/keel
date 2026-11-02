//! Data directory and device files.
//!
//! Keel is local-only: every process uses `profiles/local` under the data
//! directory. A `session.json` left by an older sync-capable build is ignored.

use std::fs;
use std::path::{Path, PathBuf};

pub use keel_proto::{ThemeChoice, UiSettings};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::StoreError;

#[derive(Debug, Serialize, Deserialize)]
struct LocalProfileFile {
    id: Uuid,
}

/// One data directory. Device files are created on open.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
    device_id: Uuid,
    local_profile_id: Uuid,
}

impl Layout {
    /// Open `root`. Creates `device-id`, `local-profile.json`, and
    /// `ui-settings.json` when they are missing.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let device_id = load_or_create_device_id(&root.join("device-id"))?;
        let local_profile_id = load_or_create_local_profile(&root.join("local-profile.json"))?;
        ensure_ui_settings(&root.join("ui-settings.json"))?;
        Ok(Self {
            root,
            device_id,
            local_profile_id,
        })
    }

    /// `KEEL_DATA_DIR` when set, otherwise `~/Library/Application Support/Keel` on macOS.
    pub fn open_default() -> Result<Self, StoreError> {
        Self::open(default_root()?)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn device_id(&self) -> Uuid {
        self.device_id
    }

    pub fn local_profile_id(&self) -> Uuid {
        self.local_profile_id
    }

    /// The profile directory: `profiles/local`.
    pub fn profile_dir(&self) -> PathBuf {
        self.root.join("profiles").join("local")
    }

    pub fn store_path(&self) -> PathBuf {
        self.profile_dir().join("store.sqlite")
    }

    pub fn ui_settings(&self) -> Result<UiSettings, StoreError> {
        let text = fs::read_to_string(self.root.join("ui-settings.json"))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn write_ui_settings(&self, settings: &UiSettings) -> Result<(), StoreError> {
        write_atomic(
            &self.root.join("ui-settings.json"),
            &format!("{}\n", serde_json::to_string(settings)?),
        )
    }
}

fn default_root() -> Result<PathBuf, StoreError> {
    if let Some(dir) = std::env::var_os("KEEL_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or(StoreError::NoDataDir)?;
        Ok(PathBuf::from(home).join("Library/Application Support/Keel"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(StoreError::NoDataDir)
    }
}

fn load_or_create_device_id(path: &Path) -> Result<Uuid, StoreError> {
    if path.exists() {
        let text = fs::read_to_string(path)?;
        return Uuid::parse_str(text.trim()).map_err(|_| StoreError::BadDeviceId);
    }
    let id = Uuid::new_v4();
    write_atomic(path, &format!("{id}\n"))?;
    Ok(id)
}

fn load_or_create_local_profile(path: &Path) -> Result<Uuid, StoreError> {
    if path.exists() {
        let text = fs::read_to_string(path)?;
        let file: LocalProfileFile = serde_json::from_str(&text)?;
        return Ok(file.id);
    }
    let id = Uuid::new_v4();
    let file = LocalProfileFile { id };
    write_atomic(path, &format!("{}\n", serde_json::to_string(&file)?))?;
    Ok(id)
}

fn ensure_ui_settings(path: &Path) -> Result<(), StoreError> {
    if path.exists() {
        return Ok(());
    }
    write_atomic(
        path,
        &format!("{}\n", serde_json::to_string(&UiSettings::default())?),
    )
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("keel-layout-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn device_files_are_created_once_and_the_profile_is_local() {
        let root = TempRoot::new();
        let layout = Layout::open(&root.0).unwrap();
        let device = layout.device_id();
        let profile = layout.local_profile_id();
        assert_ne!(device, profile);
        assert_eq!(layout.profile_dir(), root.0.join("profiles").join("local"));

        let again = Layout::open(&root.0).unwrap();
        assert_eq!(again.device_id(), device);
        assert_eq!(again.local_profile_id(), profile);
    }

    #[test]
    fn a_leftover_session_file_is_ignored() {
        let root = TempRoot::new();
        fs::write(
            root.0.join("session.json"),
            r#"{"scope":"synced","owner_id":"00000000-0000-0000-0000-000000000001","device_id":"00000000-0000-0000-0000-000000000002"}"#,
        )
        .unwrap();
        let layout = Layout::open(&root.0).unwrap();
        assert_eq!(layout.profile_dir(), root.0.join("profiles").join("local"));
    }

    #[test]
    fn ui_settings_round_trip() {
        let root = TempRoot::new();
        let layout = Layout::open(&root.0).unwrap();
        assert_eq!(layout.ui_settings().unwrap(), UiSettings::default());
        let mut drafts = std::collections::BTreeMap::new();
        drafts.insert("chat-a".into(), "draft".into());
        let settings = UiSettings {
            open_tabs: vec!["chat-a".into(), "chat-b".into()],
            theme: Some(ThemeChoice::Dark),
            reduced_motion: Some(true),
            sidebar_width: None,
            sidebar_collapsed: false,
            drafts,
            zoom: Some(1.25),
            notifications: keel_proto::NotifySettings { badge: keel_proto::BadgeStyle::Dot, ..Default::default() },
            background: Some("bg-1.jpg".into()),
            background_dim: Some(0.4),
            dotify_nudge: true,
            chat_glow_off: false,
            collapsed_spaces: Vec::new(),
            browser_allowed_sites: vec!["localhost:3000".into()],
            browser_off_chats: Vec::new(),
            harness_prefs: Default::default(),
            recent_models: Default::default(),
            open_model_providers: Default::default(),
        };
        layout.write_ui_settings(&settings).unwrap();
        let again = Layout::open(&root.0).unwrap();
        assert_eq!(again.ui_settings().unwrap(), settings);
    }
}
