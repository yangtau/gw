# 06 — Visibility query breaks once the preview session exists

Status: resolved

Field bug in issue 05's visibility gating, found by E2E: the poll uses
`display-message -p -t $TMUX_PANE '#{window_active}'`. A pane target resolves to
"the best session containing the window" — once the grouped `gw-preview-*` session
exists, tmux may resolve against *it* (whose current window is the previewed one),
so the panel's own window reads inactive. Observed sequence: first poll → visible →
acquire succeeds (pin+zoom applied), second poll → `window_active` flips to 0 →
released. Net effect: live preview turns itself off within a second in dashboard
mode; borrows never stick.

Verified fix shape (tmux 3.7b):

- Resolve the panel's identity once at startup, deterministically:
  `list-panes -a -F '#{session_name}\t#{session_id}\t#{window_id}\t#{pane_id}'`,
  pick the row whose pane_id == `$TMUX_PANE` and whose session_name does NOT start
  with `gw-preview-` (a pane linked into grouped sessions yields one row per
  session). Cache that `session_id` and `window_id`. This also survives stale or
  concurrent gw instances' preview sessions, unlike any `-t <pane>` inference.
- Poll: `display-message -p -t '<session_id>' '#{window_id}'` (bare session-id
  target — unambiguous, rename-proof) and compare with the cached window id.
  Verified: returns the user session's current window correctly while the preview
  session exists and tracks user window switches.
- Resolution failure at startup → fall back to always-visible (same as TMUX_PANE
  unset). Poll failure at runtime → treat as hidden and log to tui.log (fail-closed:
  borrows must not outlive certainty).

Deliverables:

- Replace `pane_window_active` with the two helpers above (`panel_identity()` /
  `session_current_window(session_id)` or similar); update `PreviewVisibility` to
  carry the cached ids; adjust call sites and regression tests.
- No other behavior changes.

## Comments

Resolved the Panel's user-session identity before creating a preview session and
poll the cached session id's current window, failing open only during startup and
failing closed with logging after runtime query errors.
