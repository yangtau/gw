# Live preview via nested read-only tmux client

Replace the capture-pane snapshot preview with a live, full-fidelity view of the
selected Agent's window. See `docs/adr/0003-live-preview-nested-client.md` for the
decision record and `CONTEXT.md` ("Preview") for the term.

## Mechanism

A grouped tmux session shares the window list with the user's session. gw attaches a
**read-only nested client** to that grouped session inside a PTY it owns, sized to the
preview area, and renders the client's screen into the preview Rect via an embedded
terminal emulator. Because the nested client is the only session for which the
previewed window is current (with `aggressive-resize on`), tmux resizes the window to
the preview PTY's size and the agent program re-renders for it. This is deliberate and
accepted (see ADR).

Environment facts (verified):

- tmux 3.7b on the dev machine; grouped sessions, `window-size latest` (default),
  `aggressive-resize`, `attach -r`, `destroy-unattached` all available.
- tui-term 0.3.4 (actively maintained) requires ratatui 0.30 + crossterm 0.29;
  gw is currently on ratatui 0.29 + crossterm 0.28 → prerequisite upgrade.
- Default `window-size latest` without aggressive-resize lets the *user's* client
  (attached to a session sharing the window) reclaim the window size on every
  keystroke — so per-window `aggressive-resize on` during preview is required,
  not an optimization.
- copy-mode is pane-level shared state → the preview must never enter it.
- zoom (`resize-pane -Z`) is window-level shared state → the preview never zooms;
  it shows the whole window (gw-launched agent windows are single-pane anyway).

## Decisions (settled, do not relitigate)

1. **Resize policy: fully accepted.** All agent windows get live preview, including
   the window under the popup (its size may flap with user input; accepted, popup
   covers it). `aggressive-resize` is set per-window on preview enter, unset
   (`set -uw`) on leave.
2. **Dependency path:** upgrade workspace to ratatui 0.30 + crossterm 0.29 as a
   separate prerequisite commit, then use tui-term 0.3.4 (vt100 feature) +
   portable-pty + vt100.
3. **Topology:** one persistent grouped session + one persistent nested client per
   gw process; selection changes are `select-window` on the preview session only.
4. **Interaction: pure read-only.** No input is ever written to the nested client's
   PTY. No in-preview scrolling. Enter still jumps to the pane (existing behavior).
5. **Fallback:** if the live client cannot be established or dies, fall back to the
   existing `tmux::capture` snapshot path (keep that code).

## Implementation plan

### Commit 1 — prerequisite upgrade

Upgrade workspace deps: ratatui `0.29` → `0.30`, crossterm `0.28` → `0.29`.
Adapt `crates/gw/src/tui.rs` to API changes. No behavior change; `cargo clippy`
and tests clean.

### Commit 2 — live preview

**New tmux helpers** (`crates/gw-core/src/tmux.rs`):

- Add `window_id` (`#{window_id}`, e.g. `@3`) to the `list-panes` format and the
  `Pane` struct (update the parser test).
- `preview_session_create(name, group_with)` →
  `new-session -d -s <name> -t <current session>` then on the new session:
  `set-option -t <name> destroy-unattached on` and `set-option -t <name> status off`.
- `preview_select_window(session, window_id)` → `select-window -t <session>:<window_id>`.
- `set_window_aggressive_resize(window_id, on: bool)` →
  `set-option -w -t <window_id> aggressive-resize on` / `set-option -uw ...`.
  (`-u` on leave restores the inherited value; a user's explicit per-window setting
  would be lost — accepted, noted here.)
- `kill_session(name)`.
- `stale_preview_sessions()` → list sessions named `gw-preview-<pid>` whose pid is
  no longer alive (for the startup sweep).
- Current session name: `display-message -p '#{session_name}'` (needed for `-t` group
  target).

**Preview client** (`crates/gw/src/preview.rs`, new):

- Session name: `gw-preview-<std::process::id()>` (dashboard + popup instances can
  coexist).
- On first use (lazy init): startup sweep of stale `gw-preview-*` sessions, create the
  grouped session, open a PTY via portable-pty sized to the preview Rect
  (cols × rows), spawn `tmux attach -r -t gw-preview-<pid>` in it with `TMUX`/`TMUX_PANE`
  removed from the env and `TERM=xterm-256color`. Try the same tmux binary fallback
  list as `run_tmux`.
- Feed PTY output into a `vt100::Parser` behind a mutex from a blocking reader task;
  after each read, send a notification on a `tokio::sync::watch`/`mpsc` channel.
- Expose `resize(cols, rows)` → resize both the PTY master and the vt100 parser.
- Expose `select(window_id)` → unset aggressive-resize on the previous window, set it
  on the new one, `select-window`. Track the current window to avoid redundant calls.
- `deselect()` (no agent selected / Ended view): unset aggressive-resize on the last
  window; stop rendering the live widget (client stays attached, harmless).
- On drop / TUI exit: unset aggressive-resize, `kill_session`. `destroy-unattached on`
  covers crashes (the nested client is a child process; gw death closes the PTY, the
  client dies, tmux destroys the session). A `kill -9` may leak the per-window
  aggressive-resize flag — accepted.
- Any error at any stage → mark the client dead and return; callers fall back to
  snapshots. Do not retry in a loop.

**TUI integration** (`crates/gw/src/tui.rs`):

- Add the notification channel to the `tokio::select!` loop; throttle redraws to
  ~30 fps (e.g. coalesce notifications, `Instant`-based min interval — note
  `std::time::Instant` is fine here).
- `refresh_preview()` on selection change: if live client healthy →
  `preview.select(window_id)`; else existing capture path.
- `render_preview()`: keep the rule line (window index:name header). Below it, if
  live → render `tui_term::widget::PseudoTerminal` from the parser's screen; else
  existing snapshot lines. Remove the `.dim()` on live content (it's a faithful view;
  keep dim for snapshot fallback).
- On terminal resize / preview Rect size change: `preview.resize(area.width,
  area.height - 1)`.
- Layout unchanged: 40% height, `MIN_PREVIEW_TERM_HEIGHT` 16, Ended view has no
  preview. When the preview area is hidden (small terminal), `deselect()`.

## Verification

- `cargo clippy --workspace` and `cargo test --workspace` clean after each commit.
- Manual, inside tmux with a real claude agent window:
  1. Open gw popup → preview shows the agent live, in color; agent output streams
     without keypresses.
  2. j/k across agents → preview follows instantly; no flicker to blank.
  3. Jump (enter), reopen gw → previewed window rendered at full size again.
  4. Quit gw → `tmux ls` shows no `gw-preview-*` session; previewed window back to
     normal size; `show-options -w -t <win> aggressive-resize` unset.
  5. Kill gw with SIGKILL → nested client dies, session auto-destroyed
     (`destroy-unattached`).
  6. Break the live path (e.g. temporarily point the attach argv at a bogus binary)
     → snapshot fallback renders.
