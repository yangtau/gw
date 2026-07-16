# ADR 0004: Activity timeline from the Event Log replaces the live preview

## Status

Accepted (2026-07-16). Supersedes ADR-0003.

## Context

The nested read-only client (ADR-0003) achieved full-fidelity previews by making
the previewed window really adopt the preview's size. In daily use the accepted
trade-off proved too intrusive: agent windows visibly resize and redraw
(SIGWINCH) whenever the panel looks at them or away, scrollback reflows, pane
zoom is toggled behind the user's back, and the grouped `gw-preview-*` session
leaks into session lists and window pickers. The tmux 3.7b livelock guard (the
client PTY must never be smaller than the viewed window) also kept a class of
tmux-version-specific risk permanently in the codebase. All of this bought
pixels the user can see by just jumping to the pane — which the panel makes a
one-keystroke action.

## Decision

The panel area shows an **activity timeline** instead: the selected Agent's
recent normalized events (turns, tool heartbeats, attention, subagent
lifecycle) rendered from the Event Log — the same source that drives Status,
consistent with ADR-0001's hook-only stance. Zero tmux side effects.

The live preview is **deleted, not flagged off**: the nested client, the
window pin/zoom lifecycle, the `capture-pane` fallback (which existed only to
serve the preview), and the preview command cluster in the tmux adapter all go.
The mechanism remains recoverable from git history, and this ADR records what
to weigh before bringing it back.

## Considered options

- **Keep the live preview behind a default-off config flag**: preserves a
  quick escape hatch, but two display implementations would coexist untested
  in combination, and the livelock risk and tmux side effects stay in the tree.
- **`capture-pane` snapshot as the display**: zero side effects, but it is
  hard-wrapped text laid out for the source pane's width — the exact fidelity
  problem ADR-0003 set out to solve, now without the structure the Event Log
  already provides.

## Consequences

- No tmux side effects from the panel at all: agent windows are never resized,
  zoomed, or grouped; the livelock constraint disappears from the codebase.
- The panel loses visual fidelity: the timeline shows what the Agent *did*,
  not what its screen looks like. Seeing the screen means jumping to the pane.
- `portable-pty`, `vt100`, and `tui-term` dependencies drop from the TUI.
- The timeline is a pure function of the event log tail, testable like the
  rest of the status pipeline.
- Reinstating a live preview means re-accepting the window-resize intrusion or
  finding a mechanism that avoids it (none was found in ADR-0003's search).
