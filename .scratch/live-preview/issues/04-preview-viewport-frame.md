# 04 — Make the preview read as a viewport, not a real pane

Status: resolved

Design rationale: real tmux panes are edge-to-edge rectangles separated by straight
lines. The preview currently mimics that shape (top rule + full-bleed content), so it
reads as a real pane. A rounded, dimmed, horizontally inset card cannot be a tmux
pane — the shape alone signals "viewport". No textual "preview" label.

Deliverables (all in `render_preview`, `crates/gw/src/tui.rs`):

- Replace the top rule line with a full `Block`: `BorderType::Rounded`, border style
  dim dark-gray, title ` {window_index}:{window_name} ` top-left (same info as
  today's rule head, no extra wording).
- Inset the block horizontally: render at `x + 1`, `width - 2` of the preview area
  (full preview height). Content renders in the block's inner Rect.
- Live PTY/parser size follows the inner Rect (existing resize plumbing; the source
  window now reflows to the inner size — expected).
- Do not render the terminal cursor in the live widget — check tui-term 0.3's
  `PseudoTerminal` API for cursor visibility/style control and switch it off; if the
  API offers no way to hide it, say so in your final message instead of hacking
  around it.
- Snapshot fallback renders inside the same block, content keeps its dim style.
- `MIN_PREVIEW_TERM_HEIGHT` and the 40% layout stay unchanged.

## Comments

Replaced the full-bleed rule with an inset rounded viewport block, sized live and
snapshot content to its inner Rect, and hid the live terminal cursor via tui-term.
