# ADR 0001: Hook-only status detection, event log as sole state, no daemon

## Status

Accepted (2026-07-13)

## Context

The panel must show what each agent is doing (working / waiting for approval / idle). Candidate detection mechanisms: scraping pane content with per-provider regexes, process-level heuristics, or the hook facilities every supported provider ships (claude `settings.json` hooks, codex `hooks.json`, traex `.trae/hooks.json`). Independently, hook events occur while the TUI is closed, so the state has to live somewhere: a resident daemon, or plain files.

## Decision

Status is derived **exclusively from provider hook events**. The tool never captures pane content for state (capture-pane is allowed only for the human-facing preview) and never injects keys.

There is **no daemon**. Each hook invocation appends normalized events to a per-session append-only JSONL log. The TUI replays the log to derive status (`fn derive(events) -> Status` is pure) and follows it live via filesystem watch. Statuses are eventually consistent: attention clears when later activity events arrive, never by explicit acknowledgement.

Agents without hooks installed show as **Unknown**; the fix is a one-time global `gw setup` that installs hooks into provider configs (surgical merge: preserve unrelated content, back up before writing, idempotent).

## Consequences

- Provider UI changes cannot break detection; the plugin surface stays "events in → status out".
- No daemon lifecycle problems; logs survive crashes; `cat` is the debugger.
- Status derivation is trivially golden-testable (event log fixture → expected status).
- A hung agent would show Working forever, so a Stale status (no events past a threshold while the process lives) is part of the model.
- Requires the setup step; an uninstrumented provider degrades to Unknown rather than a rough guess.
- Log files need a retention sweep (delete logs of long-gone sessions on startup).
