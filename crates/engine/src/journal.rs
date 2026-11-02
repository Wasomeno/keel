//! Host-local JSONL. `seq` is the replay cursor. Replay does not re-send tool calls.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use keel_proto::AgentEvent;
use serde::{Deserialize, Serialize};

use crate::limits::{JOURNAL_SYNC_EVERY, JOURNAL_SYNC_MS};

pub struct Journal {
    file: File,
    seq: u64,
    pending: u64,
    last_sync_ms: i64,
    terminal: bool,
}

#[derive(Serialize)]
struct Record<'a> {
    seq: u64,
    at: i64,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<&'a AgentEvent>,
}

#[derive(Deserialize)]
struct Stored {
    seq: u64,
    kind: String,
    #[serde(default)]
    command_id: Option<String>,
    #[serde(default)]
    event: Option<AgentEvent>,
}

pub struct Replay {
    pub last_seq: u64,
    pub terminal: bool,
    pub command_id: Option<String>,
}

impl Journal {
    pub fn create(path: &Path, command_id: &str, now: i64) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = File::options().create(true).append(true).open(path)?;
        let mut journal = Self {
            file,
            seq: 0,
            pending: 0,
            last_sync_ms: now,
            terminal: false,
        };
        journal.write(
            now,
            Record {
                seq: 0,
                at: now,
                kind: "start",
                command_id: Some(command_id),
                event: None,
            },
            true,
        )?;
        Ok(journal)
    }

    pub fn append_event(&mut self, now: i64, event: &AgentEvent) -> std::io::Result<u64> {
        let terminal = matches!(event, AgentEvent::Done { .. });
        self.write(
            now,
            Record {
                seq: 0,
                at: now,
                kind: "event",
                command_id: None,
                event: Some(event),
            },
            terminal,
        )?;
        if terminal {
            self.terminal = true;
            tracing::info!(target: "keel_engine::journal", seq = self.seq, "terminal");
        } else {
            tracing::debug!(target: "keel_engine::journal", seq = self.seq, "append");
        }
        Ok(self.seq)
    }

    pub fn poll_sync(&mut self, now: i64) -> std::io::Result<()> {
        if self.pending() > 0 && now.saturating_sub(self.last_sync_ms) >= JOURNAL_SYNC_MS {
            self.sync(now)?;
        }
        Ok(())
    }

    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    pub fn pending(&self) -> u64 {
        self.pending
    }

    fn write(&mut self, now: i64, mut record: Record<'_>, force: bool) -> std::io::Result<()> {
        self.seq += 1;
        record.seq = self.seq;
        let line = serde_json::to_string(&record)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        self.pending += 1;
        if force || self.pending >= JOURNAL_SYNC_EVERY {
            self.sync(now)?;
        }
        Ok(())
    }

    fn sync(&mut self, now: i64) -> std::io::Result<()> {
        self.file.sync_all()?;
        self.pending = 0;
        self.last_sync_ms = now;
        Ok(())
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self.file.flush();
        let _ = self.file.sync_all();
    }
}

pub fn replay(path: &Path) -> std::io::Result<Replay> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut last_seq = 0u64;
    let mut terminal = false;
    let mut command_id = None;
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let Ok(stored) = serde_json::from_str::<Stored>(&line) else {
            continue;
        };
        last_seq = last_seq.max(stored.seq);
        if stored.kind == "start" {
            command_id = stored.command_id;
        }
        if matches!(stored.event, Some(AgentEvent::Done { .. })) {
            terminal = true;
        }
    }
    Ok(Replay {
        last_seq,
        terminal,
        command_id,
    })
}

/// Appends a terminal `Done { Aborted }` after `last_seq` so a later boot
/// sees the run as finished and does not stamp it again.
pub fn seal_aborted(path: &Path, last_seq: u64, now: i64) -> std::io::Result<()> {
    let event = AgentEvent::Done {
        status: keel_proto::TurnStatus::Aborted {},
    };
    let record = Record {
        seq: last_seq + 1,
        at: now,
        kind: "event",
        command_id: None,
        event: Some(&event),
    };
    let line = serde_json::to_string(&record)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let mut file = File::options().append(true).open(path)?;
    // A crash can leave a torn last line; start the seal on its own line.
    file.write_all(b"\n")?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

/// Delete journal files whose mtime is older than `retain_ms` before `now_ms`.
pub fn delete_older(root: &Path, now_ms: i64, retain_ms: i64) -> std::io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    let mut empty_dirs = Vec::new();
    for chat in fs::read_dir(root)? {
        let chat = chat?;
        if !chat.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(chat.path())? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let modified = entry
                .metadata()?
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let modified_ms = modified
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis() as i64)
                .unwrap_or(0);
            if now_ms.saturating_sub(modified_ms) > retain_ms {
                fs::remove_file(entry.path())?;
            }
        }
        empty_dirs.push(chat.path());
    }
    for dir in empty_dirs {
        if fs::read_dir(&dir)?.next().is_none() {
            let _ = fs::remove_dir(dir);
        }
    }
    Ok(())
}

pub fn journal_path(profile: &Path, chat_id: &str, run_id: &str) -> PathBuf {
    profile
        .join("journals")
        .join(chat_id)
        .join(format!("{run_id}.jsonl"))
}
