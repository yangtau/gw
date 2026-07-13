//! `gw hook <provider>`: the ingest path. Called by provider hook configs;
//! must be fast and must never fail the provider's hook (log errors to
//! stderr, always exit 0 once the payload was read).
//!
//! Flow: read stdin → plugin normalize → locate agent (ppid walk from our
//! own parent) → store append → desktop notification on attention events.

use anyhow::Result;

pub fn run(provider: &str) -> Result<()> {
    todo!()
}
