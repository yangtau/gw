//! Hook installation into provider configs. Surgical by contract:
//! unrelated keys, ordering, and TOML formatting are preserved; targets are
//! backed up (`<file>.gw-backup`) before the first write; install and remove
//! are idempotent. See docs/protocol.md for patch semantics.

use std::path::PathBuf;

use anyhow::Result;

use crate::protocol::Manifest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    AlreadyApplied,
}

/// Apply every hook patch of every manifest. Returns touched files.
pub fn install(manifests: &[Manifest]) -> Result<Vec<(PathBuf, Outcome)>> {
    todo!()
}

/// Reverse `install`: remove `ensure` elements, keep `set` values.
pub fn remove(manifests: &[Manifest]) -> Result<Vec<(PathBuf, Outcome)>> {
    todo!()
}
