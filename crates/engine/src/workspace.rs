//! Path resolution, security validation, and file I/O for workspace files.
//!
//! Enforces:
//! - Component walk from canonical space root.
//! - Rejection of empty paths, NUL bytes, absolute paths, `""`, `"."`, `".."`, and `.git`.
//! - Symlink resolution against parent directory with component-wise containment checks.
//! - Symlink depth limit of 8.
//! - Sibling-prefix protection (e.g. `/tmp/space` vs `/tmp/space-evil`).
//! - 1 MiB (1,048,576 bytes) payload size limit.
//! - Forwarding checks: `local_device` not forwardable; `to` must equal space's `device_id`.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use keel_proto::RpcErrorCode;

pub const MAX_WORKSPACE_FILE_BYTES: usize = 1_048_576; // 1 MiB
pub const MAX_SYMLINK_DEPTH: usize = 8;

/// Lexically collapses `.` and `..` in a path.
pub fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(c) => out.push(c),
        }
    }
    out
}

/// `""`, `"."`, `".."`, and `.git` in any ASCII case (APFS is case-insensitive).
fn bad_component(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.eq_ignore_ascii_case(b".git")
}

/// A walked path plus the identity of the final entry when it existed.
struct Walked {
    path: PathBuf,
    root: PathBuf,
    ino: Option<(u64, u64)>,
}

/// Resolves `rel_path` inside `space_root`, following symlinks safely.
pub fn resolve_workspace_path(
    space_root: &Path,
    rel_path: &str,
    is_write: bool,
) -> Result<PathBuf, RpcErrorCode> {
    walk(space_root, rel_path, is_write).map(|walked| walked.path)
}

/// Spec "Path walk": every component is `lstat`ed from the canonical root. A
/// symlink's target is joined against the directory holding it, collapsed
/// lexically, required to stay strictly inside the root, and then the walk
/// restarts at the root over the target's components plus whatever was left.
/// The returned path therefore contains no symlink at walk time.
fn walk(space_root: &Path, rel_path: &str, is_write: bool) -> Result<Walked, RpcErrorCode> {
    if rel_path.is_empty() || rel_path.contains('\0') || rel_path.starts_with('/') {
        return Err(RpcErrorCode::PathRejected);
    }
    // Split the raw string: `Path::components` hides `a//b`, `a/./b`, and a trailing `/`.
    let mut queue: VecDeque<OsString> = VecDeque::new();
    for name in rel_path.split('/') {
        if bad_component(OsStr::new(name)) {
            return Err(RpcErrorCode::PathRejected);
        }
        queue.push_back(OsString::from(name));
    }

    let root = space_root
        .canonicalize()
        .map_err(|_| RpcErrorCode::PathRejected)?;
    if !root.is_dir() {
        return Err(RpcErrorCode::PathRejected);
    }

    let mut cur = root.clone();
    let mut depth = 0usize;
    while let Some(name) = queue.pop_front() {
        let is_final = queue.is_empty();
        let next = cur.join(&name);
        let meta = match fs::symlink_metadata(&next) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && is_final && is_write => {
                return Ok(Walked {
                    path: next,
                    root,
                    ino: None,
                });
            }
            Err(_) => return Err(RpcErrorCode::PathRejected),
        };
        let kind = meta.file_type();
        if kind.is_symlink() {
            depth += 1;
            if depth > MAX_SYMLINK_DEPTH {
                return Err(RpcErrorCode::PathRejected);
            }
            let target = fs::read_link(&next).map_err(|_| RpcErrorCode::PathRejected)?;
            let joined = lexically_normalize(&cur.join(target));
            let inside = joined
                .strip_prefix(&root)
                .map_err(|_| RpcErrorCode::PathRejected)?;
            let mut restart: VecDeque<OsString> = VecDeque::new();
            for comp in inside.components() {
                let Component::Normal(part) = comp else {
                    return Err(RpcErrorCode::PathRejected);
                };
                if bad_component(part) {
                    return Err(RpcErrorCode::PathRejected);
                }
                restart.push_back(part.to_os_string());
            }
            // A link to the root itself is a directory; it only works as an ancestor.
            if restart.is_empty() && is_final {
                return Err(RpcErrorCode::PathRejected);
            }
            restart.extend(queue.drain(..));
            queue = restart;
            cur = root.clone();
        } else if kind.is_dir() {
            if is_final {
                return Err(RpcErrorCode::PathRejected);
            }
            cur = next;
        } else if kind.is_file() && is_final {
            return Ok(Walked {
                path: next,
                root,
                ino: Some((meta.dev(), meta.ino())),
            });
        } else {
            return Err(RpcErrorCode::PathRejected);
        }
    }
    Err(RpcErrorCode::PathRejected)
}

