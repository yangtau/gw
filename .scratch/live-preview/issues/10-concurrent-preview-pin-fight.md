# 10 — Concurrent gw previews fight over the pinned window size

Status: open

Split from issue 09 (S2). When two gw instances hold a live preview of the
same agent window at once (dashboard + popup, or two dashboards in different
sessions), each pins the window to its own panel viewport via
`window-size manual` + `resize-window`. Last writer wins; the loser's nested
client PTY is then smaller than the window it views — exactly the tmux 3.7b
redraw-livelock recipe documented in issue 07. Even without the livelock, the
pins flap the window size.

Proposed direction (not yet designed in detail): an owner lock as a tmux
window user option, e.g. `@gw-preview-owner <pid>` set before acquire and
cleared on release. A gw instance that finds a live foreign owner (pid exists)
falls back to snapshot mode for that agent instead of acquiring. Stale owners
(dead pid) are reclaimed.

Open questions: takeover semantics (should a popup preempt a dashboard?),
cleanup on SIGKILL (stale option until reclaim), and whether the lock should
be per-window or per-agent-pane.
