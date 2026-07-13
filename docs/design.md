# gw — Design (v1)

A tmux-native TUI panel that shows every coding agent running in the current tmux session, their live status, and lets you jump to, launch, and resume agents. Rust, from scratch. Vocabulary in [CONTEXT.md](../CONTEXT.md); load-bearing decisions in [docs/adr/](adr/).

## Model

- **Discovery-based identity** — an Agent is any pane in the current session whose process tree matches a provider's process rules. No registry; panes are the source of truth. ([CONTEXT.md](../CONTEXT.md))
- **Hook-only status, no daemon** — plugins normalize provider hook payloads into unified events; the core appends them to per-session JSONL logs; the TUI derives status by pure replay + fs watch. (ADR 0001)
- **Providers as external executables** — `gw-provider-<id>` binaries speaking a pure-translator protocol (`manifest` / `normalize`); the core owns all I/O. (ADR 0002)
- **Statuses** (sort order = priority): Attention (approval > question) / Error / Stale / Working / Done / Idle / Unknown. Done means the turn ended; it never decays. ([CONTEXT.md](../CONTEXT.md))
- **Session vs Agent**: an ended Session (pane gone, native session id in the log) is resumable from a secondary view.

## Event ↔ pane correlation

The hook process is a child of the agent process. The core walks the ppid chain to the provider process (matching via the plugin's process rules), resolves its tty to a pane id, and records pane id + pid + native session id + cwd with the events. The panel joins live pane scans against the log from both directions. Jumping resolves strongest-to-weakest: recorded pid alive and still a provider process → its tty's pane; recorded pane still hosting a provider process; heuristics (unique provider pane with same cwd).

## UX

- **Panel** runs identically in a persistent pane (dashboard) or `tmux display-popup` (primary posture); after a jump the popup instance exits. Scope: current session only.
- **List columns**: provider, status (+ duration in that status), window/pane, detail (one-line status context: current activity · task, awaited approval, turn summary, failure reason), cwd (abbreviated), git branch.
- **Preview**: read-only `capture-pane` tail of the selected agent (display only, never used for status).
- **Keys (v1)**: `j/k` move, `Enter` jump, `n` launch (pick provider → new window in the panel's cwd → jump), `r` resumable-sessions view (`Enter` = new window running the provider's resume command), `tab` jump to next Attention agent, `q` quit.
- **Notifications**: `gw hook` itself fires a desktop notification (macOS `osascript`) + terminal bell when writing an attention event — global notifications without a daemon.
- **Setup**: `gw setup` installs hooks into provider global configs for every discovered plugin. Surgical merge only — preserve unrelated keys and formatting (claude's `settings.json` mixes user config with hooks), back up before writing, idempotent, reversible via `gw setup --remove`. The panel banners providers that are discovered but uninstrumented.

## Plugin protocol (v1 sketch)

- Discovery: `gw-provider-*` on PATH and in `~/.config/gw/providers/bin/`.
- `manifest` → JSON: protocol version, provider id, display label/color, process match rules (argv basename patterns), launch command, resume command template (`{session_id}`, `{cwd}`), hook install spec (target files, entries to merge).
- `normalize` → stdin: raw hook payload JSON; stdout: zero or more unified events (JSONL): `session_start`, `turn_start`, `turn_end`, `turn_error`, `attention` (kind: approval | question), `heartbeat`, `session_end` — each with native session id; see `protocol.md` for the per-kind optional fields.
- Official plugins: `gw-provider-claude`, `gw-provider-codex` (same workspace, same protocol, no fast path).

## Storage

- Event logs: `~/.local/state/gw/sessions/<sha256(provider:native_session_id)>.jsonl`, append-only, `O_APPEND` single-write per event; retention sweep on panel start (drop logs of dead sessions older than N days).
- Config (optional, TOML): `~/.config/gw/config.toml` — stale threshold, plugin dir overrides, keybindings later.

## Crate layout

Cargo workspace:

- `gw-core` — domain: discovery, event model, status derivation (pure), correlation, log store, plugin client, tmux shell-out wrapper.
- `gw` — the binary: CLI (`panel`, `hook`, `setup`), ratatui TUI (fullscreen alt-screen; `TuiEvent`/`AppEvent` split, single `tokio::select!` loop, frame coalescing — patterned after codex-rs's tui architecture).
- `gw-plugin-protocol` — serde types for manifest/events, published for Rust plugin authors (the protocol itself is JSON-over-CLI; non-Rust plugins just follow the spec).
- `gw-provider-claude`, `gw-provider-codex` — official plugin binaries.

## Backlog

- Kill selected agent from the panel (`x`, with confirm).
- Last-message summary line per agent (from `turn_end` payload).

## Transition note

The predecessor tool installed hooks invoking `gw hook <provider>` — the same command shape. Installing this binary first on PATH takes over those hook calls cleanly; remove the old binary to avoid schema confusion.
