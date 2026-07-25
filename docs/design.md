# gw — Design (v1)

A tmux-native TUI panel that shows every coding agent running in the current tmux session, their live status, and lets you jump to, launch, and resume agents. Rust, from scratch. Vocabulary in [CONTEXT.md](../CONTEXT.md); load-bearing decisions in [docs/adr/](adr/).

## Model

- **Discovery-based identity** — an Agent is any pane in the current session whose process tree matches a provider's process rules. No registry; panes are the source of truth. ([CONTEXT.md](../CONTEXT.md))
- **One row per pane** — providers may host more than one native Session, but the panel still renders one Agent per pane. Amp's row follows its interactive TUI's foreground thread; background threads and non-TUI runner/execute modes are outside the integration boundary.
- **Hook-driven status, no daemon** — plugins normalize provider hook payloads into unified events; the core appends them to per-session JSONL logs; the TUI derives dynamic status by pure replay + fs watch, with Idle as the fallback before a discovered process emits its first event. (ADR 0001)
- **Providers as external executables** — `gw-provider-<id>` binaries speaking a pure-translator protocol (`manifest` / `normalize`); the core owns all I/O. (ADR 0002)
- **Statuses** (sort order = priority): Attention (approval > question) / Error / Stale / Working / Done / Idle. Done means the turn ended; it never decays. ([CONTEXT.md](../CONTEXT.md))
- **Session vs Agent**: an ended Session (pane gone, native session id in the log) is resumable from a secondary view.

## Event ↔ pane correlation

The hook process is a child of the agent process. The core walks the ppid chain to the provider process (matching via the plugin's process rules), resolves its tty to a pane id, and records pane id + pid + native session id + cwd with the events. The panel joins live pane scans to the log by provider pid. If the pid does not match, the Agent is Idle until the current process emits a hook event; pane id and cwd describe location but never transfer Session state between processes.

## UX

- **Panel** runs identically in a persistent pane (dashboard) or `tmux display-popup` (primary posture); after a jump the popup instance exits. Scope: current session only.
- **List columns**: provider, status (+ duration in that status), window/pane, detail (one-line status context: current activity · task, awaited approval, turn summary, failure reason), cwd (abbreviated), git branch. Running subagents render as dim indented sub-lines under their agent's row (`↳ type · model · task · age`).
- **Activity**: compact event timeline of the selected agent (recent turns, tool activity, attention, subagents from its Event Log; display only — the panel never touches the agent's window).
- **Keys (v1)**: `j/k` or arrows move, `Enter` jumps, `n` launches (pick provider → new window in the panel's cwd → jump), `r` toggles the resumable-sessions view (`Enter` = new window running the provider's resume command), `tab` toggles current/global views, `a` selects the next Attention agent, `?` opens the keyboard-shortcuts page, and `Esc`/`Ctrl-C` quit.
- **Notifications**: `gw hook` itself fires a desktop notification (macOS `osascript`) + terminal bell when writing an attention event — global notifications without a daemon.
- **Setup**: `gw setup` installs hooks into provider global configs for every discovered plugin. Surgical merge only — preserve unrelated keys and formatting (claude's `settings.json` mixes user config with hooks), back up before writing, idempotent, reversible via `gw setup --remove`. Providers may also declare a whole managed integration file: the core creates, hashes, upgrades, and removes it only while its ownership marker and body hash prove it remains unmodified. Amp uses this for `~/.config/amp/plugins/gw.ts`. For live providers, the panel checks these declared targets directly and banners only when setup is missing or drifted; event presence is not evidence of setup health.

## Plugin protocol (v1 sketch)

- Discovery: `gw-provider-*` on PATH and in `~/.config/gw/providers/bin/`.
- `manifest` → JSON: protocol version, provider id, display label/color, process match rules (argv basename patterns plus exact excluded args), launch command, resume command template (`{session_id}`, `{cwd}`), hook install specs (target files, entries to merge), and optional managed integration files.
- `normalize` → stdin: raw hook payload JSON; stdout: zero or more unified events (JSONL): `session_focus`, `session_start`, `turn_start`, `turn_end`, `turn_error`, `attention` (kind: approval | question), `heartbeat`, `subagent_start`, `subagent_end`, `session_end` — each with native session id; see `protocol.md` for the per-kind optional fields. `session_focus` changes correlation without changing status.
- Official plugins: `gw-provider-claude`, `gw-provider-codex`, `gw-provider-amp` (same workspace, same protocol, no fast path).

## Storage

- Event logs: `~/.local/state/gw/sessions/<sha256(provider:native_session_id)>.jsonl`, append-only, `O_APPEND` single-write per event; retention sweep on panel start (drop logs of dead sessions older than N days).
- Config (optional, TOML): `~/.config/gw/config.toml` — currently `notify` and `[debug] hooks` (see `config.md`); stale threshold, plugin dir overrides, keybindings later.

## Crate layout

Cargo workspace:

- `gw-core` — domain: discovery, event model, status derivation (pure), correlation, log store, plugin client, tmux shell-out wrapper.
- `gw` — the binary: CLI (`panel`, `hook`, `setup`), ratatui TUI (fullscreen alt-screen; `TuiEvent`/`AppEvent` split, single `tokio::select!` loop, frame coalescing — patterned after codex-rs's tui architecture).
- `gw-plugin-protocol` — serde types for manifest/events, published for Rust plugin authors (the protocol itself is JSON-over-CLI; non-Rust plugins just follow the spec).
- `gw-provider-claude`, `gw-provider-codex`, `gw-provider-amp` — official plugin binaries.

## Backlog

- Kill selected agent from the panel (`x`, with confirm).
- Last-message summary line per agent (from `turn_end` payload).
- Reconcile the agy provider with `provider-hooks.md`: the shipped
  `gw-provider-agy` has drifted from the doc. The doc describes PascalCase
  `hook_event_name` events (`PreInvocation`/`PostInvocation`/`Stop`…) keyed on
  `trajectory_id`/`conversation_id` with `tool_call_json`; the implementation
  instead reads `conversationId` (camelCase) and discriminates purely by which
  numeric field is present (`invocationNum`→turn_start, `stepIdx`→heartbeat,
  `executionNum`+`fullyIdle`→turn_end/heartbeat), never touching
  `hook_event_name`. The doc's "Current gw subscriptions" table also omits agy
  entirely. Capture real agy payloads via `[debug] hooks` (see `config.md`),
  then update the agy section and the subscriptions table to match. claude and
  codex already match the doc 1:1.

## Transition note

The predecessor tool installed hooks invoking `gw hook <provider>` — the same command shape. Installing this binary first on PATH takes over those hook calls cleanly; remove the old binary to avoid schema confusion.
