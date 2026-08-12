# gw — Design (v1)

A tmux-native TUI panel that shows coding agents across tmux sessions, their live status, and lets you jump to, launch, and resume agents. Rust, from scratch. Vocabulary in [CONTEXT.md](../CONTEXT.md); load-bearing decisions in [docs/adr/](adr/).

## Model

- **Discovery-based identity** — an Agent is any pane across the tmux server whose process tree matches a provider's process rules. No registry; panes are the source of truth. ([CONTEXT.md](../CONTEXT.md))
- **One row per pane** — providers may host more than one native Session, but the panel still renders one Agent per pane. Amp and Pi rows follow their interactive TUI's foreground Session; Amp background threads and non-TUI modes are outside the integration boundary.
- **Hook-driven status, no daemon** — plugins normalize provider hook payloads into unified events; the core appends them to per-session JSONL logs; the TUI derives dynamic status by pure replay + fs watch, with Idle as the fallback before a discovered process emits its first event. (ADR 0001)
- **Providers as external executables** — `gw-provider-<id>` binaries speaking a pure-translator protocol (`manifest` / `normalize`); the core owns all I/O. (ADR 0002)
- **Statuses** (sort order = priority): Attention (approval > question) / Error / Stale / Working / Done / Idle. Done means the turn ended; it never decays. ([CONTEXT.md](../CONTEXT.md))
- **Session vs Agent**: an ended Session (pane gone, native session id in the log) is resumable from a secondary view.

## Event ↔ pane correlation

The hook process is a child of the agent process. The core walks the ppid chain to the provider process (matching via the plugin's process rules), resolves its tty to a pane id, and records pane id + pid + native session id + cwd with the events. The panel joins live pane scans to the log by provider pid. If the pid does not match, the Agent is Idle until the current process emits a hook event; pane id and cwd describe location but never transfer Session state between processes.

## UX

- **Panel** runs in a persistent pane (dashboard), `tmux display-popup`
  (primary posture), or a terminal outside tmux while a tmux server is
  available. Inside tmux, a jump switches the existing client; outside tmux,
  the Panel first restores the terminal and then attaches it to the Agent's
  exact tmux session/window/pane. Detaching that client resumes the same Panel.
  A popup exits after a jump.
- **List columns**: provider, status (+ duration in that status), window/pane, detail (one-line status context: current activity · task, awaited approval, turn summary, failure reason), cwd (abbreviated), git branch. Running subagents render as dim indented sub-lines under their agent's row (`↳ type · model · task · age`).
- **Activity**: compact event timeline of the selected agent (recent turns, tool activity, attention, subagents from its Event Log; display only — the panel never touches the agent's window).
- **Keys (v1)**: `j/k` or arrows move, `Enter` jumps, `n` launches (pick provider → new window in the panel's cwd → jump), `r` toggles the resumable-sessions view (`Enter` = new window running the provider's resume command), `tab` toggles current/global views, `a` selects the next Attention agent, `?` opens the keyboard-shortcuts page, and `Esc`/`Ctrl-C` quit.
- **Notifications**: `gw hook` itself fires a desktop notification through macOS `osascript` after writing a configured event. Other platforms currently skip desktop notifications.
- **Setup**: `gw setup` filters discovered plugins to agent CLIs available on the local `PATH`, displays the matching providers and target files, and requires explicit confirmation before installation (`--yes` is the non-interactive opt-in). Surgical merge only — preserve unrelated keys and formatting (claude's `settings.json` mixes user config with hooks), back up before writing, idempotent, reversible via `gw setup --remove`. Removal still considers every discovered plugin so an integration can be cleaned up after its agent CLI is uninstalled. Providers may also declare a whole managed integration file: the core creates, hashes, upgrades, and removes it only while its ownership marker and body hash prove it remains unmodified. Amp, OpenCode, and Pi use this for their TypeScript observer integrations. Grok uses a dedicated hook file under `~/.grok/hooks/`. The panel does not report setup health; an uninstrumented provider remains Idle.

## Plugin protocol (v1 sketch)

- Discovery: `gw-provider-*` on PATH and in `~/.config/gw/providers/bin/`.
- `manifest` → JSON: protocol version, provider id, display label/color, process match rules (argv basename patterns plus excluded args/sequences), launch command, resume command template (`{session_id}`, `{cwd}`), hook install specs (target files, entries to merge), and optional managed integration files.
- `normalize` → stdin: raw hook payload JSON; stdout: zero or more unified events (JSONL): `session_focus`, `session_start`, `turn_start`, `turn_end`, `turn_error`, `attention` (kind: approval | question), `heartbeat`, `subagent_start`, `subagent_end`, `session_end` — each with native session id; see `protocol.md` for the per-kind optional fields. `session_focus` changes correlation without changing status.
- Official plugins: `gw-provider-claude`, `gw-provider-codex`, `gw-provider-amp`, `gw-provider-opencode`, `gw-provider-pi`, `gw-provider-grok` (same workspace, same protocol, no fast path).

## Storage

- Event logs: `~/.local/state/gw/sessions/<date>-<cwd>-<provider>-<session-hash>.jsonl`, append-only, `O_APPEND` single-write per event; the panel removes logs of dead sessions older than seven days on startup.
- Config (optional, TOML): `~/.config/gw/config.toml` — currently `notify`, `[panel] default_view`, and `[debug] hooks` (see `config.md`); stale threshold, plugin dir overrides, keybindings later.

## Crate layout

Cargo workspace:

- `gw-core` — domain: discovery, event model, Session interpretation (pure Status, Subagent, and Activity replay), correlation, log store, plugin client, tmux shell-out wrapper.
- `gw` — the binary: CLI (`panel`, `hook`, `setup`), ratatui TUI (fullscreen alt-screen; `TuiEvent`/`AppEvent` split, single `tokio::select!` loop, frame coalescing — patterned after codex-rs's tui architecture).
- `gw-plugin-protocol` — serde types for manifest/events, published for Rust plugin authors (the protocol itself is JSON-over-CLI; non-Rust plugins just follow the spec).
- `gw-provider-claude`, `gw-provider-codex`, `gw-provider-amp`, `gw-provider-opencode`, `gw-provider-pi`, `gw-provider-grok` — official plugin binaries.

## Backlog

- Kill selected agent from the panel (`x`, with confirm).
- Last-message summary line per agent (from `turn_end` payload).

## Transition note

The predecessor tool installed hooks invoking `gw hook <provider>` — the same command shape. Installing this binary first on PATH takes over those hook calls cleanly; remove the old binary to avoid schema confusion.