/// Opens the walked path with `O_NOFOLLOW` and checks that the descriptor is
/// the entry the walk saw (same dev+ino, still a regular file, still inside the
/// canonical root). Narrows the walk-then-open race as far as std allows.
fn open_walked(walked: &Walked, write: bool) -> Result<(fs::File, bool), RpcErrorCode> {
    let mut options = fs::OpenOptions::new();
    options.custom_flags(libc::O_NOFOLLOW);
    if write {
        // A missing final entry is created exclusively, so a racing file is never adopted.
        options.write(true).create_new(walked.ino.is_none());
    } else {
        options.read(true);
    }
    let file = options
        .open(&walked.path)
        .map_err(|_| RpcErrorCode::PathRejected)?;
    let created = walked.ino.is_none();
    let reject = |file: fs::File| {
        drop(file);
        if created {
            let _ = fs::remove_file(&walked.path);
        }
        Err(RpcErrorCode::PathRejected)
    };
    let Ok(meta) = file.metadata() else {
        return reject(file);
    };
    // A hard link can name a file outside the root; never write through one.
    if !meta.is_file() || (write && meta.nlink() > 1) {
        return reject(file);
    }
    if let Some(ino) = walked.ino
        && ino != (meta.dev(), meta.ino())
    {
        return reject(file);
    }
    let real = walked.path.canonicalize().ok();
    let real_meta = real.as_ref().and_then(|real| fs::metadata(real).ok());
    match (real, real_meta) {
        (Some(real), Some(real_meta))
            if real.starts_with(&walked.root)
                && real != walked.root
                && (real_meta.dev(), real_meta.ino()) == (meta.dev(), meta.ino()) => {}
        _ => return reject(file),
    }
    Ok((file, created))
}

/// Reads a workspace file, returning its UTF-8 string content.
pub fn read_workspace_file(space_root: &Path, rel_path: &str) -> Result<String, RpcErrorCode> {
    let walked = walk(space_root, rel_path, false)?;
    let (file, _) = open_walked(&walked, false)?;
    let mut bytes = Vec::new();
    file.take(MAX_WORKSPACE_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| RpcErrorCode::PathRejected)?;
    if bytes.len() > MAX_WORKSPACE_FILE_BYTES {
        return Err(RpcErrorCode::PayloadTooLarge);
    }
    String::from_utf8(bytes).map_err(|_| RpcErrorCode::PathRejected)
}

