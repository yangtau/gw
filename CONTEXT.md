# CONTEXT

Glossary for `gw`, a tmux-native coding agent status panel (Rust TUI).

## Terms

### Agent
A tmux pane in any tmux session running a known coding agent CLI. Identity is **discovery-based**: the tool finds Agents by scanning panes, regardless of how they were started; tmux panes are the single source of truth and the tool keeps no registry of its own.

### Provider
A kind of coding agent CLI (e.g. claude, codex, amp, opencode, pi). Defines how to recognize, launch, and interpret the state of Agents of that kind.

### Provider Plugin
The implementation vehicle of a Provider: a standalone executable, discovered by naming convention, implementing a uniform plugin protocol. A plugin is a pure translator with no side effects: `manifest` describes the provider statically (process match rules, launch command, hook/config or managed-file install spec); `normalize` turns a provider hook payload (stdin) into unified events (stdout). Hook commands installed into provider configs invoke the core (`<tool> hook <provider>`), which delegates payload translation to the plugin and owns all event-log writing itself. Managed integration files, such as the Amp, OpenCode, and Pi TypeScript observer plugins, are likewise written only by the core from manifest data. Every provider — including official ones shipped with the tool — goes through the same protocol; there is no built-in fast path. Private providers live in separate repositories.

### Session
A provider-native conversation session, identified by the provider's native session id, recorded in the Event Log. The bare word "session" always means this; a tmux session is never abbreviated — it is written "tmux session" in full everywhere (UI copy, code identifiers: `tmux_session_*`). A Session whose process is alive and bound to a pane is an Agent; a Session is *ended* only when no pane in any tmux session hosts it — ended Sessions may still be resumable (`--resume <id>`). The panel's main list shows Agents; a secondary view lists recently ended, resumable Sessions, unscoped by tmux session (an ended Session belongs to no tmux session).

Some providers can host multiple Sessions in one process. gw remains pane-centric:
one pane is one Agent row. For Amp and Pi, that row follows only the interactive
TUI's foreground Session; Amp background threads do not take ownership of the row.

### Status
An Agent's runtime state. Dynamic states come from provider hook events: hooks installed into the provider's config report key moments (turn start, stop, approval requests), and the panel derives Status from that event stream. A discovered process with no attributable events defaults to **Idle**. The tool never scrapes pane content or injects keys. Statuses are **eventually consistent**: attention is cleared by subsequent activity events, never by explicit acknowledgement.

Table order is sort order (most urgent first):

| Status | Meaning |
|---|---|
| **Attention** | Blocked **mid-turn** on the user: a pending **approval** (permission dialog) or **question** (the agent explicitly asked something). The turn has not ended. |
| **Error** | The last turn aborted with a provider-reported failure (rate limit, billing, auth, …). |
| **Stale** | Working, but silent past a threshold — a suspected silent failure (hung process, provider without failure events dying quietly). |
| **Working** | A turn is in progress. |
| **Done** | The last turn finished normally; its result awaits the user. Cleared only by the next turn — never by being looked at. |
| **Idle** | Agent alive with no active turn, including a newly discovered process that has not emitted an attributable event yet. |

Statuses a provider can reach depend on which hook events it emits; the model is sized to the richest provider and degrades per-provider. Existence is not a Status: when the pane/process disappears, the Agent leaves the panel (hence no "exited" state — Done is about the turn, not the process).

### Subagent
A child agent running inside a Session, reported by the provider's subagent hooks (`subagent_start`/`subagent_end` events on the parent's session id). Subagents are display-only context — the panel lists them under their Agent's row (type, model, task) — and are **status-neutral**: they never clear Attention, revive Done, or affect staleness. The running set is replayed from start/end pairs and cleared at turn and session boundaries — a subagent cannot outlive the turn that spawned it, so a Done agent shows no subagents even when an end event was missed.

### Panel
The TUI itself. Runs equally in a persistent pane (dashboard mode) or a tmux `display-popup` (switcher mode — the primary posture: summon, pick, jump, gone). The only behavioral difference is whether the panel exits after a jump. The Panel has two Views; discovery itself is always global — a View only filters what is displayed.

The Panel can also run in a terminal outside tmux when a tmux server is
available. Jumping to an Agent then exits the Panel, restores the terminal, and
attaches that terminal to the Agent's exact tmux session, window, and pane.
Detaching that tmux client returns to the same Panel. Inside tmux, jumping
continues to switch the existing tmux client.

### View
Which Agents the Panel displays. The **current view** shows Agents in the current tmux session, with a passive hint when Agents elsewhere need attention. The **global view** shows all Agents grouped by tmux session — current tmux session first, others by name; groups are plain headers (not selectable, not collapsible); tmux sessions with no Agents do not appear. One key toggles between the two; the starting view is configurable. Views never change behavior — Launch always targets the current tmux session regardless of view.

### Launch
Creating a new Agent from the panel: pick a provider → a new tmux window opens running that provider's CLI in the panel's current working directory → focus jumps there. No further prompts; anything unusual is started by hand and picked up by discovery.

### Activity
The Panel's view of what the selected Agent has been doing: a compact timeline of the Session's recent events (turns, tool activity, attention, subagent lifecycle) rendered from the Event Log — the same source that drives Status. Activity is display-only and side-effect-free: the Panel never touches the Agent's window; seeing the Agent's actual screen means jumping to its pane.

### Event Log
One append-only JSONL file per agent session. Plugins write normalized events on hook invocation; the TUI derives Status by replaying the full log and updates incrementally via filesystem watch. There is no daemon: the event log is the only persistent state, and status derivation is a pure function (event sequence → Status). The log additionally admits **core-written operational annotations** (`wait_start`/`wait_end`, recorded by `gw wait` on the waiter's log) — these are status-neutral, same class as subagent/focus events; dynamic Status remains a pure replay of **provider** events only (locked invariant).

### Address
How CLI commands (`gw ls` / `show` / `wait` / `resume`) reference a Session: canonically `provider:session-id`; a bare session id or a unique id prefix (≥ 4 chars) is accepted when unambiguous. Ambiguity or an unknown address is an error, never a silent first match. Scope is honest: only Sessions gw has observed via hooks resolve — not the provider's full universe.
