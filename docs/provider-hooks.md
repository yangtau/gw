# Provider hook reference

Authoritative reference for the hook systems of the three providers gw integrates
with: what events exist, how hooks are configured, how the hook process is invoked,
and the exact payload each event delivers. This is the factual basis for the unified
event vocabulary in `protocol.md`.

Sources and confidence:

- **claude**: official docs (code.claude.com/docs/en/hooks). Verified.
- **codex**: generated JSON schemas in the codex repo
  (`codex-rs/hooks/schema/generated/*.input.schema.json`) — the authoritative
  contract. Verified against source.
- **agy**: binary inspection (proto type names, format strings). Directionally
  reliable; exact payload JSON marked UNVERIFIED until confirmed from the internal
  repo.

## Shared model

All three providers run hooks the same way: an external command receives one JSON
object on **stdin** and communicates back via **exit code** (and optionally stdout
JSON for decision-making hooks). gw only ever observes: exit 0, no stdout, so no
hook can ever block or alter the agent's behavior.

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

## agy

Everything below is from binary inspection (`hooks_go_proto` / `exa.hooks_pb`
types and format strings); treat as UNVERIFIED until checked against the internal
repo.

### Configuration

- Hooks attach through the plugin system, not global config: a plugin ships
  `plugins/<name>/hooks.json` ("Lifecycle hooks run by the plugin").
- `hooks.json` keys are PascalCase event names mapping to command arrays,
  e.g. `"PreInvocation": [...]`.
- The hook command's working directory is set to the directory containing
  `hooks.json`.
- External hooks receive JSON (format string: `failed to construct JSON
session-start hook`), presumably the proto message in JSON encoding
  (camelCase field names per the proto json tags — UNVERIFIED which casing the
  external boundary uses).

### Hook points (6)

Proto: `exa.hooks_pb.HookArgs` = `HookArgsCommon` + oneof:

| Event            | Args type                | Known fields        |
| ---------------- | ------------------------ | ------------------- |
| `SessionStart`   | `SessionStartHookArgs`   |                     |
| `PreInvocation`  | `PreInvocationHookArgs`  | explicit turn begin |
| `PostInvocation` | `PostInvocationHookArgs` | explicit turn end   |
| `PreToolUse`     | `PreToolHookArgs`        | `tool_call_json`    |
| `PostToolUse`    | `PostToolHookArgs`       | `tool_call_json`    |
| `Stop`           | `StopHookArgs`           |                     |

`HookArgsCommon` carries `trajectory_id` and `conversation_id` — the session
identity is the trajectory/conversation, not a claude-style `session_id`.

Hook _results_ are richer than claude/codex: `PreToolHookResult` can influence
permissions (`permissions.PermissionHookResult`), and results may inject steps
(`HookInjectedStep`: user/system/error message or tool call). gw ignores all of
this — observe only.

Notably absent: no `PermissionRequest`, no `Notification`, no failure events.
Until the hook surface grows, an agy agent can never show Attention in gw —
only Working / Idle / Stale.

### `PreInvocation`/`PostInvocation` vs `UserPromptSubmit`/`Stop`

agy models turn boundaries explicitly, which is cleaner than claude/codex where
turn start is inferred from prompt submission. The distinction between
`PostInvocation` and `Stop` (per-invocation end vs session-level stop?) must be
confirmed in the internal repo before writing the `gw-provider-agy` mapping.

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
| `SubagentStart`      | `agent_type`, `agent_id`, `task`                                                                    |                                                                                                                                                  |
| `SubagentStop`       | `agent_type`, `agent_id`, `last_assistant_message`                                                  |                                                                                                                                                  |
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

| Capability          | claude                                              | codex                                            | agy                                 |
| ------------------- | --------------------------------------------------- | ------------------------------------------------ | ----------------------------------- |
| Session begin       | `SessionStart` (source)                             | `SessionStart` (source)                          | `SessionStart`                      |
| Session end         | `SessionEnd` (end_reason)                           | —                                                | —                                   |
| Turn begin          | `UserPromptSubmit`                                  | `UserPromptSubmit` (turn_id)                     | `PreInvocation`                     |
| Turn end            | `Stop` (last_assistant_message)                     | `Stop` (last_assistant_message)                  | `PostInvocation`, `Stop`            |
| Turn failed         | `StopFailure` (error_type)                          | —                                                | —                                   |
| Approval dialog     | `PermissionRequest`                                 | `PermissionRequest`                              | —                                   |
| Tool activity       | `PreToolUse` / `PostToolUse` / `PostToolUseFailure` | `PreToolUse` / `PostToolUse`                     | `PreToolUse` / `PostToolUse`        |
| Typed notifications | `Notification` (notification_type matcher)          | —                                                | —                                   |
| Subagents           | `SubagentStart/Stop` + task events                  | `SubagentStart/Stop`                             | —                                   |
| Compaction          | `PreCompact`/`PostCompact`                          | `PreCompact`/`PostCompact`                       | —                                   |
| Session identity    | `session_id`                                        | `session_id` (+ `turn_id`)                       | `trajectory_id` / `conversation_id` |
| Config surface      | `~/.claude/settings.json`                           | `~/.codex/hooks.json` + config.toml feature flag | plugin `hooks.json`                 |

Consequences for gw:

- Only claude reports turn failures — the only source for an Error-like status.
  A codex/agy agent killed by a rate limit just goes quiet (Working → Stale).
- Only claude/codex expose approval dialogs; agy agents cannot show
  Attention/approval until its hook surface grows.
- claude fires both `PermissionRequest` and `Notification(permission_prompt)`
  for the same dialog; a provider plugin must subscribe exactly one.

## Current gw subscriptions

What the shipped plugins subscribe and how they map to unified events
(`protocol.md`).

| Provider | Hook (matcher)                                       | Unified event                                 |
| -------- | ---------------------------------------------------- | --------------------------------------------- |
| claude   | `SessionStart`                                       | `session_start` {model}                       |
| claude   | `UserPromptSubmit`                                   | `turn_start` {prompt}                         |
| claude   | `PermissionRequest`                                  | `attention` approval {tool: argument}         |
| claude   | `PreToolUse` (`AskUserQuestion\|ExitPlanMode`)       | `attention` question {first question / plan}  |
| claude   | `Notification` (`elicitation_dialog\|agent_needs_input`) | `attention` question {message}           |
| claude   | `PostToolUse`                                        | `heartbeat` {tool_name}                       |
| claude   | `PreCompact` / `PostCompact`                         | `heartbeat` {"compact"}                       |
| claude   | `Stop`                                               | `turn_end` {last_assistant_message}           |
| claude   | `StopFailure`                                        | `turn_error` {error_type, error_message}      |
| claude   | `SessionEnd`                                         | `session_end`                                 |
| codex    | `SessionStart`                                       | `session_start` {model}                       |
| codex    | `UserPromptSubmit`                                   | `turn_start` {prompt}                         |
| codex    | `PermissionRequest`                                  | `attention` approval {command}                |
| codex    | `PostToolUse`                                        | `heartbeat` {tool_name}                       |
| codex    | `PreCompact` / `PostCompact`                         | `heartbeat` {"compact"}                       |
| codex    | `Stop`                                               | `turn_end` {last_assistant_message}           |

Deliberately unsubscribed on claude: `Notification` types `permission_prompt`
(duplicate of `PermissionRequest`), `idle_prompt` (Done already expresses it),
`auth_success` and elicitation lifecycle types (noise); subagent/task events
(subagent tool activity already heartbeats through the parent's `PostToolUse`).
