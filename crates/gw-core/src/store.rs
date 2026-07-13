//! Event-log storage: one append-only JSONL file per session plus a sidecar
//! meta JSON, under `~/.local/state/gw/sessions/`. The store is the only
//! writer; plugins never touch the filesystem.

use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::procs::AgentLocation;
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
        todo!()
    }

    pub fn open(root: PathBuf) -> Result<Self> {
        todo!()
    }

    /// Append one event (stamping `ts` if absent) and refresh the meta
    /// sidecar. Single `O_APPEND` write per event; a JSONL line is the
    /// atomicity unit.
    pub fn append(&self, provider: &str, event: &Event, loc: Option<&AgentLocation>) -> Result<()> {
        todo!()
    }

    /// All sessions with their full event logs, in no particular order.
    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        todo!()
    }

    /// Delete logs of sessions not updated within `keep`.
    /// Called on panel start.
    pub fn sweep(&self, keep: Duration) -> Result<()> {
        todo!()
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }
}
