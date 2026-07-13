//! Discovery: join live tmux panes, provider processes, and the event log
//! into the panel's data model. Panes are the source of truth for existence;
//! the log is the source of truth for status.

use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::plugins::Plugin;
use crate::protocol::AttentionKind;
use crate::store::Store;
use crate::tmux::Pane;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Attention(AttentionKind),
    Working,
    Idle,
    Stale,
    /// Agent process discovered but no hook events attributable to it.
    Unknown,
}

/// A live agent: a provider process in a pane of the current session.
#[derive(Debug, Clone)]
pub struct Agent {
    pub provider: String,
    pub pane: Pane,
    pub pid: i32,
    pub cwd: PathBuf,
    pub session_id: Option<String>,
    pub status: AgentStatus,
    /// When the current status was established; None for Unknown.
    pub since: Option<DateTime<Utc>>,
}

/// An ended but resumable session (log has a session id, pane is gone).
#[derive(Debug, Clone)]
pub struct EndedSession {
    pub provider: String,
    pub session_id: String,
    pub cwd: Option<PathBuf>,
    pub ended_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct Snapshot {
    pub agents: Vec<Agent>,
    pub ended: Vec<EndedSession>,
    /// Provider ids discovered as plugins but with no hooks installed.
    pub uninstrumented: Vec<String>,
}

/// One full scan: list panes, find provider processes under each pane,
/// correlate with session logs (by recorded pane id / pid, falling back to
/// unique provider+cwd match), derive statuses. Agents sort Attention first,
/// then by window index; ended sessions sort most recent first.
pub fn snapshot(store: &Store, plugins: &[Plugin], now: DateTime<Utc>, stale_after: Duration) -> Result<Snapshot> {
    todo!()
}
