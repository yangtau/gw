# 03 — Zoom the Agent's pane during preview

Status: resolved

Spec: `.scratch/live-preview/spec.md` — decision 6 and the zoom bullet under
"Environment facts" (all mechanics there are empirically verified on tmux 3.7b).

Deliverables:

- tmux helpers: query `#{window_panes}` and `#{window_zoomed_flag}` for a window;
  `resize-pane -Z -t <pane>` toggle.
- `Preview::select` gains the Agent's pane id: after aggressive-resize +
  select-window, if the window has >1 pane and is not zoomed, zoom the Agent's pane
  and remember that we did (window id + zoomed-by-us flag).
- Release path (select-away, deselect, fail, Drop): if zoomed-by-us and
  `#{window_zoomed_flag}` is still 1, toggle unzoom — before unsetting
  aggressive-resize and `resize-window -A`. Best-effort, errors to tui.log.
  Never unzoom a window we did not zoom.
- `tui.rs::activate` (jump): release the preview (deselect) before `tmux::focus`,
  so neither zoom nor aggressive-resize is held on a window the user just entered
  (matters in dashboard mode where gw keeps running).
- Command-construction regression tests in the style of the existing ones.

## Comments

Added conditional Agent-pane zoom with ownership tracking and conditional restore,
including jump-before-focus release and tmux command/parser regression coverage.
