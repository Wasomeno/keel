//! The two GETs, and the minisign check. Blocking: call from a background
//! thread.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{Asset, MAX_ARCHIVE, Result, UpdateError, msg};

/// `latest.json` is small; anything bigger is not a manifest.
const MAX_MANIFEST: u64 = 64 * 1024;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .redirects(5)
        .user_agent(&format!("Keel/{}", crate::CURRENT_VERSION))
        .build()
}

fn unreachable(e: impl std::fmt::Display) -> UpdateError {
    UpdateError::new(msg::UNREACHABLE, e.to_string())
}

/// GET the manifest text. No query string, no device id.
pub fn fetch_manifest(url: &str) -> Result<String> {
    let resp = agent().get(url).call().map_err(unreachable)?;
    let mut body = String::new();
    resp.into_reader()
        .take(MAX_MANIFEST)
        .read_to_string(&mut body)
        .map_err(unreachable)?;
    Ok(body)
}

/// Stream the archive to `dir/archive.tar.xz`, hashing as it writes.
/// `progress` gets `Some(fraction)` once `Content-Length` is known, at most
/// about every percent.
pub fn download(
    asset: &Asset,
    dir: &Path,
    mut progress: impl FnMut(Option<f32>),
) -> Result<PathBuf> {
    let cap = asset.size.min(MAX_ARCHIVE);
    let resp = agent().get(&asset.url).call().map_err(unreachable)?;
    let length = resp
        .header("Content-Length")
        .and_then(|v| v.trim().parse::<u64>().ok());
    if length.is_some_and(|n| n != asset.size) {
        return Err(UpdateError::new(msg::BAD_ARCHIVE, "Content-Length does not match size"));
    }
    let path = dir.join("archive.tar.xz");
    let mut file = std::fs::File::create(&path).map_err(unreachable)?;
    let mut reader = resp.into_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    let mut last_step = -1i64;
    progress(None);
    loop {
        let n = reader.read(&mut buf).map_err(unreachable)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > cap {
            return Err(UpdateError::new(msg::BAD_ARCHIVE, "body is larger than size"));
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(unreachable)?;
        if let Some(len) = length {
            let step = (total * 100 / len.max(1)) as i64;
            if step != last_step {
                last_step = step;
                progress(Some(total as f32 / len.max(1) as f32));
            }
        }
    }
    file.flush().map_err(unreachable)?;
    drop(file);
    if total != asset.size {
        return Err(UpdateError::new(msg::BAD_ARCHIVE, format!("got {total} bytes, expected {}", asset.size)));
    }
    let hex: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if hex != asset.sha256 {
        return Err(UpdateError::new(msg::BAD_ARCHIVE, "sha256 does not match"));
    }
    Ok(path)
}

/// Whether `public_key` (the `update-key.pub` text) is a real minisign key.
/// A placeholder (no key generated yet) is not.
pub fn public_key_usable(public_key: &str) -> bool {
    minisign_verify::PublicKey::decode(public_key.trim()).is_ok()
}

/// minisign over the file's bytes, streamed.
pub fn verify_signature(path: &Path, signature: &str, public_key: &str) -> Result<()> {
    let key = minisign_verify::PublicKey::decode(public_key.trim())
        .map_err(|e| UpdateError::new(msg::NO_KEY, e.to_string()))?;
    let bad = |e: &dyn std::fmt::Display| UpdateError::new(msg::BAD_SIGNATURE, e.to_string());
    let sig = minisign_verify::Signature::decode(signature.trim()).map_err(|e| bad(&e))?;
    let mut verifier = key.verify_stream(&sig).map_err(|e| bad(&e))?;
    let mut file = std::fs::File::open(path).map_err(|e| bad(&e))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| bad(&e))?;
        if n == 0 {
            break;
        }
        verifier.update(&buf[..n]);
    }
    verifier.finalize().map_err(|e| bad(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair_and_sig(data: &[u8]) -> (String, String) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk = kp.pk.to_box().unwrap().to_string();
        let sig = minisign::sign(Some(&kp.pk), &kp.sk, std::io::Cursor::new(data), None, None)
            .unwrap()
            .to_string();
        (pk, sig)
    }

    #[test]
    fn minisign_verifies_and_a_flipped_byte_fails() {
        let dir = std::env::temp_dir().join(format!("keel-update-test-sig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let (pk, sig) = keypair_and_sig(&data);
        let path = dir.join("archive.tar.xz");
        std::fs::write(&path, &data).unwrap();
        verify_signature(&path, &sig, &pk).unwrap();

        let mut flipped = data.clone();
        flipped[1234] ^= 0x01;
        std::fs::write(&path, &flipped).unwrap();
        let err = verify_signature(&path, &sig, &pk).unwrap_err();
        assert_eq!(err.message, msg::BAD_SIGNATURE);

        // Another key's signature fails too.
        std::fs::write(&path, &data).unwrap();
        let (other_pk, _) = keypair_and_sig(b"x");
        assert!(verify_signature(&path, &sig, &other_pk).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_repo_key_is_a_real_minisign_key() {
        assert!(public_key_usable(include_str!("../../../apps/keel/update-key.pub")));
    }

    #[test]
    fn a_placeholder_key_fails_safely() {
        let placeholder = "untrusted comment: PLACEHOLDER, not a key.\nPLACEHOLDER-KEEL-UPDATE-KEY-NOT-GENERATED\n";
        assert!(!public_key_usable(placeholder));
        let err = verify_signature(Path::new("/nonexistent"), "x", placeholder).unwrap_err();
        assert_eq!(err.message, msg::NO_KEY);
    }
}
