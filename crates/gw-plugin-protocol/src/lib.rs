//! JSON contract between the gw core and provider plugins. See docs/protocol.md.
//!
//! Plugins are pure translators: `manifest` prints a [`Manifest`]; `normalize`
//! reads one raw hook payload on stdin and prints newline-delimited [`Event`]s.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub protocol: u32,
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub color: Option<String>,
    pub process: ProcessMatch,
    pub launch: Command,
    #[serde(default)]
    pub resume: Option<Command>,
    #[serde(default)]
    pub hooks: Vec<HookFile>,
}

/// An agent process is recognized when its argv[0] basename matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessMatch {
    pub argv0: Vec<String>,
}

/// Command template; the core expands `{session_id}` and `{cwd}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookFile {
    /// `~` is expanded by the core.
    pub path: String,
    pub format: FileFormat,
    pub patches: Vec<Patch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    Json,
    Toml,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    /// JSON-Pointer-style path; for TOML it addresses nested tables.
    pub pointer: String,
    pub mode: PatchMode,
    pub value: serde_json::Value,
}

/// `Ensure`: the pointer addresses an array; setup guarantees it contains
/// `value` (deep equality) and removes that element on uninstall.
/// `Set`: setup writes `value` at the pointer and keeps it on uninstall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PatchMode {
    Ensure,
    Set,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub v: u32,
    /// Stamped by the core with arrival time when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<DateTime<Utc>>,
    /// Provider-native session id extracted from the payload.
    pub session: String,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    SessionStart,
    TurnStart,
    TurnEnd,
    Attention {
        attention: AttentionKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Heartbeat,
    SessionEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    Approval,
    Question,
    Notification,
}
