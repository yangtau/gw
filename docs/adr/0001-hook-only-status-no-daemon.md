# ADR 0001: Hook-only status detection, event log as sole state, no daemon

## Status

Accepted (2026-07-13); amended (2026-07-25, 2026-07-28)

## Context

The panel must show what each agent is doing (working / waiting for approval / idle). Candidate detection mechanisms: scraping pane content with per-provider regexes, process-level heuristics, or the hook facilities every supported provider ships (claude `settings.json` hooks, codex `hooks.json`, traex `.trae/hooks.json`). Independently, hook events occur while the TUI is closed, so the state has to live somewhere: a resident daemon, or plain files.

## Decision

Dynamic status is derived **exclusively from provider hook events**; the sole fallback is that a discovered process with no attributable event is Idle. The tool never captures pane content for state and never injects keys. (An earlier allowance of capture-pane for the human-facing preview ended with ADR-0004; the tool no longer reads pane content at all.)

There is **no daemon**. Each hook invocation appends normalized events to a per-session append-only JSONL log. The TUI replays the log to derive status (`fn derive(events) -> Status` is pure) and follows it live via filesystem watch. Statuses are eventually consistent: attention clears when later activity events arrive, never by explicit acknowledgement.

A discovered Agent with no attributable events is **Idle**. Event absence is not an installation-health signal: a provider can legitimately be alive before its first event. The panel does not inspect or report setup health.

## Consequences

- Provider UI changes cannot break detection; the plugin surface stays "events in → status out".
- No daemon lifecycle problems; logs survive crashes; `cat` is the debugger.
- Status derivation is trivially golden-testable (event log fixture → expected status).
- A hung agent would show Working forever, so a Stale status (no events past a threshold while the process lives) is part of the model.
- Requires the setup step; an uninstrumented provider remains Idle without events.
- Log files need a retention sweep (delete logs of long-gone sessions on startup).
