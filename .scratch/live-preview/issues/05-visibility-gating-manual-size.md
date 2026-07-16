# 05 — Visibility gating + manual window sizing

Status: resolved

Fixes two field-reported bugs:

1. **Preview freezes**: when the previewed window is also current in the user's
   session, client arbitration keeps the window at the user's size and the nested
   client shows a clipped, near-static top-left viewport.
2. **Zoom held while the user works elsewhere**: dashboard-mode gw borrows zoom +
   window size for as long as it runs, even when nobody is looking at the panel.

Spec: `.scratch/live-preview/spec.md` — rewritten sizing bullet under "Environment
facts" (mechanics verified on tmux 3.7b) and decisions 1/1b.

Deliverables:

- **Manual sizing replaces aggressive-resize.** tmux helpers:
  `resize-window -t <window_id> -x <cols> -y <rows>` (pin) and release =
  `set-option -uw -t <window_id> window-size` then existing `resize-window -A`.
  Remove the aggressive-resize helper and its call sites/tests.
- Acquire order: `select-window` → zoom (existing conditions) → pin to the current
  preview inner size. `Preview::resize` must re-pin the selected window when live
  (the window no longer follows the client size).
- Release order: conditional unzoom (existing) → unset `window-size` →
  `resize-window -A`. Same four release paths (select-away, deselect, fail, Drop).
- **Visibility gating** (dashboard mode only; popup mode `GW_POPUP=1` is always
  visible; if `TMUX_PANE` is unset treat as always visible):
  - Visible = `display-message -p -t $TMUX_PANE '#{window_active}'` == 1.
  - Poll on a 500ms tokio interval in the select loop, only while a selection is
    held or wanted. Any received key event also implies visible (keys only arrive
    when focused) — mark visible immediately without waiting for the poll.
  - Visible→hidden: release the selection but remember it (do not touch the nested
    client or session); render side falls back to snapshot (nobody is watching).
  - Hidden→visible: re-acquire the remembered selection.
- Errors on these paths follow the established pattern: best-effort + tui.log.
- Command-construction regression tests updated (aggressive-resize ones removed,
  manual-size ones added).

## Comments

Replaced client size arbitration with explicit manual window-size pins and added
dashboard visibility polling that releases and re-acquires zoom and size borrows
without restarting the nested client.
