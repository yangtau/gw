# 02 — Live preview via nested read-only tmux client

Status: resolved
Blocked by: 01

Full spec: `.scratch/live-preview/spec.md` (Mechanism, Decisions, Commit 2).
Decision record: `docs/adr/0003-live-preview-nested-client.md`.

Deliverables:

- tmux helpers in `crates/gw-core/src/tmux.rs` (window_id in `Pane`, grouped preview
  session create/select/kill, per-window aggressive-resize toggle, stale-session sweep).
- `crates/gw/src/preview.rs`: PTY + read-only tmux attach + vt100 parser + notification
  channel, with the lifecycle and error handling exactly as specced.
- `crates/gw/src/tui.rs` integration: select-loop branch with ~30 fps throttle,
  live widget via tui-term, capture-snapshot fallback preserved.
- Land as one commit on top of 01. Run the Verification section of the spec
  (automated parts; list the manual steps you could not run in your summary).

## Comments

Added the grouped read-only tmux client, PTY/vt100 renderer, redraw notifications,
window resize lifecycle, stale-session sweep, and snapshot fallback.

Fixed the attach lifecycle after real tmux verification: defer `destroy-unattached`
until the client is attached, keep the read-only client resize-aware, restore released
windows immediately, and log preview degradation errors to `tui.log`.
