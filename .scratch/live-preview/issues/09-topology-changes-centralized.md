# 09 — Panel/agent topology changes: one snapshot, one reducer

Status: resolved

Field bug: after the gw pane is moved to another window, the panel misbehaves —
the preview card goes dead, and every keypress pins/zooms the agent's window in
the background (or zooms the user's current window when gw was joined into it).

Root cause (reproduced on the isolated server, both `break-pane` and
`join-pane` variants): `PanelIdentity` (session_id / window_id / pane_id) is
resolved once at startup from `$TMUX_PANE` and cached forever. Every downstream
judgment consumes the stale copy:

- Visibility poll compares `session_current_window` against the stale
  window_id → after a move the answer is *inverted*: gw believes it is hidden
  while the user stares at it (preview card dead), and believes it is visible
  when the user visits the old window (pins/zooms the agent window unwatched).
- Key-implies-visible then acquires on stale facts → each keypress pins the
  agent window to panel size (`ws=manual 96x14` observed) or zooms the user's
  current window when gw shares it (4 zoom flips per 6 keys observed), released
  ~500ms later by the poll — visible flapping.
- The co-located check compares against the stale panel window → the direction
  placard never engages after a move, and the zoom-loop protection of issue 08
  silently stops applying.

The fix must not be a point patch: tmux topology can change under gw in many
ways, and each judgment currently samples different, differently-stale data.

## Change inventory

Panel side:

- P1 gw pane moved to another window (`break-pane`, `join-pane`, `move-pane`)
  — the reported bug; visibility + colocated + direction all wrong.
- P2 gw pane moved into the selected agent's window — additionally must switch
  to the direction placard.
- P3 gw pane moved to another *session* — stale session_id; also
  `list-panes -s` discovery then scans the wrong session.
- P4 gw pane resized — handled (issue 07).
- P5 gw pane killed — process dies with the pane; Drop cleanup. Handled.
- P6 user client detaches — gw keeps pinning/zooming an unwatched window;
  `#{session_attached}` can pause the preview for free.
- P7 window renamed / reindexed / `swap-window` — window_id is stable; titles
  refresh on the 2s tick. No action.

Agent side:

- A1 agent pane appears — 2s discovery tick. Handled.
- A2 agent process exits, pane lives — 2s discovery tick. Handled.
- A3 selected agent's pane killed — the pinned window dies; tmux flips the
  grouped preview session to an arbitrary surviving window, the nested client
  shows it for up to 2s; releasing against the dead window errors and today
  `fail()`s the whole preview. Needs sub-second detection + tolerant release.
- A4 agent pane moved to another window — live preview keeps showing the *old*
  window (wrong content) until the 2s tick; needs release + re-acquire.
- A5 agent pane moved into gw's window — must flip live preview → placard
  (today up to 2s of issue-08-style self-zoom before the tick catches it).
- A6 agent pane moved out of gw's window — placard → live preview.
- A7 panes added/removed in the previewed window — zoom needs re-evaluation
  (1-pane windows are pinned unzoomed; gaining a 2nd pane should zoom the
  agent pane).
- A8 agent pane resized/swapped within its window — direction placard should
  track the new geometry; live preview unaffected (zoomed).

Server / combinations:

- S1 tmux server dies — all calls error; existing fail-safe degradation.
- S2 dashboard gw + popup gw (or two dashboards) preview the same agent —
  two pins fight over the window size and the losing nested client's PTY ends
  up smaller than the window: the tmux 3.7b livelock recipe from issue 07.
  Out of scope here — split to issue 10.

## Design: one snapshot, one reducer

All preview-affecting judgments derive from a single per-tick topology
snapshot; a single reducer diffs consecutive snapshots and drives the existing
ordered acquire/release transitions. No other code path queries or caches
panel/agent location.

1. `tmux::observe_topology()` — ONE round-trip:
   `list-panes -a -F '#{session_name}\t#{session_id}\t#{window_id}\t
   #{window_active}\t#{session_attached}\t#{pane_id}\t#{pane_left}\t
   #{pane_top}\t#{pane_width}\t#{pane_height}\t#{window_panes}'`,
   dropping rows whose session name starts with `gw-preview-` (grouped preview
   sessions duplicate every window; verified: `window_active` and
   `session_attached` are reported per session row).
2. `PanelIdentity` keeps only `pane_id` as the anchor (`$TMUX_PANE` is stable
   across moves, including cross-session). Session and window are read fresh
   from each snapshot: panel visible = its row's `window_active` (fixes P1—P3;
   `&& session_attached` fixes P6 if accepted).
3. The 500ms visibility tick becomes the topology tick. Gate: Polled mode and
   an agent is selected (placard direction updates need it too, not just held
   selections). Popup mode also consumes snapshots (agent existence/moves),
   with visible ≡ true.
4. Derived state per tick: `panel { session_id, window_id, visible }` +
   `agent { exists, window_id, geometry, window_panes }`. The reducer compares
   against the previous derivation and applies, in order:
   - agent gone → deselect, clear card (A3, sub-second);
   - colocated (fresh window ids equal) → release, placard (P2, A5);
   - not colocated anymore → placard → live re-acquire (A6);
   - agent window changed, or previewed window's pane count changed → release
     old pin, re-acquire (A4, A7);
   - panel visibility changed → acquire/release as today (P1);
   - placard shown → recompute direction from snapshot geometry (A8; the two
     extra `pane_geometry` calls disappear — geometry rides the snapshot).
5. Key-implies-visible becomes "run one topology tick now": acquire only ever
   happens on fresh facts, never on the optimistic flag alone. Kills the
   keypress pin/zoom flapping in every stale scenario.
6. Release/deselect tolerate vanished targets: "window/pane not found" during
   unpin/unzoom is success, not `fail()` (A3).
7. Discovery unification (fixes P3's second half): `discover::snapshot`
   consumes the same `observe_topology()` row shape, scoped to the panel's
   *fresh* session, replacing `list-panes -s`. One pane-facts query shape, one
   parser, two cadences (500ms preview reducer, 2s discovery join).

Cost: one `list-panes -a` per 500ms while a preview card is showing — same
class as today's `display-message` + occasional `pane_geometry` pair.

## Verification

- Unit: snapshot parser; reducer transition table (each inventory row above
  that the reducer owns gets a case).
- Isolated-server E2E (`repro_move_gw.py` promoted): break-pane and join-pane
  of the gw pane while previewing — expect placard/live preview to follow
  within 500ms, zero background pins (`ws=latest` on the agent window while gw
  hidden), zero zoom flips on keypresses; agent-pane kill while previewed —
  expect card cleared, no `fail()`, no leftover `ws=manual`; agent pane moved
  between windows — preview follows.

## Decisions (user, 2026-07-16)

- P6: yes — pause the preview while the panel's session has no attached client
  (`visible = window_active && session_attached`).
- Step 7 discovery unification: in scope for this issue.
- S2 multi-instance pin fight: split to issue 10 (owner-lock proposal recorded
  there).

## Comments

Implemented one non-preview topology snapshot shape for preview and discovery,
centralized fresh panel/agent transitions in a tested reducer, and made vanished
window or pane targets a successful release while preserving the parked-PTY order.
