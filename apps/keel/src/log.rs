//! Log file writer with size-based rotation.
//!
//! Log file: `{data_dir}/logs/keel.log`, also stderr when a TTY is attached.
//! Rotates by size at 10 MB, keeping 3 files (`keel.log`, `keel.log.1`, `keel.log.2`).
//! `launchd.log` (launchd's stdout/stderr capture) is rotated the same way each
//! time a writer is created, i.e. at every process start.

use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const DEFAULT_MAX_LOG_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
pub const DEFAULT_MAX_LOG_FILES: usize = 3;

/// Manages rotating log files on disk.
#[derive(Debug)]
pub struct RotatingLog {
    dir: PathBuf,
    file_name: String,
    max_bytes: u64,
    max_files: usize,
    current_file: Option<File>,
    current_size: u64,
}

impl RotatingLog {
    pub fn new(dir: PathBuf, file_name: &str, max_bytes: u64, max_files: usize) -> Self {
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(file_name);
        let (file, size) = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                let sz = f.metadata().map(|m| m.len()).unwrap_or(0);
                (Some(f), sz)
            }
            Err(_) => (None, 0),
        };

        Self {
            dir,
            file_name: file_name.to_string(),
            max_bytes,
            max_files,
            current_file: file,
            current_size: size,
        }
    }

    pub fn rotate(&mut self) -> io::Result<()> {
        self.current_file = None;
        // Shift existing rotated files: e.g. for max_files = 3:
        // delete keel.log.2
        // rename keel.log.1 -> keel.log.2
        // rename keel.log -> keel.log.1
        // open new keel.log
        for i in (1..self.max_files).rev() {
            let target = self.dir.join(format!("{}.{}", self.file_name, i));
            let prev = if i == 1 {
                self.dir.join(&self.file_name)
            } else {
                self.dir.join(format!("{}.{}", self.file_name, i - 1))
            };
            if prev.exists() {
                let _ = fs::rename(&prev, &target);
            }
        }

        let new_path = self.dir.join(&self.file_name);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&new_path)?;
        self.current_size = 0;
        self.current_file = Some(file);
        Ok(())
    }

    pub fn write_bytes(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.current_size > 0 && self.current_size + buf.len() as u64 > self.max_bytes {
            let _ = self.rotate();
        }

        if let Some(ref mut file) = self.current_file {
            file.write_all(buf)?;
            self.current_size += buf.len() as u64;
        }
        Ok(buf.len())
    }

    pub fn flush_bytes(&mut self) -> io::Result<()> {
        if let Some(ref mut file) = self.current_file {
            file.flush()?;
        }
        Ok(())
    }
}

/// launchd writes our stdout/stderr here (see `scripts/sh.keel.engine.plist`).
pub const LAUNCHD_LOG: &str = "launchd.log";

/// Rotate a file someone else appends to (launchd's stdout/stderr capture)
/// once it is over `max_bytes`. If this process's stdout/stderr point at it,
/// they are moved to the fresh file so new output lands in the right place.
pub fn rotate_stdio_log(dir: &Path, file_name: &str, max_bytes: u64, max_files: usize) {
    use std::os::unix::fs::MetadataExt;

    let path = dir.join(file_name);
    let Ok(meta) = fs::metadata(&path) else {
        return;
    };
    if meta.len() <= max_bytes {
        return;
    }
    let mut log = RotatingLog::new(dir.to_path_buf(), file_name, max_bytes, max_files);
    if log.rotate().is_err() {
        return;
    }
    let Some(fresh) = log.current_file.as_ref() else {
        return;
    };
    for fd in [1, 2] {
        // SAFETY: fstat only writes into `st`, a zeroed plain-old-data struct.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            continue;
        }
        if st.st_dev as u64 == meta.dev() && st.st_ino as u64 == meta.ino() {
            use std::os::fd::AsRawFd;
            // SAFETY: both descriptors are open; dup2 atomically repoints `fd`.
            unsafe { libc::dup2(fresh.as_raw_fd(), fd) };
        }
    }
}

