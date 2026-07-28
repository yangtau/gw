# Provider hook reference

Authoritative reference for the hook systems of the four providers gw integrates
with: what events exist, how hooks are configured, how the hook process is invoked,
and the exact payload each event delivers. This is the factual basis for the unified
event vocabulary in `protocol.md`.

Sources and confidence:

- **claude**: official docs (code.claude.com/docs/en/hooks). Verified.
- **codex**: generated JSON schemas in the codex repo
  (`codex-rs/hooks/schema/generated/*.input.schema.json`) — the authoritative
  contract. Verified against source.
- **amp**: official manual and `@ampcode/plugin` type reference
  (ampcode.com/manual and ampcode.com/manual/plugin-api). Verified against the
  2026-07-18 public API.
- **pi**: official extension and session-format documentation, verified against
  Pi 0.82.1.

## Shared model

Claude and Codex run external hook commands that receive one JSON object on
**stdin** and communicate back via exit code (and optionally stdout JSON for
decision-making hooks). Amp and Pi instead deliver typed events to TypeScript
integrations; gw installs small observer files that forward compact JSON to
`gw hook <provider>` on stdin. Every integration is observer-only: gw never
returns a permission decision or modifies provider behavior.

---

## amp

### Configuration and invocation

- Amp system plugins live at `~/.config/amp/plugins/*.ts` and run under Bun.
- `gw setup` installs `~/.config/amp/plugins/gw.ts` as a hash-protected managed
  file. An already-running Amp TUI must run `plugins: reload` or restart.
- The bridge invokes `gw hook amp` with compact JSON on stdin. It serializes
  invocations to preserve event order and swallows/logs all failures so gw can
  never fail an Amp turn.
- One Amp TUI can host several threads. The bridge uses
  `amp.activeThread.current` and forwards only the foreground thread; a
  foreground change emits `session_focus`, which changes pane correlation but
  preserves the thread's previous status.
- `amp --no-tui` and `amp -x`/`--execute` have no focused TUI thread and are not
  forwarded. Process discovery also excludes those argv flags.

### Public lifecycle surface

| Signal | Fields relevant to gw | Notes |
|---|---|---|
| `session.start` | `thread.id` | fires for a new thread and when an existing thread is opened/switched; not equivalent to a new native session |
| `agent.start` | `thread.id`, `message`, `id` | user turn began |
| `agent.end` | `thread.id`, `status`, `messages` | status is `done`, `error`, or `cancelled`; messages contain the final assistant text |
| `tool.call` | `thread.id`, `tool`, `input` | decision hook: every handler must return allow/reject/modify/synthesize/error |
| `tool.result` | `thread.id`, `tool`, `input`, `status` | read-only post-tool activity signal |
| `PluginThread.state` | `idle`, `running`, `awaiting-approval`, `error` | observable state plus an immediate `get()` snapshot |

gw deliberately does **not** subscribe to `tool.call`: the API has no neutral
observer result, and returning `allow` would participate in the permission
decision. Tool heartbeats therefore come from `tool.result`. An approval is
observed from `PluginThread.state == "awaiting-approval"`; its tool detail may be
absent.

There is no `session.end`, explicit question-waiting event, or public subagent
lifecycle event. Amp normally does not ask for tool approval; Attention/approval
is reachable only when the user has independently enabled an Amp permission
policy. A textual question in the final assistant response is `Done`, not a
mid-turn Attention/question.

### Bridge payload

The managed TypeScript bridge intentionally narrows Amp's large event objects
before invoking the provider normalizer:

```json
{"thread_id":"T-…","event":"agent_start","message":"fix the tests"}
{"thread_id":"T-…","event":"tool_result","tool":"shell: cargo test"}
{"thread_id":"T-…","event":"agent_end","status":"done","summary":"all green"}
```

`cancelled` maps to the same terminal `turn_end`/Done state as `done`, by product
decision. `error` maps to `turn_error`.

---

## codex

### Configuration

- Hook definitions: `~/.codex/hooks.json` (same event→matcher→commands shape as
  claude settings).
- Feature gate: `~/.codex/config.toml` must contain `[features] hooks = true`
  (gw setup writes this as a `set` patch, kept on uninstall).
- Hooks are spawned via `$SHELL -lc '<command>'`, default timeout 600s.
- Trust model: non-managed hook files prompt a one-time review in the codex TUI
  before hooks run. Expect the user to approve gw's hooks on first codex launch
  after `gw setup`.

