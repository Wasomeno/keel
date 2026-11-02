//! `{data_dir}/ipc.token`: 32 bytes from `/dev/urandom`, lowercase hex, mode `0600`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

const TOKEN_BYTES: usize = 32;

pub fn load_token(data_dir: &Path) -> Option<String> {
    let path = data_dir.join("ipc.token");
    let token = read_existing(&path).ok()?;
    if token.is_empty() { None } else { Some(token) }
}

/// Create the token after the lock is won. An existing non-empty token is kept,
/// but its mode is forced back to `0600`. An empty one is regenerated.
pub fn ensure_token(data_dir: &Path) -> io::Result<String> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join("ipc.token");
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.is_file() {
            return Err(io::Error::other("ipc.token is not a regular file"));
        }
        if meta.permissions().mode() & 0o777 != 0o600 {
            tracing::warn!(target: "keel_rpc", "ipc.token had loose permissions; resetting to 0600");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        let token = read_existing(&path)?;
        if !token.is_empty() {
            return Ok(token);
        }
        fs::remove_file(&path)?;
    }
    let token = random_hex()?;
    let tmp = data_dir.join("ipc.token.tmp");
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(token.as_bytes())?;
        file.write_all(b"\n")?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    if path.exists() {
        let _ = fs::remove_file(&tmp);
        return read_existing(&path);
    }
    fs::rename(&tmp, &path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(token)
}

fn read_existing(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut buf = [0u8; 128];
    let n = file.read(&mut buf)?;
    let text = String::from_utf8_lossy(&buf[..n]);
    Ok(text.trim().to_string())
}

fn random_hex() -> io::Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    let mut urandom = File::open("/dev/urandom")?;
    urandom.read_exact(&mut bytes)?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