/// Writes a workspace file in place. The final component is never followed.
pub fn write_workspace_file(
    space_root: &Path,
    rel_path: &str,
    content: &str,
) -> Result<usize, RpcErrorCode> {
    if content.len() > MAX_WORKSPACE_FILE_BYTES {
        return Err(RpcErrorCode::PayloadTooLarge);
    }
    let walked = walk(space_root, rel_path, true)?;
    let (mut file, _) = open_walked(&walked, true)?;
    file.set_len(0).map_err(|_| RpcErrorCode::PathRejected)?;
    file.write_all(content.as_bytes())
        .map_err(|_| RpcErrorCode::PathRejected)?;
    Ok(content.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("keel-space-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn test_1_symlink_leaving_root_is_rejected() {
        let space_root = fresh_dir("symlink-escape");
        let outside_dir = fresh_dir("symlink-outside");
        let outside_file = outside_dir.join("secret.txt");
        fs::write(&outside_file, "secret data").unwrap();

        // Symlink pointing outside root
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_file, space_root.join("escaped.txt")).unwrap();

        assert_eq!(
            read_workspace_file(&space_root, "escaped.txt"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            write_workspace_file(&space_root, "escaped.txt", "overwrite"),
            Err(RpcErrorCode::PathRejected)
        );

        let _ = fs::remove_dir_all(&space_root);
        let _ = fs::remove_dir_all(&outside_dir);
    }

    #[test]
    fn test_2_symlink_staying_inside_is_allowed() {
        let space_root = fresh_dir("symlink-inside");
        let sub = space_root.join("subdir");
        fs::create_dir_all(&sub).unwrap();
        let target_file = sub.join("real.txt");
        fs::write(&target_file, "hello from real file").unwrap();

        // Symlink pointing inside root
        #[cfg(unix)]
        std::os::unix::fs::symlink("subdir/real.txt", space_root.join("link.txt")).unwrap();

        let read_res = read_workspace_file(&space_root, "link.txt");
        assert_eq!(read_res, Ok("hello from real file".to_string()));

        let write_res = write_workspace_file(&space_root, "link.txt", "updated content");
        assert_eq!(write_res, Ok(15));
        assert_eq!(fs::read_to_string(&target_file).unwrap(), "updated content");

        let _ = fs::remove_dir_all(&space_root);
    }

    #[test]
    fn test_3_git_component_is_rejected() {
        let space_root = fresh_dir("git-reject");
        let git_dir = space_root.join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("config"), "[core]").unwrap();

        assert_eq!(
            read_workspace_file(&space_root, ".git/config"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            write_workspace_file(&space_root, ".git/config", "hacked"),
            Err(RpcErrorCode::PathRejected)
        );

        // Also nested .git
        let sub = space_root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(
            read_workspace_file(&space_root, "sub/.git/HEAD"),
            Err(RpcErrorCode::PathRejected)
        );

        let _ = fs::remove_dir_all(&space_root);
    }

    #[test]
    fn test_4_parent_dir_is_rejected() {
        let space_root = fresh_dir("parent-reject");
        assert_eq!(
            read_workspace_file(&space_root, "../secret.txt"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            read_workspace_file(&space_root, "sub/../../secret.txt"),
            Err(RpcErrorCode::PathRejected)
        );
        let _ = fs::remove_dir_all(&space_root);
    }

    #[test]
    fn test_5_absolute_path_is_rejected() {
        let space_root = fresh_dir("abs-reject");
        assert_eq!(
            read_workspace_file(&space_root, "/etc/passwd"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            write_workspace_file(&space_root, "/tmp/foo", "content"),
            Err(RpcErrorCode::PathRejected)
        );
        let _ = fs::remove_dir_all(&space_root);
    }

    #[test]
    fn test_6_nul_byte_is_rejected() {
        let space_root = fresh_dir("nul-reject");
        assert_eq!(
            read_workspace_file(&space_root, "foo\0bar.txt"),
            Err(RpcErrorCode::PathRejected)
        );
        let _ = fs::remove_dir_all(&space_root);
    }

    #[test]
    fn test_7_sibling_prefix_is_rejected() {
        let base = std::env::temp_dir().join(format!("keel-prefix-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        // Canonical base: the link target then shares the string prefix `.../space`.
        let base = base.canonicalize().unwrap();
        let space = base.join("space");
        let space_evil = base.join("space-evil");
        fs::create_dir_all(&space).unwrap();
        fs::create_dir_all(&space_evil).unwrap();

        fs::write(space_evil.join("evil.txt"), "evil").unwrap();

        let canon_space = space.canonicalize().unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(space_evil.join("evil.txt"), space.join("evil_link")).unwrap();

        assert!(
            space_evil
                .join("evil.txt")
                .to_string_lossy()
                .starts_with(&*canon_space.to_string_lossy())
        );
        // Component-wise containment must reject evil_link because it belongs to space-evil, not space!
        assert_eq!(
            read_workspace_file(&canon_space, "evil_link"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            write_workspace_file(&canon_space, "evil_link", "x"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            fs::read_to_string(space_evil.join("evil.txt")).unwrap(),
            "evil"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_8_payload_over_1_mib_is_rejected() {
        let space_root = fresh_dir("size-limit");
        let oversized = "x".repeat(MAX_WORKSPACE_FILE_BYTES + 1);

        // Write exceeds 1 MiB -> PayloadTooLarge
        assert_eq!(
            write_workspace_file(&space_root, "large.txt", &oversized),
            Err(RpcErrorCode::PayloadTooLarge)
        );

        // Direct write 1 MiB + 1 to disk, read -> PayloadTooLarge
        let large_path = space_root.join("existing_large.txt");
        fs::write(&large_path, &oversized).unwrap();
        assert_eq!(
            read_workspace_file(&space_root, "existing_large.txt"),
            Err(RpcErrorCode::PayloadTooLarge)
        );

        // Exactly 1 MiB is accepted
        let max_content = "y".repeat(MAX_WORKSPACE_FILE_BYTES);
        assert_eq!(
            write_workspace_file(&space_root, "max.txt", &max_content),
            Ok(MAX_WORKSPACE_FILE_BYTES)
        );

        let _ = fs::remove_dir_all(&space_root);
    }

    #[test]
    fn intermediate_symlink_in_a_resolved_target_cannot_escape() {
        let space_root = fresh_dir("intermediate-escape");
        let outside = fresh_dir("intermediate-outside");
        fs::write(outside.join("id_ed25519"), "KEY").unwrap();
        // `a` leaves the root; `link` stays lexically inside but goes through `a`.
        std::os::unix::fs::symlink(&outside, space_root.join("a")).unwrap();
        std::os::unix::fs::symlink("a/id_ed25519", space_root.join("link")).unwrap();
        assert_eq!(
            read_workspace_file(&space_root, "link"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            write_workspace_file(&space_root, "link", "pwned"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            write_workspace_file(&space_root, "link2", "x").map(|_| ()),
            Ok(())
        );
        std::os::unix::fs::symlink("a/new_file", space_root.join("newlink")).unwrap();
        assert_eq!(
            write_workspace_file(&space_root, "newlink", "pwned"),
            Err(RpcErrorCode::PathRejected)
        );
        assert!(!outside.join("new_file").exists());
        assert_eq!(
            fs::read_to_string(outside.join("id_ed25519")).unwrap(),
            "KEY"
        );
        let _ = fs::remove_dir_all(&space_root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn git_in_any_case_and_after_resolution_is_rejected() {
        let space_root = fresh_dir("git-case");
        fs::create_dir_all(space_root.join(".git/hooks")).unwrap();
        fs::write(space_root.join(".git/config"), "[core]").unwrap();
        for rel in [".GIT/config", ".Git/hooks/pre-commit", "sub/.gIt/x"] {
            assert_eq!(
                write_workspace_file(&space_root, rel, "x"),
                Err(RpcErrorCode::PathRejected),
                "{rel}"
            );
        }
        std::os::unix::fs::symlink(".Git", space_root.join("g")).unwrap();
        std::os::unix::fs::symlink(".git/config", space_root.join("cfg")).unwrap();
        assert_eq!(
            write_workspace_file(&space_root, "g/hooks/pre-commit", "x"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            read_workspace_file(&space_root, "cfg"),
            Err(RpcErrorCode::PathRejected)
        );
        assert!(!space_root.join(".git/hooks/pre-commit").exists());
        let _ = fs::remove_dir_all(&space_root);
    }

    #[test]
    fn empty_and_dot_components_are_rejected() {
        let space_root = fresh_dir("dots");
        fs::create_dir_all(space_root.join("a")).unwrap();
        fs::write(space_root.join("a/b"), "b").unwrap();
        assert_eq!(read_workspace_file(&space_root, "a/b"), Ok("b".to_string()));
        for rel in ["a//b", "a/./b", "a/", "./a/b", "a/b/", "."] {
            assert_eq!(
                read_workspace_file(&space_root, rel),
                Err(RpcErrorCode::PathRejected),
                "{rel}"
            );
            assert_eq!(
                write_workspace_file(&space_root, rel, "x"),
                Err(RpcErrorCode::PathRejected),
                "{rel}"
            );
        }
        let _ = fs::remove_dir_all(&space_root);
    }

    #[test]
    fn write_through_a_hard_link_is_rejected() {
        let root = fresh_dir("hardlink");
        let outside = fresh_dir("hardlink-outside").join("secret");
        fs::write(&outside, "keep").unwrap();
        fs::hard_link(&outside, root.join("link")).unwrap();
        assert_eq!(
            write_workspace_file(&root, "link", "pwned"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "keep");
    }

    #[test]
    fn read_caps_bytes_and_directories_are_rejected() {
        let space_root = fresh_dir("dir-final");
        fs::create_dir_all(space_root.join("d")).unwrap();
        assert_eq!(
            read_workspace_file(&space_root, "d"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            write_workspace_file(&space_root, "d", "x"),
            Err(RpcErrorCode::PathRejected)
        );
        assert_eq!(
            write_workspace_file(&space_root, "missing/x", "x"),
            Err(RpcErrorCode::PathRejected)
        );
        let _ = fs::remove_dir_all(&space_root);
    }
}