### Common payload fields

Every event payload includes:

| Field                    | Type             | Notes                                                                                             |
| ------------------------ | ---------------- | ------------------------------------------------------------------------------------------------- |
| `hook_event_name`        | string           | PascalCase event name                                                                             |
| `session_id`             | string           |                                                                                                   |
| `cwd`                    | string           |                                                                                                   |
| `model`                  | string           |                                                                                                   |
| `transcript_path`        | string \| null   |                                                                                                   |
| `permission_mode`        | enum             | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` (absent on compact events)   |
| `turn_id`                | string           | codex extension: the active turn id; present on all turn-scoped events (absent on `SessionStart`) |
| `agent_id`, `agent_type` | string, optional | set when the event originates from a subagent                                                     |

### Events (10)

| Event               | Extra required fields                                                                                                                                   |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `SessionStart`      | `source`: `startup` / `resume` / `clear` / `compact`                                                                                                    |
| `UserPromptSubmit`  | `prompt`                                                                                                                                                |
| `PreToolUse`        | `tool_name`, `tool_input`, `tool_use_id`                                                                                                                |
| `PermissionRequest` | `tool_name`, `tool_input` (fires at 4 approval call sites, incl. network access and privilege escalation; stdout JSON can allow/deny — gw stays silent) |
| `PostToolUse`       | `tool_name`, `tool_input`, `tool_response`, `tool_use_id` (fires on success only)                                                                       |
| `PreCompact`        | `trigger`: `manual` / `auto`                                                                                                                            |
| `PostCompact`       | `trigger`: `manual` / `auto`                                                                                                                            |
| `SubagentStart`     | `agent_id`, `agent_type` (required here)                                                                                                                |
| `SubagentStop`      | `agent_id`, `agent_type`, `agent_transcript_path`, `last_assistant_message`, `stop_hook_active`                                                         |
| `Stop`              | `last_assistant_message` (string \| null), `stop_hook_active`                                                                                           |

Notably absent vs claude: no `SessionEnd`, no `Notification`, no failure events
(`StopFailure` / `PostToolUseFailure`). A codex agent that dies of a rate limit
emits nothing — its gw status decays through Working → Stale.

### Example payload (`PermissionRequest`)

```json
{
  "hook_event_name": "PermissionRequest",
  "session_id": "0198…",
  "cwd": "/Users/me/project",
  "model": "gpt-5.6-sol",
  "permission_mode": "default",
  "transcript_path": "/Users/me/.codex/sessions/….jsonl",
  "turn_id": "turn_42",
  "tool_name": "shell",
  "tool_input": { "command": ["rm", "-rf", "build"] }
}
```

---

## pi

### Configuration and invocation

- Pi extensions live at `~/.pi/agent/extensions/*.ts`.
- `gw setup` installs `~/.pi/agent/extensions/gw.ts` as a hash-protected
  managed file. An already-running Pi TUI must run `/reload` or restart.
- The extension invokes `gw hook pi` with compact JSON on stdin, serializes
  invocations to preserve event order, and swallows failures so gw cannot affect
  a Pi run.
- Only interactive TUI mode is observed. Print, JSON, RPC, export, and model-list
  invocations are outside the integration boundary.
- Pi can replace the current Session inside one process via `/new`, `/resume`,
  `/fork`, and `/clone`. The extension reads the current UUID and transcript path
  from `ctx.sessionManager` on every event, so the pane's Agent row follows the
  foreground Session.

### Public lifecycle surface

| Signal | Fields relevant to gw | Notes |
|---|---|---|
| `session_start` | `reason`, `ctx.sessionManager`, `ctx.model` | `startup`, `reload`, `new`, `resume`, or `fork`; distinguishes a new Session from a foreground change |
| `before_agent_start` | `prompt` | top-level user run began |
| `agent_start` | — | low-level run began; repeats for automatic retry and compaction recovery |
| `tool_execution_start` | `toolName`, `args` | tool activity; parallel calls can interleave |
| `turn_end` | finalized assistant `message` | cached for final text and `stopReason` |
| `agent_end` | messages from the low-level run | may precede automatic retry or compaction |
| `agent_settled` | current idle state | authoritative terminal signal after retry, compaction, and queued follow-ups finish |
| `session_shutdown` | `reason` | `quit`, `reload`, `new`, `resume`, or `fork` |

The bridge emits `session_start` for new Sessions and `session_focus` for reloads
or resumed Sessions. It maps `before_agent_start` to `turn_start`,
`tool_execution_start` to `heartbeat`, and waits for `agent_settled` before
emitting `turn_end` or `turn_error`. The final assistant message's
`stopReason == "error"` is a failure; aborts and other terminal reasons settle as
Done. `session_shutdown` maps to `session_end` except during `/reload`, where the
same Session remains active.

Pi has no built-in permission popup and no provider-wide event for arbitrary
extension UI dialogs. The observer therefore never emits Attention. A Pi Agent
can show Error because finalized assistant messages report provider failures.

---

## claude

### Configuration

Hooks live in `~/.claude/settings.json` (shared with all other user settings —
this is why gw's setup uses surgical JSON patches with a backup):

```json
{
  "hooks": {
    "PermissionRequest": [
      { "hooks": [{ "type": "command", "command": "gw hook claude" }] }
    ],
    "Notification": [
      {
        "matcher": "elicitation_dialog|agent_needs_input",
        "hooks": [{ "type": "command", "command": "gw hook claude" }]
      }
    ]
  }
}
```

Matcher semantics (per entry, optional):

| Pattern              | Meaning               |
| -------------------- | --------------------- |
| omitted / `""` / `*` | match all             |
| plain name           | exact, case-sensitive |
| `A\|B`               | alternation           |
| anything else        | regex                 |

What the matcher matches depends on the event: tool events → `tool_name`,
`SessionStart` → `source`, `Notification` → `notification_type`,
`SessionEnd` → `end_reason`, `StopFailure` → `error_type`,
`SubagentStart/Stop` → `agent_type`. `UserPromptSubmit` and `Stop` take no
matcher.

### Invocation mechanics

The claude process spawns the hook command **directly** (no intermediate shell —
verified live: the hook process's ppid is the agent itself; gw's ppid-walk
correlation starts inclusive of `from_pid` for exactly this reason). JSON on
stdin; exit 0 = proceed (stdout JSON parsed if present), exit 2 = block (where
the event is blockable), other = non-blocking error.

### Common payload fields

`session_id`, `transcript_path`, `cwd`, `hook_event_name`, `permission_mode`
(`default`/`acceptEdits`/`plan`/`dontAsk`/`bypassPermissions`/`auto`),
`prompt_id`, `effort.level`; plus `agent_id`/`agent_type` in subagent context.

### Events relevant to gw

Claude Code has ~30 hook events; the ones below are the observation-relevant
subset. (Others — `PostToolBatch`, `FileChanged`, `CwdChanged`,
`MessageDisplay`, `Setup`, `UserPromptExpansion`, `TaskCreated/Completed`,
`TeammateIdle`, elicitation control events — are automation/steering hooks gw
does not need.)

| Event                | Extra fields                                                                                        | Notes                                                                                                                                            |
| -------------------- | --------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `SessionStart`       | `source` (startup/resume/clear/compact), `model`, `session_title?`                                  |                                                                                                                                                  |
| `UserPromptSubmit`   | `prompt`                                                                                            |                                                                                                                                                  |
| `PreToolUse`         | `tool_name`, `tool_input`                                                                           |                                                                                                                                                  |
| `PermissionRequest`  | `tool_name`, `tool_input`, `permission_kind` (`tool_call`)                                          | fires when a permission dialog appears; exit 0 + no output = pure observer, dialog proceeds normally. Does NOT fire in `-p` non-interactive mode |
| `PostToolUse`        | `tool_name`, `tool_input`, `tool_output` ({stdout, stderr, exit_code})                              |                                                                                                                                                  |
| `PostToolUseFailure` | `tool_name`, `tool_input`, `error`, partial `tool_output`                                           | agent is still working after this                                                                                                                |
| `Notification`       | `notification_type`, `message`*                                                                     | see type table below                                                                                                                             |
| `Stop`               | `last_assistant_message`, `stop_hook_active`                                                        |                                                                                                                                                  |
| `StopFailure`        | `error_type`, `error_message`                                                                       | turn aborted by API error                                                                                                                        |
| `SubagentStart`      | `agent_id`, `agent_type`                                                                            | fires with the parent's `session_id`; **no** model or task text (verified against the 2.1.210 binary — `task_description` belongs to `TaskCreated`) |
| `SubagentStop`       | `agent_id`, `agent_type`, `agent_transcript_path`, `last_assistant_message`, `stop_hook_active`     | fires with the parent's `session_id`                                                                                                            |
| `SessionEnd`         | `end_reason` (clear/resume/logout/prompt_input_exit/bypass_permissions_disabled/other), `exit_code` |                                                                                                                                                  |

\* `message` is not listed in the documented payload schema but is present in
live-captured payloads (gw stores it as the attention summary today). Treat
`notification_type` as the contract, `message` as best-effort display text.

`notification_type` values:

| Value                                           | Meaning                                | gw relevance                                                                               |
| ----------------------------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------ |
| `permission_prompt`                             | tool approval needed                   | redundant with `PermissionRequest` (both fire for the same dialog) — do not subscribe both |
| `idle_prompt`                                   | done, waiting for next input (60s nag) | noise; turn end already implies idle                                                       |
| `auth_success`                                  | login completed                        | noise                                                                                      |
| `elicitation_dialog`                            | MCP server opened an input form        | the agent is asking the user a question                                                    |
| `elicitation_complete` / `elicitation_response` | form lifecycle                         | noise                                                                                      |
| `agent_needs_input`                             | background session waiting             | question                                                                                   |
| `agent_completed`                               | background session finished            | covered by `Stop`                                                                          |

`StopFailure.error_type` values: `rate_limit`, `overloaded`,
`authentication_failed`, `oauth_org_not_allowed`, `billing_error`,
`invalid_request`, `model_not_found`, `server_error`, `max_output_tokens`,
`unknown`.

### Example payload (`PermissionRequest`)

```json
{
  "hook_event_name": "PermissionRequest",
  "session_id": "279b0f33-…",
  "cwd": "/Users/me/project",
  "transcript_path": "/Users/me/.claude/projects/…/279b0f33-….jsonl",
  "permission_mode": "default",
  "tool_name": "Bash",
  "tool_input": {
    "command": "rm -rf build",
    "description": "Remove build dir"
  },
  "permission_kind": "tool_call"
}
```

---

## Cross-provider comparison

| Capability          | claude                                              | codex                                            | amp                                      | pi                                      |
| ------------------- | --------------------------------------------------- | ------------------------------------------------ | ---------------------------------------- | --------------------------------------- |
| Session begin/focus | `SessionStart` (source)                             | `SessionStart` (source)                          | `session.start` (begin or focus)         | `session_start` (reason)                |
| Session end         | `SessionEnd` (end_reason)                           | —                                                | —                                        | `session_shutdown`                      |
| Turn begin          | `UserPromptSubmit`                                  | `UserPromptSubmit` (turn_id)                     | `agent.start`                            | `before_agent_start`                    |
| Turn end            | `Stop` (last_assistant_message)                     | `Stop` (last_assistant_message)                  | `agent.end` (`done` / `cancelled`)       | `agent_settled` after final `turn_end`   |
| Turn failed         | `StopFailure` (error_type)                          | —                                                | `agent.end` (`error`)                    | assistant `stopReason == "error"`       |
| Approval dialog     | `PermissionRequest`                                 | `PermissionRequest`                              | thread state `awaiting-approval`         | —                                       |
| Tool activity       | `PreToolUse` / `PostToolUse` / `PostToolUseFailure` | `PreToolUse` / `PostToolUse`                     | `tool.result`                            | `tool_execution_start`                  |
| Typed notifications | `Notification` (notification_type matcher)          | —                                                | —                                        | —                                       |
| Subagents           | `SubagentStart/Stop` + task events                  | `SubagentStart/Stop`                             | —                                        | — (extension-specific, no shared event) |
| Compaction          | `PreCompact`/`PostCompact`                          | `PreCompact`/`PostCompact`                       | —                                        | `session_before_compact`/`session_compact` |
| Session identity    | `session_id`                                        | `session_id` (+ `turn_id`)                       | `thread.id`                              | `SessionManager.getSessionId()`         |
| Config surface      | `~/.claude/settings.json`                           | `~/.codex/hooks.json` + config.toml feature flag | `~/.config/amp/plugins/gw.ts`            | `~/.pi/agent/extensions/gw.ts`          |

Consequences for gw:

- Claude, Amp, and Pi report turn failures. A Codex agent killed by a rate limit
  emits nothing and decays through Working → Stale.
- Claude and Codex expose dedicated approval events. Amp exposes approval as a
  thread state when a user permission policy is active; Pi has no generic
  provider-wide approval or question event.
- Claude fires both `PermissionRequest` and `Notification(permission_prompt)`
  for the same dialog; a provider plugin must subscribe exactly one.

## Current gw subscriptions

What the shipped plugins subscribe and how they map to unified events
(`protocol.md`).

| Provider | Hook (matcher)                                       | Unified event                                 |
| -------- | ---------------------------------------------------- | --------------------------------------------- |
| amp      | foreground change / `session.start`                   | `session_focus`                               |
| amp      | `agent.start`                                        | `turn_start` {message}                        |
| amp      | `tool.result`                                        | `heartbeat` {tool summary}                    |
| amp      | thread state `awaiting-approval`                      | `attention` approval                          |
| amp      | `agent.end` (`done` / `cancelled`)                    | `turn_end` {last assistant text}              |
| amp      | `agent.end` (`error`) / focused error state           | `turn_error` {last assistant text}            |
| claude   | `SessionStart`                                       | `session_start` {model}                       |
| claude   | `UserPromptSubmit`                                   | `turn_start` {prompt}                         |
| claude   | `PermissionRequest`                                  | `attention` approval {tool: argument}         |
| claude   | `PreToolUse` (`AskUserQuestion\|ExitPlanMode`)       | `attention` question {first question / plan}  |
| claude   | `Notification` (`elicitation_dialog\|agent_needs_input`) | `attention` question {message}           |
| claude   | `PostToolUse`                                        | `heartbeat` {tool_name}                       |
| claude   | `PreCompact` / `PostCompact`                         | `heartbeat` {"compact"}                       |
| claude   | `SubagentStart`                                      | `subagent_start` {agent_id, agent_type}       |
| claude   | `SubagentStop`                                       | `subagent_end` {agent_id}                     |
| claude   | `Stop`                                               | `turn_end` {last_assistant_message}           |
| claude   | `StopFailure`                                        | `turn_error` {error_type, error_message}      |
| claude   | `SessionEnd`                                         | `session_end`                                 |
| codex    | `SessionStart`                                       | `session_start` {model}                       |
| codex    | `UserPromptSubmit`                                   | `turn_start` {prompt}                         |
| codex    | `PermissionRequest`                                  | `attention` approval {command}                |
| codex    | `PostToolUse`                                        | `heartbeat` {tool_name}                       |
| codex    | `PreCompact` / `PostCompact`                         | `heartbeat` {"compact"}                       |
| codex    | `SubagentStart`                                      | `subagent_start` {agent_id, agent_type, model} |
| codex    | `SubagentStop`                                       | `subagent_end` {agent_id}                     |
| codex    | `Stop`                                               | `turn_end` {last_assistant_message}           |
| pi       | new `session_start`                                  | `session_start` {model}                       |
| pi       | resumed/reloaded `session_start`                     | `session_focus`                               |
| pi       | `before_agent_start`                                 | `turn_start` {prompt}                         |
| pi       | `agent_start`                                        | — (clear prior retry outcome)                 |
| pi       | `tool_execution_start`                               | `heartbeat` {tool summary}                    |
| pi       | `turn_end` / `agent_end`                             | — (cache final assistant outcome)             |
| pi       | `agent_settled` after successful final message       | `turn_end` {last assistant text}              |
| pi       | `agent_settled` after final assistant error          | `turn_error` {error message}                  |
| pi       | `session_shutdown` except reload                     | `session_end`                                 |

Deliberately unsubscribed on claude: `Notification` types `permission_prompt`
(duplicate of `PermissionRequest`), `idle_prompt` (Done already expresses it),
`auth_success` and elicitation lifecycle types (noise); task events (subagent
tool activity already heartbeats through the parent's `PostToolUse`).

Deliberately unsubscribed on Amp: `tool.call`, because it is a decision hook
with no observer-only return; all background-thread events, because one pane is
one foreground Agent row; and runner/execute modes, because they have no
interactive foreground thread to jump to.

Pi consumes `turn_end` and `agent_end` only to cache the latest assistant
outcome; neither is terminal because automatic retries, compaction, or queued
follow-ups may still run. It deliberately ignores per-message streaming and
tool progress updates (too noisy), plus arbitrary extension dialog internals,
which have no provider-wide observer event.
