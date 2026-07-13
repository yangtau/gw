//! Event-log storage: one append-only JSONL file per session plus a sidecar
//! meta JSON, under `~/.local/state/gw/sessions/`. The store is the only
//! writer; plugins never touch the filesystem.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::procs::AgentLocation;
use crate::protocol::{Event, EventKind};

pub struct Store {
    root: PathBuf,
}

/// Correlation snapshot, refreshed on every ingest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub provider: String,
    pub session: String,
    pub pane_id: Option<String>,
    pub pid: Option<i32>,
    pub cwd: Option<PathBuf>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct SessionRecord {
    pub meta: SessionMeta,
    pub events: Vec<Event>,
}

impl Store {
    /// `~/.local/state/gw` (override with `GW_STATE_DIR`, for tests).
    pub fn open_default() -> Result<Self> {
        let root = match std::env::var_os("GW_STATE_DIR") {
            Some(root) => PathBuf::from(root),
            None => dirs::home_dir()
                .context("could not determine home directory")?
                .join(".local/state/gw"),
        };
        Self::open(root)
    }

    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("sessions"))
            .with_context(|| format!("create store at {}", root.display()))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(root.join("sessions"), fs::Permissions::from_mode(0o700))?;
        Ok(Self { root })
    }

    /// Append one event (stamping `ts` if absent) and refresh the meta
    /// sidecar. Single `O_APPEND` write per event; a JSONL line is the
    /// atomicity unit.
    pub fn append(&self, provider: &str, event: &Event, loc: Option<&AgentLocation>) -> Result<()> {
        let now = Utc::now();
        let mut event = event.clone();
        if event.ts.is_none() {
            event.ts = Some(now);
        }

        let (log_path, meta_path) = self.paths(provider, &event.session);
        let throttled = matches!(event.kind, EventKind::Heartbeat { .. })
            && last_complete_event(&log_path)?.is_some_and(|last| {
                matches!(last.kind, EventKind::Heartbeat { .. })
                    && last.ts.is_some_and(|ts| ts > now - Duration::seconds(30))
            });

        if !throttled {
            let mut line = serde_json::to_vec(&event)?;
            line.push(b'\n');
            let mut log = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&log_path)
                .with_context(|| format!("open {}", log_path.display()))?;
            fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600))?;
            let written = log.write(&line)?;
            if written != line.len() {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "partial event write").into());
            }
        }

        let previous = if meta_path.exists() {
            Some(
                serde_json::from_slice::<SessionMeta>(&fs::read(&meta_path)?)
                    .with_context(|| format!("parse {}", meta_path.display()))?,
            )
        } else {
            None
        };
        let (pane_id, pid, cwd) = match loc {
            Some(loc) => (loc.pane_id.clone(), Some(loc.pid), loc.cwd.clone()),
            None => previous
                .map(|meta| (meta.pane_id, meta.pid, meta.cwd))
                .unwrap_or((None, None, None)),
        };
        let meta = SessionMeta {
            provider: provider.to_owned(),
            session: event.session.clone(),
            pane_id,
            pid,
            cwd,
            updated_at: now,
        };
        write_private(&meta_path, &serde_json::to_vec_pretty(&meta)?)?;
        Ok(())
    }

    /// All sessions with their full event logs, in no particular order.
    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        let mut sessions = Vec::new();
        for entry in fs::read_dir(self.sessions_dir())? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    eprintln!("warning: could not read session entry: {error}");
                    continue;
                }
            };
            let path = entry.path();
            let Some(sid) = meta_sid(&path) else {
                continue;
            };
            let log_path = self.sessions_dir().join(format!("{sid}.jsonl"));
            match read_record(&path, &log_path) {
                Ok(record) => sessions.push(record),
                Err(error) => eprintln!(
                    "warning: skipping corrupt session {}: {error:#}",
                    path.display()
                ),
            }
        }
        Ok(sessions)
    }

    /// Delete logs of sessions not updated within `keep`.
    /// Called on panel start.
    pub fn sweep(&self, keep: Duration) -> Result<()> {
        let cutoff = Utc::now() - keep;
        let mut stale = Vec::new();
        for entry in fs::read_dir(self.sessions_dir())? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    eprintln!("warning: could not read session entry: {error}");
                    continue;
                }
            };
            let path = entry.path();
            let Some(sid) = meta_sid(&path) else {
                continue;
            };
            match fs::read(&path)
                .with_context(|| format!("read {}", path.display()))
                .and_then(|bytes| {
                    serde_json::from_slice::<SessionMeta>(&bytes)
                        .with_context(|| format!("parse {}", path.display()))
                }) {
                Ok(meta) if meta.updated_at < cutoff => stale.push(sid),
                Ok(_) => {}
                Err(error) => eprintln!(
                    "warning: skipping corrupt session meta {}: {error:#}",
                    path.display()
                ),
            }
        }

        for sid in stale {
            for entry in fs::read_dir(self.sessions_dir())? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{sid}."))
                {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(())
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn paths(&self, provider: &str, session: &str) -> (PathBuf, PathBuf) {
        let sid = session_id(provider, session);
        let dir = self.sessions_dir();
        (
            dir.join(format!("{sid}.jsonl")),
            dir.join(format!("{sid}.meta.json")),
        )
    }
}

fn session_id(provider: &str, session: &str) -> String {
    let digest = Sha256::digest(format!("{provider}:{session}").as_bytes());
    format!("{digest:x}")[..16].to_owned()
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    file.write_all(contents)?;
    Ok(())
}

fn last_complete_event(path: &Path) -> Result<Option<Event>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    let last = complete_lines(&bytes)
        .rev()
        .find_map(|line| serde_json::from_slice(line).ok());
    Ok(last)
}

