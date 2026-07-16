# 07 — tmux redraw livelock via undersized nested client + window-size residue

Status: resolved

Field bug: with gw running, typing `tmux split-window` inside a pane of the
previewed window hangs the entire tmux server (100% CPU livelock in
`screen_redraw_screen -> tty_draw_line`, tmux 3.7b). Root cause isolated on an
isolated server (no gw involved):

- Trigger: a client whose tty is SMALLER than the window it is viewing, when
  that window's layout changes (split). Read-only/read-write and
  ignore-size/participating make no difference; equal or larger client tty
  never hangs.
- gw arms this exactly when hidden: release drops the pin, the user window
  grows back to e.g. 200x50, while the nested preview client keeps viewing it
  through a 176x14 PTY.

Separate confirmed residue: the release path (`set-option -uw window-size` then
`resize-window -A`) leaves the window marked `window-size manual` — `-A` re-marks
it. Verified fix: run `set-option -uw window-size` once more AFTER
`resize-window -A`; the window then reports `window-size latest` and follows
client resizes again.

## Fix design (verified empirically on tmux 3.7b, keep exactly this shape)

New invariant: **the nested client PTY is never smaller than the window it
views.** Two PTY regimes:

- *panel size* — the preview inner area; used only while a selection is held
  (window is pinned to the same size, so equality holds).
- *parked size* — constants `PARKED_COLS: u16 = 800`, `PARKED_ROWS: u16 = 240`;
  used whenever no selection is held (covers any realistic user window).

Concrete changes, all in `crates/gw/src/preview.rs` + `crates/gw-core/src/tmux.rs`:

1. **Attach flags**: `attach -f read-only` → `attach -f read-only,ignore-size`.
   With ignore-size the parked-size client no longer participates in window
   sizing (verified: user window stays at the user's size while the nested PTY
   is 800x240), and manual pinning still wins while visible. Update the
   command-construction regression test (name it for both properties).
2. **Spawn at parked size**: `start_client` opens the PTY at parked size (the
   vt100 parser stays at panel size — it is only rendered while a selection is
   held). This closes the attach-time race where the client briefly views the
   user's full-size current window through a panel-sized tty.
3. **Acquire order** (in `acquire_selection`, which now needs access to the
   live client's master PTY): select-window → conditional zoom → pin window to
   panel size → resize PTY master to panel size → feed `\x1bc` (RIS) to the
   parser to drop stale parked-size content. Window never exceeds PTY at any
   intermediate step.
4. **Release order** (paths where the nested client stays alive: select-switch,
   deselect, set_visible(false)): resize PTY master to parked size FIRST, then
   conditional unzoom, then size release. Growing first keeps the invariant
   when the unpinned window snaps back to the user's size.
5. **Teardown order** (`fail()` and `Drop`): kill the preview session FIRST
   (this detaches/kills the nested client, removing the livelock ingredient),
   THEN release zoom/size borrows (those tmux commands target the user
   session's window and do not need the preview session). No PTY grow needed on
   these paths.
6. **Size-release residue**: the size release becomes unset `window-size` →
   `resize-window -A` → unset `window-size` again. Fold all three into one
   tmux.rs helper (replacing the current two-call sequence at the call site);
   update its command-construction tests.
7. **`Preview::resize`**: track panel size separately from the PTY size. Resize
   the PTY (and re-pin the window) only while a selection is held; while
   unselected/hidden only update the stored panel size and parser size (PTY
   stays parked).

No other behavior changes. `cargo test` + `cargo clippy --all-targets -- -D
warnings` must pass.

## Verification (already scripted)

Isolated-server E2E (`TMUX_TMPDIR=/private/tmp/gwiso`, never the default
socket): with gw running and previewing a 2-pane window, typing
`tmux split-window -v` through the user client's PTY must complete instantly in
BOTH visible and hidden states, server CPU stays idle, and after q-quit the
window reports `window-size latest` and follows client resizes.

## Comments

Parked the nested PTY at 800x240 whenever no selection is held, added
`ignore-size`, reordered acquire/release/teardown to preserve the size invariant,
and made window-size release end with a second unset after `resize-window -A`.
