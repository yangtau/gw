//! Process facts via `ps`: the bridge between hook processes, provider
//! processes, and tmux panes. No /proc on macOS, so everything goes through
//! one `ps -axo pid,ppid,tty,args` snapshot.

use std::path::PathBuf;

use anyhow::Result;

use crate::protocol::Manifest;

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub tty: Option<String>,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AgentLocation {
    pub pid: i32,
    pub pane_id: Option<String>,
    pub cwd: Option<PathBuf>,
}

/// One `ps` snapshot of all processes.
pub fn snapshot() -> Result<Vec<Proc>> {
    todo!()
}

/// Whether the process looks like an agent of `manifest`'s provider:
/// any of the first few argv tokens has a basename matching `process.argv0`
/// (first few, not just argv[0], to survive wrappers like `node /path/claude`).
pub fn matches_provider(proc_: &Proc, manifest: &Manifest) -> bool {
    todo!()
}

/// Walk the ppid chain from `from_pid` (a hook process) to the nearest
/// ancestor matching any manifest; resolve its tty to a pane and its cwd.
/// Returns the matched provider id with the location.
pub fn locate_agent(from_pid: i32, manifests: &[Manifest]) -> Result<Option<(String, AgentLocation)>> {
    todo!()
}

/// Provider processes running inside `pane_root_pid`'s process tree
/// (pane pid is usually a shell; the agent is a descendant).
pub fn provider_procs_under(pane_root_pid: i32, procs: &[Proc], manifests: &[Manifest]) -> Vec<(String, Proc)> {
    todo!()
}

/// Current working directory of a pid (via `lsof -a -d cwd -p <pid> -Fn`).
pub fn cwd_of(pid: i32) -> Option<PathBuf> {
    todo!()
}