// Unparseable lines are skipped, not fatal: the event vocabulary evolves and
// logs written by other versions must still replay.
fn read_record(meta_path: &Path, log_path: &Path) -> Result<SessionRecord> {
    let meta = serde_json::from_slice(&fs::read(meta_path)?)?;
    let bytes = fs::read(log_path)?;
    let events = complete_lines(&bytes)
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    Ok(SessionRecord { meta, events })
}

fn complete_lines(bytes: &[u8]) -> impl DoubleEndedIterator<Item = &[u8]> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| line.last() == Some(&b'\n'))
        .map(|line| &line[..line.len() - 1])
}

fn meta_sid(path: &Path) -> Option<String> {
    path.file_name()?
        .to_str()?
        .strip_suffix(".meta.json")
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration as StdDuration;

    fn event(session: &str, kind: EventKind) -> Event {
        Event {
            v: 1,
            ts: None,
            session: session.to_owned(),
            kind,
        }
    }

    #[test]
    fn append_read_roundtrip_and_stamp_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state")).unwrap();
        let input = event("session-1", EventKind::TurnStart { summary: None });

        store.append("test", &input, None).unwrap();

        let records = store.sessions().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].meta.provider, "test");
        assert_eq!(records[0].meta.session, "session-1");
        assert_eq!(records[0].events.len(), 1);
        assert!(records[0].events[0].ts.is_some());
        assert!(input.ts.is_none());
    }

    #[test]
    fn throttles_consecutive_heartbeats_and_refreshes_meta() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state")).unwrap();
        let heartbeat = event("session-1", EventKind::Heartbeat { activity: None });
        store.append("test", &heartbeat, None).unwrap();
        let before = store.sessions().unwrap().remove(0).meta.updated_at;
        thread::sleep(StdDuration::from_millis(2));

        store.append("test", &heartbeat, None).unwrap();

        let record = store.sessions().unwrap().remove(0);
        assert_eq!(record.events.len(), 1);
        assert!(record.meta.updated_at > before);
    }

    #[test]
    fn refreshes_and_preserves_location_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state")).unwrap();
        let first = AgentLocation {
            pid: 42,
            pane_id: Some("%7".to_owned()),
            cwd: Some(PathBuf::from("/tmp/project")),
        };
        store
            .append(
                "test",
                &event("session-1", EventKind::TurnStart { summary: None }),
                Some(&first),
            )
            .unwrap();

        store
            .append(
                "test",
                &event("session-1", EventKind::TurnEnd { summary: None }),
                None,
            )
            .unwrap();

        let meta = store.sessions().unwrap().remove(0).meta;
        assert_eq!(meta.pid, Some(42));
        assert_eq!(meta.pane_id.as_deref(), Some("%7"));
        assert_eq!(meta.cwd, Some(PathBuf::from("/tmp/project")));
    }

    #[test]
    fn sweep_removes_stale_session_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state")).unwrap();
        store
            .append(
                "test",
                &event("old", EventKind::TurnEnd { summary: None }),
                None,
            )
            .unwrap();
        store
            .append(
                "test",
                &event("new", EventKind::TurnEnd { summary: None }),
                None,
            )
            .unwrap();
        let (_, old_meta_path) = store.paths("test", "old");
        let mut old_meta: SessionMeta =
            serde_json::from_slice(&fs::read(&old_meta_path).unwrap()).unwrap();
        old_meta.updated_at = Utc::now() - Duration::days(2);
        write_private(&old_meta_path, &serde_json::to_vec(&old_meta).unwrap()).unwrap();

        store.sweep(Duration::days(1)).unwrap();

        let records = store.sessions().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].meta.session, "new");
        assert!(!old_meta_path.exists());
    }

    #[test]
    fn skips_lines_it_cannot_parse_and_keeps_the_rest() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state")).unwrap();
        store
            .append(
                "mixed",
                &event("s", EventKind::TurnStart { summary: None }),
                None,
            )
            .unwrap();
        let (log, _) = store.paths("mixed", "s");
        // A partial tail, garbage, and a line from a retired vocabulary
        // (attention kind `notification`) must not take the session down.
        OpenOptions::new()
            .append(true)
            .open(log)
            .unwrap()
            .write_all(
                b"not json\n{\"v\":1,\"ts\":\"2026-01-01T00:00:00Z\",\"session\":\"s\",\"kind\":\"attention\",\"attention\":\"notification\"}\n{\"v\":1",
            )
            .unwrap();

        let records = store.sessions().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].events.len(), 1);
        assert_eq!(
            records[0].events[0].kind,
            EventKind::TurnStart { summary: None }
        );
    }
}
