# Live preview via a nested read-only tmux client

`capture-pane` snapshots are plain text, hard-wrapped at the source pane's width, and — fundamentally — TUI output is only correct at the size it was rendered for, so no snapshot-based preview can faithfully show an agent at the preview area's size. We instead run a nested read-only tmux client: a grouped session (`gw-preview-<pid>`) attached inside a PTY owned by the panel, rendered into the preview area through an embedded terminal emulator (tui-term/vt100). The previewed window is pinned to the preview area's size (`resize-window -x -y`, manual sizing — client-based `window-size` arbitration proved unreliable whenever the user's client also displayed the window), so the agent program itself re-renders at preview size — full color, live, correctly reflowed. The pin, and the pane zoom used for multi-pane windows, are held only while the panel is actually visible and restored on release.

The accepted trade-off: the source pane experiences a real resize (SIGWINCH redraw, possible scrollback reflow artifacts) while previewed and again when the preview moves away. `capture-pane` remains only as a degraded fallback when the live client cannot be established.

One hard constraint shapes the client lifecycle: tmux 3.7b livelocks the whole server (100% CPU redraw loop) whenever a client's tty is smaller than the window it is viewing and that window's layout changes. The nested client therefore attaches with `ignore-size` and its PTY is never smaller than the viewed window — pinned-panel size while the preview is visible, a parked oversize otherwise.

## Considered options

- **`capture-pane -e -J` + ANSI parsing**: colors and reflow of joined lines, zero side effects — but still a polled snapshot laid out for the source size; cannot re-render TUI frames. Kept as the fallback path.
- **Positioned `display-popup` running `attach -r`**: popups cannot nest, and the panel's primary posture is inside a popup.
