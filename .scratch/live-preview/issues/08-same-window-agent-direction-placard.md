# 08 — No preview for same-window agents; show a direction arrow instead

Status: resolved

Field bug: an agent running in the same window as the gw panel (dashboard mode)
sends the preview into a zoom loop — acquire zooms the agent pane inside the
panel's own window, which covers the panel and fights it endlessly.

Decision (user): gw does NOT preview agents that share the panel's window — no
live client, no snapshot. The preview card instead shows a large arrow pointing
toward the agent pane's position relative to the panel pane (pure UI, no text).

## Design

Co-located = dashboard mode only: `PreviewVisibility::Polled` (panel identity
resolved) AND selected agent's `pane.window_id` equals the panel's window id.
Popup mode (`PreviewVisibility::Always`) keeps full live preview — a popup
overlays the window, zooming beneath it is correct and E2E-asserted.

1. `PanelIdentity` gains `pane_id` (it is resolved from `$TMUX_PANE` already —
   store it). `PreviewVisibility::Polled` carries it through.
2. New tmux helper `pane_geometry(pane_id) -> PaneGeometry { left, top, cols,
   rows }` via `display-message -p -t '<pane_id>'` printing
   `#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}` (tab-separated,
   same style as the other helpers). Command-construction + parse tests.
3. `refresh_preview` (crates/gw/src/tui.rs): when the selected agent is
   co-located, call `live_preview.deselect()`, clear `snapshot_preview`, query
   both panes' geometry, compute the direction, and cache it in a new
   `Option<Direction>` field on the app. Any geometry error → cache `None` and
   `tui_log::error`. When not co-located, clear the cache and keep today's
   behavior unchanged.
4. Direction math (pure function + unit tests): compare pane centers. Terminal
   cells are ~2:1 tall, so weight the vertical delta: horizontal wins when
   `|dx| >= |dy| * 2`, otherwise vertical. dx > 0 → Right, dx < 0 → Left,
   dy > 0 → Down, dy < 0 → Up. Equal centers (should not happen) → Right.
5. `render_preview`: co-located agents render the same viewport card (rounded
   dim DarkGray border, ` {window_index}:{window_name} ` title) with the arrow
   centered in the inner area, drawn in the card's frame style (DarkGray, dim),
   nothing else:
   - Left:  `◄◄◄` followed by a `─` shaft; shaft length = inner width / 3,
     clamped to [4, 24]. One line, centered both axes.
   - Right: `─` shaft then `►►►`, same sizing.
   - Up: three `▲` rows? No — one `▲` row on top of a `│` shaft column; shaft
     height = inner height / 3, clamped to [2, 8]; the chevron row is `▲▲▲`
     (three glyphs side by side), shaft rows are single `│` centered under the
     middle chevron. Down: mirrored (`│` shaft rows, then `▼▼▼`).
   - Extract the arrow-lines builder as a pure function with unit tests
     (left/right/up/down, clamping).
6. The 500ms visibility poll and key-implies-visible logic stay as-is; they are
   irrelevant while no selection is held (co-located never selects).

No other behavior changes. `cargo test` + `cargo clippy --all-targets -- -D
warnings` must pass.

## Comments

Added unambiguous panel pane identity and geometry queries, suppress same-window
dashboard previews, and render a centered directional arrow placard with tested
center weighting and shaft clamping.

UI refinement (user request): the static glyph arrow became an animated comet
rail — up to five chevrons (`❮`/`❯`/`▲`/`▼`) with a brightness pulse sweeping
toward the agent every 90ms, colors lerped from dim gray to the provider's
accent (truecolor), head bolded — plus a steady accent bar hugging the card
border edge that faces the agent. Animation runs off a `pulse_tick` interval
gated on the placard being shown; phase derives from a monotonic epoch so no
per-frame state. Pure helpers (`pulse_intensity`, `chevron_rail`, `edge_bar`)
are unit-tested.
