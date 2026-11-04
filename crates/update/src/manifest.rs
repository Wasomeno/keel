//! `latest.json`: parse it and decide whether it offers an update.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{UpdateError, msg};

/// Largest archive the client accepts, 1 GiB.
pub const MAX_ARCHIVE: u64 = 1 << 30;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// The full `.minisig` text.
    pub signature: String,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes_url: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // Informational in v1 (UPDATER.md, Manifest).
    min_os: Option<String>,
    #[serde(default)]
    assets: BTreeMap<String, Asset>,
}

/// A newer build for this Mac.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub version: String,
    pub notes_url: Option<String>,
    pub asset: Asset,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// Equal, older, or a prerelease in the feed.
    UpToDate,
    /// Newer, but no asset for this arch.
    NoBuild { version: String },
    Update(Offer),
}

/// Apply the manifest rules. `arch_key` is `darwin-<ARCH>`.
/// `allow_loopback` permits `http://127.0.0.1` / `http://localhost` asset
/// URLs, only when the manifest itself came from loopback (tests).
pub fn evaluate(
    body: &str,
    current: &str,
    arch_key: &str,
    allow_loopback: bool,
) -> Result<Check, UpdateError> {
    let bad = |detail: String| UpdateError::new(msg::UNREACHABLE, detail);
    let manifest: Manifest =
        serde_json::from_str(body).map_err(|e| bad(format!("manifest: {e}")))?;
    let offered = semver::Version::parse(manifest.version.trim())
        .map_err(|e| bad(format!("manifest version {:?}: {e}", manifest.version)))?;
    if !offered.pre.is_empty() || !offered.build.is_empty() {
        return Ok(Check::UpToDate);
    }
    let running = semver::Version::parse(current)
        .map_err(|e| bad(format!("running version {current:?}: {e}")))?;
    if offered <= running {
        return Ok(Check::UpToDate);
    }
    let version = offered.to_string();
    let asset = manifest
        .assets
        .get(arch_key)
        .or_else(|| manifest.assets.get("darwin-universal"));
    let Some(asset) = asset else {
        return Ok(Check::NoBuild { version });
    };
    if asset.size == 0 || asset.size > MAX_ARCHIVE {
        return Err(bad(format!("asset size {} is out of range", asset.size)));
    }
    if !allowed_url(&asset.url, allow_loopback) {
        return Err(bad(format!("asset url is not https: {}", asset.url)));
    }
    if asset.url.contains("/releases/latest/download/") {
        return Err(bad("asset url must be the versioned release asset".into()));
    }
    let hex_ok = asset.sha256.len() == 64
        && asset.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex_ok {
        return Err(bad("asset sha256 is not 64 lowercase hex".into()));
    }
    if asset.signature.trim().is_empty() {
        return Err(UpdateError::new(msg::BAD_SIGNATURE, "asset has no signature"));
    }
    let notes_url = manifest.notes_url.filter(|u| allowed_url(u, false));
    Ok(Check::Update(Offer { version, notes_url, asset: asset.clone() }))
}

/// https anywhere; plain http only to `127.0.0.1` or `localhost`, and only
/// when `allow_loopback`.
pub fn allowed_url(raw: &str, allow_loopback: bool) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => {
            allow_loopback && matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
        }
        _ => false,
    }
}

