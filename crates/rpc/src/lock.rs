//! Non-blocking exclusive `flock` on `{data_dir}/engine.lock`.
//!
//! The held [`EngineLock`] keeps its file open. Dropping it releases the lock.
//! Creating this file is not opening the snapshot database.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

pub struct EngineLock {
    file: File,
}

pub enum LockResult {
    Acquired(EngineLock),
    Held,
}

pub fn try_lock(path: &Path) -> io::Result<LockResult> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let fd = file.as_raw_fd();
    loop {
        // SAFETY: `fd` belongs to the lock file opened above and stays open on `EngineLock`.
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(LockResult::Acquired(EngineLock { file }));
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if err.kind() == io::ErrorKind::WouldBlock {
            return Ok(LockResult::Held);
        }
        return Err(err);
    }
}

impl Drop for EngineLock {
    fn drop(&mut self) {
        let fd = self.file.as_raw_fd();
        // SAFETY: `fd` is the same lock file. Unlock runs once, on drop.
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
    }
}
