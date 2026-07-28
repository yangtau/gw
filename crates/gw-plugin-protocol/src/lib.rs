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
    /// Resume an ended session with an initial prompt (`{prompt}` placeholder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_prompt: Option<Command>,
    /// Fork a session into a new one; a live target is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<Command>,
    /// Print the provider-native transcript of a session to stdout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<Command>,
    /// Glob template locating the provider-native transcript file
    /// (`{session_id}` placeholder); the newest match wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_glob: Option<String>,
    #[serde(default)]
    pub hooks: Vec<HookFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed_files: Vec<ManagedFile>,
}

/// An agent process is recognized when its argv[0] basename matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessMatch {
    pub argv0: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_args: Vec<String>,
    /// Contiguous argv sequences that disqualify a process. This handles
    /// option/value pairs without excluding an option's other valid values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_arg_sequences: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedFile {
    /// `~` is expanded by the core.
    pub path: String,
    pub content: String,
    /// Single-line prefix used for the ownership header (for example `//`).
    pub comment_prefix: String,
    /// Optional suffix closing the ownership header (for example ` -->`),
    /// so the header can be a closed HTML comment inside Markdown files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment_suffix: String,
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
    /// Provider-native transcript path, when the payload carries one; the
    /// store records the latest into the meta sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Every field beyond the tag is optional: plugins emit what the provider
/// knows and omit the rest. `summary` and `activity` are display one-liners,
/// truncated by the plugin. The core skips log lines it cannot parse, so
/// adding kinds or fields is not a breaking change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// The provider's foreground session changed. Status-neutral.
    SessionFocus,
    SessionStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// `summary`: prompt excerpt — what the agent was asked to do.
    TurnStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// `summary`: final-message excerpt — what the agent concluded.
    TurnEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// The turn aborted with a provider-reported failure.
    /// `reason` is the provider's error type (e.g. `rate_limit`).
    TurnError {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// Blocked mid-turn on the user.
    Attention {
        attention: AttentionKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// `activity`: what is running right now (e.g. a tool name).
    Heartbeat {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        activity: Option<String>,
    },
    /// A subagent spawned inside this session started running.
    /// `agent` is the provider-native subagent id; `summary` is a task excerpt.
    SubagentStart {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    SubagentEnd {
        agent: String,
    },
    SessionEnd,
    /// Core-written operational annotation: this session's agent started a
    /// `gw wait` on another session. Status-neutral, never plugin-emitted.
    WaitStart {
        wait_id: String,
        /// Canonical address (`provider:session-id`) of the awaited session.
        target: String,
    },
    /// Core-written pair closing a `wait_start`. Status-neutral.
    WaitEnd {
        wait_id: String,
        /// The wait result: done | attention | error | stale | idle |
        /// ended | timeout.
        outcome: String,
    },
}

/// Variant order is priority order (approvals are quicker to act on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// A permission dialog is open.
    Approval,
    /// The agent explicitly asked the user something.
    Question,
}