/// A writer compatible with `tracing_subscriber::fmt::writer::MakeWriter`.
#[derive(Clone)]
pub struct RotatingLogWriter {
    inner: Arc<Mutex<RotatingLog>>,
    tee_stderr: bool,
}

impl RotatingLogWriter {
    pub fn new(dir: &Path, file_name: &str) -> Self {
        rotate_stdio_log(
            dir,
            LAUNCHD_LOG,
            DEFAULT_MAX_LOG_BYTES,
            DEFAULT_MAX_LOG_FILES,
        );
        let tee_stderr = io::stderr().is_terminal();
        Self::with_options(
            dir.to_path_buf(),
            file_name,
            DEFAULT_MAX_LOG_BYTES,
            DEFAULT_MAX_LOG_FILES,
            tee_stderr,
        )
    }

    pub fn with_options(
        dir: PathBuf,
        file_name: &str,
        max_bytes: u64,
        max_files: usize,
        tee_stderr: bool,
    ) -> Self {
        let log = RotatingLog::new(dir, file_name, max_bytes, max_files);
        Self {
            inner: Arc::new(Mutex::new(log)),
            tee_stderr,
        }
    }
}

impl io::Write for RotatingLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut inner) = self.inner.lock() {
            let _ = inner.write_bytes(buf);
        }
        if self.tee_stderr {
            let _ = io::stderr().write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Ok(mut inner) = self.inner.lock() {
            let _ = inner.flush_bytes();
        }
        if self.tee_stderr {
            let _ = io::stderr().flush();
        }
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for RotatingLogWriter {
    type Writer = RotatingLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "keel-log-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).unwrap_or_default()
    }

    #[test]
    fn test_log_rotation_shifts_contents() {
        let dir = temp_dir("rotate");
        // max 10 bytes per file, keep 3 files total
        let mut log = RotatingLog::new(dir.clone(), "test.log", 10, 3);

        log.write_bytes(b"aaaaaaaa").unwrap();
        assert_eq!(read(&dir, "test.log"), "aaaaaaaa");
        assert!(!dir.join("test.log.1").exists());

        log.write_bytes(b"bbbbbbbb").unwrap();
        assert_eq!(read(&dir, "test.log"), "bbbbbbbb");
        assert_eq!(read(&dir, "test.log.1"), "aaaaaaaa");

        log.write_bytes(b"cccccccc").unwrap();
        assert_eq!(read(&dir, "test.log"), "cccccccc");
        assert_eq!(read(&dir, "test.log.1"), "bbbbbbbb");
        assert_eq!(read(&dir, "test.log.2"), "aaaaaaaa");

        // Oldest falls off; never more than 3 files.
        log.write_bytes(b"dddddddd").unwrap();
        assert_eq!(read(&dir, "test.log"), "dddddddd");
        assert_eq!(read(&dir, "test.log.1"), "cccccccc");
        assert_eq!(read(&dir, "test.log.2"), "bbbbbbbb");
        assert!(!dir.join("test.log.3").exists());

        // A single write over the cap does not rotate an empty file.
        let mut big = RotatingLog::new(dir.join("big"), "b.log", 4, 3);
        big.write_bytes(b"0123456789").unwrap();
        assert!(!dir.join("big/b.log.1").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_stdio_log_rotates_only_when_over_cap() {
        let dir = temp_dir("stdio");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("launchd.log"), "small").unwrap();
        rotate_stdio_log(&dir, "launchd.log", 10, 3);
        assert_eq!(read(&dir, "launchd.log"), "small");

        fs::write(dir.join("launchd.log"), "much more than ten").unwrap();
        rotate_stdio_log(&dir, "launchd.log", 10, 3);
        assert_eq!(read(&dir, "launchd.log"), "");
        assert_eq!(read(&dir, "launchd.log.1"), "much more than ten");
        let _ = fs::remove_dir_all(&dir);
    }
}
