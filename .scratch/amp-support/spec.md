# Amp provider support

## Goal

Add Amp as an official gw provider without weakening gw's pane-centric,
hook-only, observer-only model.

## Scope

- Ship an official `gw-provider-amp` alongside the existing official providers.
- Discover and launch interactive Amp TUIs, including
  `amp threads continue <thread-id>`.
- Exclude runner mode (`amp --no-tui`) and execute mode
  (`amp -x` / `amp --execute`).
- Keep one gw Agent row per tmux pane. An Amp pane represents only its current
  foreground thread; background threads do not update the row.
- Install a system Amp plugin at `~/.config/amp/plugins/gw.ts` through
  `gw setup`, and remove it through `gw setup --remove`.
- Keep the Amp plugin a strict observer: it must never approve, reject, modify,
  or otherwise delay an Amp tool call, and setup must not alter Amp permission
  settings.

Amp's `tool.call` plugin event is a decision hook: every handler must return an
explicit allow/reject/modify result. The bridge therefore deliberately does
not subscribe to it. Tool heartbeats come from `tool.result`, preserving the
strict-observer guarantee at the cost of not identifying a pending approval's
tool until Amp exposes a read-only signal.

## Event mapping

| Amp signal | gw event/status |
|---|---|
| foreground thread selected (`session.start`) | `session_focus` (status-neutral) |
| `agent.start` | `turn_start` / Working |
| `tool.result` | `heartbeat` |
| `agent.end` with `done` | `turn_end` / Done |
| `agent.end` with `cancelled` | `turn_end` / Done |
| `agent.end` with `error` | `turn_error` / Error |
| foreground thread state `awaiting-approval` | `attention` approval |

`session_focus` updates the thread-to-pane correlation without clearing a
previous Done, Error, Working, or Attention state. For a thread with no prior
status events, it naturally remains Idle.

Amp does not expose a reliable mid-turn "waiting for an answer" event, so the
provider does not synthesize Attention/question. A textual question in a final
assistant response is a completed turn and therefore Done.

## Foreground-thread behavior

Amp can host multiple threads in one TUI process, while gw deliberately models
one Agent per pane. The bridge forwards lifecycle events only for the currently
focused thread. It may retain enough in-process state to restore the latest
known terminal state when a background thread becomes foreground, but
background activity must not take ownership of the pane's gw row.

## Setup safety

The provider manifest needs a declarative managed-file install action because
Amp discovers TypeScript plugins from a directory rather than from a JSON/TOML
hook configuration.

- The core, not the provider, owns all file I/O.
- Create the managed file when absent.
- Treat identical content as already installed.
- Update only content that is verifiably gw-managed and unmodified.
- Refuse to overwrite an unrelated or user-modified file at the same path.
- Remove only content that is verifiably gw-managed and unmodified.
- A bridge failure to invoke `gw hook amp` must never fail an Amp turn.

## Commands

- Launch: `amp`
- Resume: `amp threads continue {session_id}`

## Explicit non-goals

- Amp runners and execute mode.
- One gw row per Amp background thread.
- Switching the Amp TUI to a particular thread when jumping to a pane.
- Pane scraping or key injection.
- Enabling or changing Amp permissions.
- Synthesizing Attention/question without an authoritative Amp event.

## Documentation and verification

- Update the README, design/protocol docs, and provider hook reference.
- Unit-test provider manifest and event normalization.
- Unit-test managed-file create/update/conflict/remove behavior.
- Unit-test process matching exclusions for non-TUI Amp modes.
- Unit-test that `session_focus` preserves status.
- Run focused tests first, then the workspace test suite.
