# CONTEXT

Glossary for `gw`, a tmux-native coding agent status panel (Rust TUI).

## Terms

### Agent
A tmux pane in the current session running a known coding agent CLI. Identity is **discovery-based**: the tool finds Agents by scanning panes, regardless of how they were started; tmux panes are the single source of truth and the tool keeps no registry of its own.

### Provider
A kind of coding agent CLI (e.g. claude, codex, agy). Defines how to recognize, launch, and interpret the state of Agents of that kind.

### Provider Plugin
The implementation vehicle of a Provider: a standalone executable, discovered by naming convention, implementing a uniform plugin protocol. A plugin is a pure translator with no side effects: `manifest` describes the provider statically (process match rules, launch command, hook install spec); `normalize` turns a provider hook payload (stdin) into unified events (stdout). Hook commands installed into provider configs invoke the core (`<tool> hook <provider>`), which delegates payload translation to the plugin and owns all event-log writing itself. Every provider — including official ones shipped with the tool — goes through the same protocol; there is no built-in fast path. Private providers (e.g. agy) live in separate repositories.

### Session
A provider-native conversation session, identified by the provider's native session id, recorded in the Event Log. A Session whose process is alive and bound to a pane is an Agent; a Session whose pane is gone is *ended* but may still be resumable (`--resume <id>`). The panel's main list shows Agents; a secondary view lists recently ended, resumable Sessions.

### Status
An Agent's runtime state. Its **only source is provider hook events**: hooks installed into the provider's config report key moments (turn start, stop, approval requests); the panel derives Status from the event stream. The tool never scrapes pane content or injects keys. Statuses are **eventually consistent**: attention is cleared by subsequent activity events, never by explicit acknowledgement.

Table order is sort order (most urgent first):

| Status | Meaning |
|---|---|
| **Attention** | Blocked **mid-turn** on the user: a pending **approval** (permission dialog) or **question** (the agent explicitly asked something). The turn has not ended. |
| **Error** | The last turn aborted with a provider-reported failure (rate limit, billing, auth, …). |
| **Stale** | Working, but silent past a threshold — a suspected silent failure (hung process, provider without failure events dying quietly). |
| **Working** | A turn is in progress. |
| **Done** | The last turn finished normally; its result awaits the user. Cleared only by the next turn — never by being looked at. |
| **Idle** | Session alive but no turn has run yet. |
| **Unknown** | Agent process discovered but no hook events attributable to it. |

Statuses a provider can reach depend on which hook events it emits; the model is sized to the richest provider and degrades per-provider. Existence is not a Status: when the pane/process disappears, the Agent leaves the panel (hence no "exited" state — Done is about the turn, not the process).

### Panel
The TUI itself. Runs equally in a persistent pane (dashboard mode) or a tmux `display-popup` (switcher mode — the primary posture: summon, pick, jump, gone). The only behavioral difference is whether the panel exits after a jump. Scope is always the current tmux session.

### Launch
Creating a new Agent from the panel: pick a provider → a new tmux window opens running that provider's CLI in the panel's current working directory → focus jumps there. No further prompts; anything unusual is started by hand and picked up by discovery.

### Event Log
One append-only JSONL file per agent session. Plugins write normalized events on hook invocation; the TUI derives Status by replaying the full log and updates incrementally via filesystem watch. There is no daemon: the event log is the only persistent state, and status derivation is a pure function (event sequence → Status).
