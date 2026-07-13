//! Plugin discovery and invocation. A plugin is any executable named
//! `gw-provider-<id>` on PATH or in `~/.config/gw/providers/bin/`.

use std::path::PathBuf;

use anyhow::Result;

use crate::protocol::{Event, Manifest};

#[derive(Debug, Clone)]
pub struct Plugin {
    pub bin: PathBuf,
    pub manifest: Manifest,
}

/// Find plugin binaries, run `manifest` on each, drop those whose protocol
/// version is unsupported (with a warning on stderr). The plugin dir takes
/// precedence over PATH for the same id.
pub fn discover() -> Result<Vec<Plugin>> {
    todo!()
}

pub fn find(id: &str) -> Result<Plugin> {
    todo!()
}

/// Pipe `payload` to the plugin's `normalize`; parse one event per stdout
/// line. A failing or garbage-printing plugin yields an error, never a panic.
pub fn normalize(plugin: &Plugin, payload: &[u8]) -> Result<Vec<Event>> {
    todo!()
}
