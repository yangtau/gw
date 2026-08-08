//! Event-log storage: one append-only JSONL file per session plus a sidecar
//! meta JSON, under `~/.local/state/gw/sessions/`. The panel reads them to
//! surface per-session status and activity.

use std::fs::{self};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::Event;

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
    /// Latest provider-native transcript path seen in an event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
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
        Self::open(default_state_dir()?)
    }

    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("sessions"))
            .with_context(|| format!("create store at {}", root.display()))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(root.join("sessions"), fs::Permissions::from_mode(0o700))?;
        Ok(Self { root })
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
            let Some(stem) = meta_stem(&path) else {
                continue;
            };
            let log_path = self.sessions_dir().join(format!("{stem}.jsonl"));
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
            let Some(stem) = meta_stem(&path) else {
                continue;
            };
            match fs::read(&path)
                .with_context(|| format!("read {}", path.display()))
                .and_then(|bytes| {
                    serde_json::from_slice::<SessionMeta>(&bytes)
                        .with_context(|| format!("parse {}", path.display()))
                }) {
                Ok(meta) if meta.updated_at < cutoff => stale.push(stem),
                Ok(_) => {}
                Err(error) => eprintln!(
                    "warning: skipping corrupt session meta {}: {error:#}",
                    path.display()
                ),
            }
        }

        for stem in stale {
            for entry in fs::read_dir(self.sessions_dir())? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{stem}."))
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
}

pub(crate) fn default_state_dir() -> Result<PathBuf> {
    match std::env::var_os("GW_STATE_DIR") {
        Some(root) => Ok(PathBuf::from(root)),
        None => Ok(dirs::home_dir()
            .context("could not determine home directory")?
            .join(".local/state/gw")),
    }
}

// Unparseable lines are skipped, not fatal: the event vocabulary evolves and
// logs written by other versions must still replay.
fn read_record(meta_path: &Path, log_path: &Path) -> Result<SessionRecord> {
    let meta = serde_json::from_slice(&fs::read(meta_path)?)?;
    let bytes = match fs::read(log_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
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

fn meta_stem(path: &Path) -> Option<String> {
    path.file_name()?
        .to_str()?
        .strip_suffix(".meta.json")
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::EventKind;
    use std::fs::OpenOptions;
    use std::io::Write;

    fn write_session(
        store: &Store,
        stem: &str,
        provider: &str,
        session: &str,
        events: &[Event],
    ) {
        let meta = SessionMeta {
            provider: provider.to_owned(),
            session: session.to_owned(),
            pane_id: None,
            pid: None,
            cwd: None,
            transcript_path: None,
            updated_at: Utc::now(),
        };
        fs::write(
            store.sessions_dir().join(format!("{stem}.meta.json")),
            serde_json::to_vec_pretty(&meta).unwrap(),
        )
        .unwrap();
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(store.sessions_dir().join(format!("{stem}.jsonl")))
            .unwrap();
        for event in events {
            let mut line = serde_json::to_vec(event).unwrap();
            line.push(b'\n');
            log.write_all(&line).unwrap();
        }
    }

    fn event(session: &str, kind: EventKind) -> Event {
        Event {
            v: 1,
            ts: Some(Utc::now()),
            session: session.to_owned(),
            transcript: None,
            kind,
        }
    }

    #[test]
    fn reads_session_records() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state")).unwrap();
        write_session(
            &store,
            "session-1",
            "test",
            "s1",
            &[event("s1", EventKind::TurnStart { summary: None })],
        );

        let records = store.sessions().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].meta.provider, "test");
        assert_eq!(records[0].meta.session, "s1");
        assert_eq!(records[0].events.len(), 1);
    }

    #[test]
    fn sweep_removes_stale_session_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state")).unwrap();
        write_session(
            &store,
            "old",
            "test",
            "old",
            &[event("old", EventKind::TurnEnd { summary: None })],
        );
        write_session(
            &store,
            "new",
            "test",
            "new",
            &[event("new", EventKind::TurnEnd { summary: None })],
        );

        let old_meta_path = store.sessions_dir().join("old.meta.json");
        let mut old_meta: SessionMeta =
            serde_json::from_slice(&fs::read(&old_meta_path).unwrap()).unwrap();
        old_meta.updated_at = Utc::now() - Duration::days(2);
        fs::write(&old_meta_path, serde_json::to_vec(&old_meta).unwrap()).unwrap();

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
        write_session(
            &store,
            "mixed",
            "mixed",
            "s",
            &[event("s", EventKind::TurnStart { summary: None })],
        );
        // A partial tail, garbage, and a line from a retired vocabulary
        // (attention kind `notification`) must not take the session down.
        OpenOptions::new()
            .append(true)
            .open(store.sessions_dir().join("mixed.jsonl"))
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
