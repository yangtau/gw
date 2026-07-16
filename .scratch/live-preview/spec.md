# Live preview via nested read-only tmux client

Replace the capture-pane snapshot preview with a live, full-fidelity view of the
selected Agent's window. See `docs/adr/0003-live-preview-nested-client.md` for the
decision record and `CONTEXT.md` ("Preview") for the term.

## Mechanism

A grouped tmux session shares the window list with the user's session. gw attaches a
**read-only nested client** to that grouped session inside a PTY it owns, sized to the
preview area, and renders the client's screen into the preview Rect via an embedded
terminal emulator. Because the nested client is the only session for which the
previewed window is current, gw can switch it independently. gw manually pins that
window to the preview PTY's size while the Panel is visible, so the agent program
re-renders for it. This is deliberate and accepted (see ADR).

Environment facts (verified):

- tmux 3.7b on the dev machine; grouped sessions, manual `resize-window`,
  `attach -f read-only`, `destroy-unattached` all available.
- `attach -r` is an alias for `-f read-only,ignore-size`; the preview uses only
  `read-only` so its client participates in window sizing.
- tui-term 0.3.4 (actively maintained) requires ratatui 0.30 + crossterm 0.29;
  gw is currently on ratatui 0.29 + crossterm 0.28 → prerequisite upgrade.
- Client-based sizing (`window-size latest`, with or without `aggressive-resize`)
  is arbitration: whenever the previewed window is also current in the user's
  session, the user's client wins and the nested client gets a clipped top-left
  viewport that barely updates — the preview looks frozen. Verified escape hatch:
  `resize-window -x <cols> -y <rows>` pins the window manually and beats any client
  arbitration (user keystrokes included); `set-option -uw window-size` followed by
  `resize-window -A` restores client sizing and snaps back to the attached client.
- copy-mode is pane-level shared state → the preview must never enter it.
- zoom (`resize-pane -Z`) is window-level shared state, but manageable with the same
  set/restore discipline as manual window sizing. Verified: zoom works from a plain
  command (no client context); it survives the manual window resize (the zoomed pane
  tracks the new window size); `select-pane` onto the zoomed pane itself
  keeps zoom (jump lands there), selecting another pane auto-unzooms; restore must be
  conditional on `#{window_zoomed_flag}` (toggle only if still zoomed).

## Decisions (settled, do not relitigate)

1. **Resize policy: fully accepted, pinned deterministically.** All agent windows
   get live preview, including the window under the popup. The previewed window is
   pinned to the preview size with `resize-window -x -y` (manual sizing — no client
   arbitration, no flap), restored on release via `set -uw window-size` +
   `resize-window -A`. Supersedes the original aggressive-resize approach.
1b. **Visibility gating.** The preview's shared-state borrows (manual window size,
   zoom) are held only while the Panel is visible: always in popup mode; in
   dashboard mode only while the Panel's own window is its session's current window
   (polled, plus any received key event implies visible). On visibility loss the
   selection is released but remembered; on regain it is re-acquired. The nested
   client and grouped session stay alive throughout.
2. **Dependency path:** upgrade workspace to ratatui 0.30 + crossterm 0.29 as a
   separate prerequisite commit, then use tui-term 0.3.4 (vt100 feature) +
   portable-pty + vt100.
3. **Topology:** one persistent grouped session + one persistent nested client per
   gw process; selection changes are `select-window` on the preview session only.
4. **Interaction: pure read-only.** No input is ever written to the nested client's
   PTY. No in-preview scrolling. Enter still jumps to the pane (existing behavior).
5. **Fallback:** if the live client cannot be established or dies, fall back to the
   existing `tmux::capture` snapshot path (keep that code).
6. **Zoom for pane-level fidelity** (supersedes the original "never zoom" stance):
   while previewing a multi-pane window that is not already zoomed, zoom the Agent's
   pane; restore on release (select-away, deselect, jump, exit, failure) only if the
   window is still zoomed. Windows the user zoomed themselves are left untouched.
   Jump releases the preview first, so dashboard mode never holds zoom on a window
   the user is working in. A `kill -9` may leak zoom state — accepted (`prefix+z`).

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
  `new-session -d -s <name> -t <current session>` then
  `set-option -t <name> status off`. `destroy-unattached` is enabled by the nested
  client only after it attaches, because enabling it on a never-attached session
  destroys that session immediately.
- `preview_select_window(session, window_id)` → `select-window -t <session>:<window_id>`.
- `pin_window_size(window_id, cols, rows)` →
  `resize-window -t <window_id> -x <cols> -y <rows>`.
- `unset_window_size(window_id)` →
  `set-option -uw -t <window_id> window-size`; follow it with
  `resize-window -A -t <window_id>` on release.
- `panel_identity(pane_id)` → list all panes with session/window identity and select
  the matching row outside `gw-preview-*`; `session_current_window(session_id)` →
  `display-message -p -t <session_id> '#{window_id}'`.
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
  (cols × rows), spawn
  `tmux attach -f read-only -t gw-preview-<pid> ; set-option -t gw-preview-<pid> destroy-unattached on`
  in it (`;` is a tmux argv element, not shell syntax), with `TMUX`/`TMUX_PANE`
  removed from the env and `TERM=xterm-256color`. Try the same tmux binary fallback
  list as `run_tmux`.
- Feed PTY output into a `vt100::Parser` behind a mutex from a blocking reader task;
  after each read, send a notification on a `tokio::sync::watch`/`mpsc` channel.
- Expose `resize(cols, rows)` → resize both the PTY master and the vt100 parser, then
  re-pin the selected window when live.
- Expose `select(window_id)` → release the previous selection, `select-window`, apply
  conditional zoom, then pin the new window to the current preview size. Track the
  wanted and currently held selections separately.
- Release conditional zoom, then best-effort unset `window-size` and
  `resize-window -A -t <window_id>` so the window snaps back immediately.
- `deselect()` (no agent selected / Ended view) releases and forgets the selection;
  visibility loss releases it but remembers the target. In both cases the nested
  client stays attached.
- On drop / TUI exit: release the selection, `kill_session`. `destroy-unattached on`
  covers crashes (the nested client is a child process; gw death closes the PTY, the
  client dies, tmux destroys the session). A `kill -9` may leak manual window size or
  zoom state — accepted.
- Any error at any stage → mark the client dead and return; callers fall back to
  snapshots. Do not retry in a loop. Log degradation errors to the append-only
  `<state dir>/tui.log` without writing to stderr from the raw-mode TUI.

**TUI integration** (`crates/gw/src/tui.rs`):

- Add the notification channel to the `tokio::select!` loop; throttle redraws to
  ~30 fps (e.g. coalesce notifications, `Instant`-based min interval — note
  `std::time::Instant` is fine here).
- In dashboard mode, resolve the Panel's non-preview session id and window id before
  creating the preview session, then poll that session's current `#{window_id}` every
  500 ms while a selection is held or wanted; any key marks it visible immediately.
  Startup identity failure, popup mode, and environments without `TMUX_PANE` are
  always visible. Runtime poll failure logs and hides the Preview.
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
     normal size; `show-options -w -t <win> window-size` unset.
  5. Kill gw with SIGKILL → nested client dies, session auto-destroyed
     (`destroy-unattached`).
  6. Break the live path (e.g. temporarily point the attach argv at a bogus binary)
     → snapshot fallback renders.
