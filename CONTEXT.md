# CONTEXT

Glossary for `gw`, a tmux-native coding agent status panel (Rust TUI).

## Terms

### Agent
A tmux pane in the current session running a known coding agent CLI. Identity is **discovery-based**: the tool finds Agents by scanning panes, regardless of how they were started; tmux panes are the single source of truth and the tool keeps no registry of its own.

### Provider
A kind of coding agent CLI (e.g. claude, codex, traex). Defines how to recognize, launch, and interpret the state of Agents of that kind.

### Provider Plugin
The implementation vehicle of a Provider: a standalone executable, discovered by naming convention, implementing a uniform plugin protocol. A plugin is a pure translator with no side effects: `manifest` describes the provider statically (process match rules, launch command, hook install spec); `normalize` turns a provider hook payload (stdin) into unified events (stdout). Hook commands installed into provider configs invoke the core (`<tool> hook <provider>`), which delegates payload translation to the plugin and owns all event-log writing itself. Every provider — including official ones shipped with the tool — goes through the same protocol; there is no built-in fast path. Private providers (e.g. traex) live in separate repositories.

### Session
A provider-native conversation session, identified by the provider's native session id, recorded in the Event Log. A Session whose process is alive and bound to a pane is an Agent; a Session whose pane is gone is *ended* but may still be resumable (`--resume <id>`). The panel's main list shows Agents; a secondary view lists recently ended, resumable Sessions.

### Status
An Agent's runtime state. Its **only source is provider hook events**: hooks installed into the provider's config report key moments (turn start, stop, approval requests); the panel derives Status from the event stream. The tool never scrapes pane content or injects keys. Statuses are **eventually consistent**: attention is cleared by subsequent activity events, never by explicit acknowledgement.

| Status | Meaning |
|---|---|
| **Attention** | Needs the user: pending approval / question / unhandled notification. Always sorts first. |
| **Working** | A turn is in progress. |
| **Idle** | Last turn finished; waiting for the next instruction. |
| **Stale** | Process alive but no hook events for a threshold period — likely hung. |
| **Unknown** | Agent process discovered but no hook events attributable to it. |

Existence is not a Status: when the pane/process disappears, the Agent leaves the panel (hence no "done"/"exited" state).

### Panel
The TUI itself. Runs equally in a persistent pane (dashboard mode) or a tmux `display-popup` (switcher mode — the primary posture: summon, pick, jump, gone). The only behavioral difference is whether the panel exits after a jump. Scope is always the current tmux session.

### Launch
Creating a new Agent from the panel: pick a provider → a new tmux window opens running that provider's CLI in the panel's current working directory → focus jumps there. No further prompts; anything unusual is started by hand and picked up by discovery.

### Event Log
One append-only JSONL file per agent session. Plugins write normalized events on hook invocation; the TUI derives Status by replaying the full log and updates incrementally via filesystem watch. There is no daemon: the event log is the only persistent state, and status derivation is a pure function (event sequence → Status).