/// The manifest URL and whether it is loopback, or why it is refused.
pub fn manifest_url() -> Result<(String, bool), UpdateError> {
    match std::env::var("KEEL_UPDATE_URL") {
        Ok(url) if !url.trim().is_empty() => {
            let url = url.trim().to_string();
            if allowed_url(&url, false) {
                Ok((url, false))
            } else if allowed_url(&url, true) {
                Ok((url, true))
            } else {
                Err(UpdateError::new(
                    msg::UNREACHABLE,
                    "KEEL_UPDATE_URL must be https or http://127.0.0.1 / http://localhost",
                ))
            }
        }
        _ => Ok((crate::DEFAULT_MANIFEST_URL.to_string(), false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn feed(version: &str, key: &str, url: &str, size: u64) -> String {
        serde_json::json!({
            "version": version,
            "notes_url": format!("https://github.com/Wasomeno/keel/releases/tag/v{version}"),
            "min_os": "12.0",
            "assets": { key: { "url": url, "sha256": HEX, "size": size, "signature": "untrusted comment: x\nRWQ\n" } }
        })
        .to_string()
    }

    const URL: &str = "https://github.com/Wasomeno/keel/releases/download/v0.2.4/Keel-0.2.4-macos-arm64.tar.xz";

    #[test]
    fn newer_version_for_this_arch_is_an_update() {
        let got = evaluate(&feed("0.2.4", "darwin-aarch64", URL, 145_000_000), "0.2.3", "darwin-aarch64", false).unwrap();
        let Check::Update(offer) = got else { panic!("{got:?}") };
        assert_eq!(offer.version, "0.2.4");
        assert_eq!(offer.asset.size, 145_000_000);
        assert_eq!(offer.notes_url.as_deref(), Some("https://github.com/Wasomeno/keel/releases/tag/v0.2.4"));
    }

    #[test]
    fn equal_older_and_prerelease_are_not_updates() {
        for v in ["0.2.3", "0.2.2", "0.1.9", "0.2.4-beta.1", "0.3.0-rc1", "0.2.4+build5"] {
            let got = evaluate(&feed(v, "darwin-aarch64", URL, 10), "0.2.3", "darwin-aarch64", false).unwrap();
            assert_eq!(got, Check::UpToDate, "{v}");
        }
    }

    #[test]
    fn missing_arch_is_no_build_not_a_failure() {
        let got = evaluate(&feed("0.2.4", "darwin-aarch64", URL, 10), "0.2.3", "darwin-x86_64", false).unwrap();
        assert_eq!(got, Check::NoBuild { version: "0.2.4".into() });
    }

    #[test]
    fn universal_asset_serves_either_arch() {
        let body = feed("0.2.4", "darwin-universal", URL, 10);
        for arch in ["darwin-aarch64", "darwin-x86_64"] {
            assert!(matches!(evaluate(&body, "0.2.3", arch, false).unwrap(), Check::Update(_)));
        }
    }

    #[test]
    fn size_over_the_cap_or_zero_is_refused() {
        assert!(evaluate(&feed("0.2.4", "darwin-aarch64", URL, MAX_ARCHIVE + 1), "0.2.3", "darwin-aarch64", false).is_err());
        assert!(evaluate(&feed("0.2.4", "darwin-aarch64", URL, 0), "0.2.3", "darwin-aarch64", false).is_err());
        assert!(evaluate(&feed("0.2.4", "darwin-aarch64", URL, MAX_ARCHIVE), "0.2.3", "darwin-aarch64", false).is_ok());
    }

    #[test]
    fn non_https_asset_url_is_refused() {
        for url in ["http://github.com/a.tar.xz", "ftp://x/a", "file:///tmp/a.tar.xz", "not a url"] {
            assert!(evaluate(&feed("0.2.4", "darwin-aarch64", url, 10), "0.2.3", "darwin-aarch64", false).is_err(), "{url}");
        }
        // Loopback http only when the manifest came from loopback.
        let local = feed("0.2.4", "darwin-aarch64", "http://127.0.0.1:8123/a.tar.xz", 10);
        assert!(evaluate(&local, "0.2.3", "darwin-aarch64", false).is_err());
        assert!(evaluate(&local, "0.2.3", "darwin-aarch64", true).is_ok());
    }

    #[test]
    fn floating_latest_asset_url_is_refused() {
        let url = "https://github.com/Wasomeno/keel/releases/latest/download/Keel.tar.xz";
        assert!(evaluate(&feed("0.2.4", "darwin-aarch64", url, 10), "0.2.3", "darwin-aarch64", false).is_err());
    }

    #[test]
    fn bad_sha_and_garbage_are_refused() {
        let body = feed("0.2.4", "darwin-aarch64", URL, 10).replace(HEX, "ABC");
        assert!(evaluate(&body, "0.2.3", "darwin-aarch64", false).is_err());
        assert!(evaluate("<html>", "0.2.3", "darwin-aarch64", false).is_err());
        assert!(evaluate(r#"{"version":"v1"}"#, "0.2.3", "darwin-aarch64", false).is_err());
    }

    #[test]
    fn url_rules() {
        assert!(allowed_url("https://example.com/latest.json", false));
        assert!(!allowed_url("http://example.com/latest.json", true));
        assert!(allowed_url("http://127.0.0.1:9000/latest.json", true));
        assert!(allowed_url("http://localhost:9000/latest.json", true));
        assert!(!allowed_url("http://127.0.0.1:9000/latest.json", false));
        assert!(!allowed_url("http://127.0.0.2/latest.json", true));
    }
}
