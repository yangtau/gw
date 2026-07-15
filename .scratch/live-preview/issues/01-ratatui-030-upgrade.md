# 01 — Upgrade workspace to ratatui 0.30 + crossterm 0.29

Status: resolved

Prerequisite for tui-term 0.3.4. See `.scratch/live-preview/spec.md` (Commit 1).

- Bump `ratatui = "0.30"`, `crossterm = "0.29"` in the workspace manifest.
- Adapt `crates/gw/src/tui.rs` (and anything else that breaks) to the new APIs.
- No behavior change. `cargo clippy --workspace` and `cargo test --workspace` clean.
- Land as its own commit.

## Comments

Upgraded the workspace to ratatui 0.30 and crossterm 0.29. The existing TUI APIs
remained compatible, so no behavior code changes were required.
