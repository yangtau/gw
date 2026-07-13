//! Thin wrapper over the tmux CLI. Scope is always the current session.

use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Pane {
    pub id: String,
    /// Root pid of the pane (usually the shell).
    pub pid: i32,
    pub tty: String,
    pub cwd: PathBuf,
    pub window_index: u32,
    pub window_name: String,
}

/// Panes of the current session (`list-panes -s`).
pub fn list_panes() -> Result<Vec<Pane>> {
    todo!()
}

/// Pane whose tty matches, if any.
pub fn pane_for_tty(tty: &str) -> Result<Option<String>> {
    todo!()
}

/// Open a new window running `argv` in `cwd`; returns the new pane id.
pub fn new_window(name: &str, cwd: &Path, argv: &[String]) -> Result<String> {
    todo!()
}

/// Focus the window/pane containing `pane_id`.
pub fn focus(pane_id: &str) -> Result<()> {
    todo!()
}

/// Last `lines` visible lines of the pane, for the preview.
pub fn capture(pane_id: &str, lines: u32) -> Result<String> {
    todo!()
}

/// Whether we are running inside a tmux display-popup.
pub fn inside_popup() -> bool {
    todo!()
}
