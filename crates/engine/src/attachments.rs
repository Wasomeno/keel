//! Image attachments on this device: `{profile}/attachments/{sha256}`.
//!
//! Content-addressed, so the same screenshot attached twice is stored once and
//! an id can never name a path outside the directory.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use keel_proto::Attachment;
use sha2::{Digest, Sha256};

/// Largest image accepted. Agents downscale anyway; this bounds memory.
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;

pub fn dir(profile: &Path) -> PathBuf {
    profile.join("attachments")
}

/// The file for `id`, or None when the id is not a sha256.
pub fn path(profile: &Path, id: &str) -> Option<PathBuf> {
    Attachment::valid_id(id).then(|| dir(profile).join(id))
}

/// Image type from the file's first bytes; the extension is not trusted.
pub fn sniff_mime(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AttachError {
    #[error("cannot read the file")]
    Unreadable,
    #[error("larger than 20 MB")]
    TooLarge,
    #[error("not a PNG, JPEG, GIF, or WebP image")]
    NotAnImage,
    #[error("cannot store the attachment")]
    Store,
}

/// Copy the image at `source` into the store and describe it.
pub fn attach_file(profile: &Path, source: &Path, name: Option<&str>) -> Result<Attachment, AttachError> {
    let meta = fs::metadata(source).map_err(|_| AttachError::Unreadable)?;
    if !meta.is_file() {
        return Err(AttachError::Unreadable);
    }
    if meta.len() > MAX_BYTES {
        return Err(AttachError::TooLarge);
    }
    let bytes = fs::read(source).map_err(|_| AttachError::Unreadable)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(AttachError::TooLarge);
    }
    let mime = sniff_mime(&bytes).ok_or(AttachError::NotAnImage)?;
    let id = format!("{:x}", Sha256::digest(&bytes));
    let dir = dir(profile);
    fs::create_dir_all(&dir).map_err(|_| AttachError::Store)?;
    let target = dir.join(&id);
    if !target.is_file() {
        // Write then rename, so a crash never leaves a truncated image under its id.
        let tmp = dir.join(format!(".{id}.tmp"));
        let mut file = fs::File::create(&tmp).map_err(|_| AttachError::Store)?;
        file.write_all(&bytes).map_err(|_| AttachError::Store)?;
        file.sync_all().map_err(|_| AttachError::Store)?;
        fs::rename(&tmp, &target).map_err(|_| AttachError::Store)?;
    }
    let name = name
        .map(str::to_string)
        .or_else(|| source.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "image".to_string());
    Ok(Attachment { id, name, mime: mime.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn tmp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keel-attach-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stores_by_content_and_dedupes() {
        let root = tmp("store");
        let src = root.join("shot.png");
        fs::write(&src, PNG).unwrap();
        let a = attach_file(&root.join("profile"), &src, None).unwrap();
        assert_eq!(a.mime, "image/png");
        assert_eq!(a.name, "shot.png");
        assert!(Attachment::valid_id(&a.id));
        let stored = path(&root.join("profile"), &a.id).unwrap();
        assert_eq!(fs::read(&stored).unwrap(), PNG);
        let again = attach_file(&root.join("profile"), &src, Some("other.png")).unwrap();
        assert_eq!(again.id, a.id);
        assert_eq!(again.name, "other.png");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_non_images_directories_and_bad_ids() {
        let root = tmp("reject");
        let text = root.join("notes.png");
        fs::write(&text, b"just text, named like an image").unwrap();
        assert_eq!(attach_file(&root, &text, None), Err(AttachError::NotAnImage));
        assert_eq!(attach_file(&root, &root, None), Err(AttachError::Unreadable));
        assert_eq!(attach_file(&root, &root.join("missing.png"), None), Err(AttachError::Unreadable));
        assert!(path(&root, "../../etc/passwd").is_none());
        assert_eq!(sniff_mime(b"GIF89a.."), Some("image/gif"));
        assert_eq!(sniff_mime(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        let _ = fs::remove_dir_all(&root);
    }
}
