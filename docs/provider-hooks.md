# Provider hook reference

Authoritative reference for the hook systems of the six providers gw integrates
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
- **opencode**: official plugin documentation and generated SDK event types,
  verified against OpenCode 1.18.9.
- **grok**: official Grok Build user-guide (`10-hooks.md`, `17-sessions.md`)
  verified against Grok 1.0.0. Event names and camelCase envelope are
  documented; `Notification` type `permission_prompt` is inferred from the
  binary's notification vocabulary plus Claude-compatible naming (Grok has no
  `PermissionRequest` hook).

## Shared model

Claude, Codex, and Grok run external hook commands that receive one JSON object on
**stdin** and communicate back via exit code (and optionally stdout JSON for
decision-making hooks). Amp, OpenCode, and Pi instead deliver typed events to TypeScript
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
extension UI prompts. gw therefore relies on a **cooperative bus convention**
described below; without at least one extension participating, a Pi Agent can
show every gw status except Attention. It can still show Error because
finalized assistant messages report provider failures.

### Cooperative UI-prompt bus (`ui:prompt:opened` / `ui:prompt:closed`)

Pi ships an in-process pub/sub bus at `pi.events` for cross-extension
communication. gw subscribes to two well-known versioned topics on that bus.
Any observer (status-line extension, telemetry, IDE integration) can consume
the same events; gw is one consumer among many.

#### Payload

```ts
// ui:prompt:opened
{
  version: 1,               // required; other values ignored
  source: string,           // required; extension name, part of identity
  id: string,               // required; unique within `source`
  summary: string,          // required; human-readable, <= 240 chars
  kind?: "approval" | "question",  // optional; missing == "approval"
  tool?: string,
  toolCallId?: string,
}

// ui:prompt:closed
{
  version: 1,
  source: string,           // required; must match the opened event
  id: string,               // required; must match the opened event
  outcome?: string,         // optional; suggested: "accepted" | "rejected"
                            // | "cancelled" | "timed_out" | "error"
}
```

#### Producer contract

- **Only emit for prompts that suspend an active agent turn.** gw maps close
  to a Heartbeat, which flips status Attention → Working. Emitting from an
  idle context makes the Agent bounce Idle → Attention → Working → Stale for
  zero agent work.
- **Identity is `(source, id)`.** Two extensions may safely reuse the same
  `id` string. Within one `source`, an `id` MUST NOT be reused for a
  logically different prompt.
- **Emit the close in `finally`.** Missing a close leaves Attention pinned
  until the next real Pi event (agent activity, session end, ...).
- **A duplicate open with the same key is treated as an idempotent update.**
  Use it to refresh `summary` while the prompt is still open.
- **Overlapping prompts are supported.** gw tracks all open prompts by
  `(source, id)` and only clears Attention when the last one closes; closing
  a superseded prompt while others remain open triggers a fresh Attention
  event for the most recently opened remaining prompt.

#### gw processing rules

- The bridge validates every payload before forwarding. Producers cannot rely
  on unknown fields being passed through: gw currently persists `summary`,
  `kind`, `source`, `tool`, and `tool_call_id` after bounding each string.
- `kind` other than `"approval"` or `"question"` is dropped so a typo or a
  future protocol addition cannot silently become an Approval.
- `ui:prompt:closed` for an unknown `(source, id)` is silently dropped;
  producers therefore cannot cause phantom Heartbeats by miscounting closes.
- Bridge listeners are unsubscribed on `session_shutdown` (including
  `reason: "reload"`) so a rebound extension instance never races the old
  listeners.

#### Interaction with gw status

- `ui:prompt:opened` publishes an Attention event carrying `summary` and the
  resolved `kind`. Any Pi lifecycle event that follows normally supersedes
  the Attention through gw's "last event wins" rule (`derive_status` in
  `crates/gw-core/src/session.rs`).
- `ui:prompt:closed` publishes a Heartbeat carrying `outcome`. That
  Heartbeat immediately shifts Attention → Working. A subsequent terminal
  event (Turn end, error, ...) replaces it in the usual way; if none
  arrives, the Agent decays through Working → Stale (never to Idle).
- `/reload` emits Pi's status-neutral `session_focus`, so an Attention
  outstanding at reload time persists in the panel until the next real
  event. gw does not synthesize a clear on reload because doing so would
  drop legitimate Attention for a prompt that survived the reload.

#### Example producer

Drop into any permission or question extension:

```typescript
pi.on("tool_call", async (event, ctx) => {
  if (event.toolName !== "bash" || !ctx.hasUI) return;
  const cmd = String(event.input.command ?? "");
  if (!/\brm\s+-rf\b|\bsudo\b/i.test(cmd)) return;

  const source = "my-permission-gate";
  const id = event.toolCallId;
  pi.events.emit("ui:prompt:opened", {
    version: 1,
    source,
    id,
    kind: "approval",
    tool: event.toolName,
    toolCallId: event.toolCallId,
    summary: `Allow ${event.toolName}: ${cmd}`,
  });
  try {
    const ok = await ctx.ui.confirm("Dangerous command", cmd);
    pi.events.emit("ui:prompt:closed", {
      version: 1, source, id,
      outcome: ok ? "accepted" : "rejected",
    });
    return ok ? undefined : { block: true, reason: "Blocked by user" };
  } catch (err) {
    pi.events.emit("ui:prompt:closed", { version: 1, source, id, outcome: "error" });
    throw err;
  }
});
```

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

## opencode

### Configuration and invocation

- Global plugins live at `~/.config/opencode/plugins/*.{js,ts}` and load at
  process startup. `gw setup` installs `gw.ts` as a hash-protected managed file.
- The bridge invokes `gw hook opencode` with compact JSON on stdin. Calls are
  serialized to preserve event order; failures are swallowed so gw cannot fail
  an OpenCode turn.
- The integration targets the interactive TUI. Process discovery excludes
  non-interactive and remote-client commands including `run`, `serve`, `web`,
  `acp`, and `attach`.
- Child sessions are OpenCode subagents. Their events are ignored so the pane's
  row remains bound to the root interactive Session.

### Public lifecycle surface

| Signal | Fields relevant to gw | Notes |
|---|---|---|
| `chat.message` | `sessionID`, model, user text parts | user turn admitted; plugin hook |
| `session.status` | `sessionID`, `status.type` | `busy`, `retry`, or `idle` |
| `message.part.updated` | text part and `sessionID` | caches latest assistant text for terminal summary |
| `permission.asked` | `sessionID`, permission, patterns | blocking approval request |
| `permission.replied` | `sessionID`, reply | clears Attention through a heartbeat |
| `tool.execute.before` | `sessionID`, tool, args | tool activity; plugin hook |
| `session.error` | `sessionID`, typed error | provider-reported turn failure |
| `session.created` / `session.deleted` | Session info | native Session lifecycle |

`session.status: idle` is the authoritative successful turn boundary. The bridge
caches full text parts but forwards only a bounded one-line summary. A `retry`
status is activity rather than failure because OpenCode is still working.

### Bridge payload

```json
{"session_id":"ses_…","event":"turn_start","summary":"fix the tests"}
{"session_id":"ses_…","event":"tool_start","activity":"bash: cargo test"}
{"session_id":"ses_…","event":"permission_asked","summary":"bash: cargo test"}
{"session_id":"ses_…","event":"turn_end","summary":"all green"}
```

---

## grok

### Configuration and invocation

- Global hooks live at `~/.grok/hooks/*.json` and are always trusted. Project
  hooks under `<repo>/.grok/hooks/` require `/hooks-trust` (or `--trust`).
- `gw setup` installs `~/.grok/hooks/gw.json` via surgical JSON patches (the
  file is created if missing). An already-running Grok TUI must restart or
  reload hooks (`/hooks`, then `r`) before the new file is picked up.
- The hook command is spawned as a shell command. JSON arrives on stdin;
  `GROK_HOOK_EVENT` / `GROK_SESSION_ID` / `GROK_WORKSPACE_ROOT` are also set
  in the environment. gw reads stdin only.
- `gw hook grok` writes nothing to stdout and always exits 0 after reading
  the payload, so it cannot deny a `PreToolUse` or block a `Stop`.
- Process discovery targets the interactive TUI (`grok`, `grok --resume`).
  Headless (`-p` / `--single` / `--prompt-file` / `--prompt-json`) and
  `grok agent` are excluded, along with utility subcommands (`dashboard`,
  `sessions`, `export`, …).

### Common payload fields

Grok's stdin envelope is camelCase. Event *values* are snake_case. The
grok-agent-sdk rewrites keys to snake_case; file hooks (what gw installs)
keep the camelCase wire form.

| Field              | Type   | Notes                                                                                          |
| ------------------ | ------ | ---------------------------------------------------------------------------------------------- |
| `hookEventName`    | string | snake_case event name (`session_start`, `pre_tool_use`, `stop`, …)                             |
| `sessionId`        | string | native session id (UUIDv7 when Grok generates it)                                              |
| `cwd`              | string |                                                                                                |
| `workspaceRoot`    | string |                                                                                                |
| `timestamp`        | string | RFC 3339                                                                                       |
| `permissionMode`   | enum   | `default` / `auto` / `plan` / `bypassPermissions`                                              |

There is no documented `transcript_path`. `gw show --transcript` uses
`grok export {session_id}`, falling back to
`~/.grok/sessions/*/{session_id}/updates.jsonl`.

### Events relevant to gw

| Event                | Extra fields                                                                 | Notes                                                                                                                                     |
| -------------------- | ---------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `SessionStart`       | `source` (`startup` / `resume` / …)                                          | matcher tests `source`                                                                                                                    |
| `UserPromptSubmit`   | `prompt`                                                                     | matcher ignored                                                                                                                           |
| `PreToolUse`         | `toolName`, `toolInput`, `toolUseId`                                         | blocking — can deny; gw stays silent. Matcher tests the real tool name (`run_terminal_command`, `ask_user_question`, …)                   |
| `PostToolUse`        | `toolName`, `toolInput`, `toolResult`                                        | success only                                                                                                                              |
| `PostToolUseFailure` | `toolName`, `toolInput`                                                      | agent still working; gw does not subscribe                                                                                                |
| `PermissionDenied`   | `toolName`, `toolInput`                                                      | a deny, not a wait; gw does not subscribe                                                                                                 |
| `Notification`       | `notificationType`, `message`?                                               | matcher tests the type. Known types in the 1.0.0 binary include `permission_prompt`, `tool_execution`, `unknown`                          |
| `Stop`               | `lastAssistantMessage`, `stopHookActive`, `reason`                           | genuine turn end is `reason: "end_turn"`. A second observe-only Stop fires at session end (`channel_closed` / `shutdown`)                 |
| `StopFailure`        | `error`, `errorDetails`, `lastAssistantMessage`                              | `error` is the classified type the matcher tests: `rate_limit`, `authentication_failed`, `invalid_request`, `server_error`, `max_output_tokens`, `unknown` |
| `SubagentStart`      | subagent type (matcher), `agentId`?                                          | fires with the parent's `sessionId`                                                                                                       |
| `SubagentStop`       | same; `SubagentEnd` is accepted as an alias                                  | can block the subagent stop; gw stays silent                                                                                              |
| `PreCompact` / `PostCompact` | `trigger`: `manual` / `auto`                                         |                                                                                                                                           |
| `SessionEnd`         | end reason                                                                   | matcher tests the reason                                                                                                                  |

Notably absent vs claude: no `PermissionRequest`. Approvals are the TUI
permission prompt. gw observes them through `Notification(permission_prompt)`.
If that notification type is missing or renamed, a Grok Agent can still
reach every status except Attention/approval (questions still arrive via
`ask_user_question` / `exit_plan_mode`).

`Stop` / `SubagentStop` default to a 600s timeout because they are stop
gates. Other events default to 5s. Failures fail-open.

### Example payload (`PreToolUse`)

```json
{
  "hookEventName": "pre_tool_use",
  "sessionId": "abc-123",
  "cwd": "/Users/me/project",
  "workspaceRoot": "/Users/me/project",
  "permissionMode": "default",
  "toolName": "run_terminal_command",
  "toolInput": { "command": "npm test" },
  "timestamp": "2026-04-14T12:00:00Z"
}
```

---

## Cross-provider comparison

| Capability          | claude                                              | codex                                            | amp                              | opencode                         | pi                                      | grok                                              |
| ------------------- | --------------------------------------------------- | ------------------------------------------------ | -------------------------------- | -------------------------------- | --------------------------------------- | ------------------------------------------------- |
| Session begin/focus | `SessionStart` (source)                             | `SessionStart` (source)                          | `session.start` (begin or focus) | `session.created` / status busy  | `session_start` (reason)                | `SessionStart` (source)                           |
| Session end         | `SessionEnd` (end_reason)                           | —                                                | —                                | `session.deleted`                | `session_shutdown`                      | `SessionEnd`                                      |
| Turn begin          | `UserPromptSubmit`                                  | `UserPromptSubmit` (turn_id)                     | `agent.start`                    | `chat.message`                   | `before_agent_start`                    | `UserPromptSubmit`                                |
| Turn end            | `Stop` (last_assistant_message)                     | `Stop` (last_assistant_message)                  | `agent.end`                      | `session.status` idle            | `agent_settled` after final `turn_end`   | `Stop` (`reason == end_turn`)                     |
| Turn failed         | `StopFailure` (error_type)                          | —                                                | `agent.end` (`error`)            | `session.error`                  | assistant `stopReason == "error"`       | `StopFailure` (`error`)                           |
| Approval dialog     | `PermissionRequest`                                 | `PermissionRequest`                              | state `awaiting-approval`        | `permission.asked`               | cooperative `ui:prompt:opened` bus      | `Notification(permission_prompt)`                 |
| Tool activity       | `PreToolUse` / `PostToolUse` / `PostToolUseFailure` | `PreToolUse` / `PostToolUse`                     | `tool.result`                    | `tool.execute.before`            | `tool_execution_start`                  | `PreToolUse` / `PostToolUse`                      |
| Typed notifications | `Notification`                                      | —                                                | —                                | —                                | —                                       | `Notification`                                    |
| Subagents           | `SubagentStart/Stop` + task events                  | `SubagentStart/Stop`                             | —                                | child sessions (ignored)         | — (extension-specific, no shared event) | `SubagentStart` / `SubagentStop`                  |
| Compaction          | `PreCompact`/`PostCompact`                          | `PreCompact`/`PostCompact`                       | —                                | `session.compacted`              | `session_before_compact`/`session_compact` | `PreCompact` / `PostCompact`                   |
| Session identity    | `session_id`                                        | `session_id` (+ `turn_id`)                       | `thread.id`                      | Session `id`                     | `SessionManager.getSessionId()`         | `sessionId`                                       |
| Config surface      | `~/.claude/settings.json`                           | `~/.codex/hooks.json` + feature flag             | Amp global plugin                | OpenCode global plugin           | Pi global extension                     | `~/.grok/hooks/*.json`                            |

Consequences for gw:

- Claude, Amp, OpenCode, Pi, and Grok report turn failures. A Codex agent killed by a rate limit
  emits nothing and decays through Working → Stale.
- Claude and Codex expose dedicated approval events. Amp exposes approval as a
  thread state when a user permission policy is active. Pi has no built-in
  provider-wide approval event; the shipped bridge subscribes to a cooperative
  `ui:prompt:opened` / `ui:prompt:closed` convention on `pi.events` so any
  user extension that shows a blocking prompt can produce Attention. Without
  at least one cooperating extension a Pi Agent cannot enter Attention.
  Grok likewise has no `PermissionRequest`; gw uses `Notification(permission_prompt)`.
- Claude fires both `PermissionRequest` and `Notification(permission_prompt)`
  for the same dialog; a provider plugin must subscribe exactly one. Grok
  only has the notification.

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
| claude   | `PostToolUse`                                        | `heartbeat` {tool summary}                    |
| claude   | `PreCompact` / `PostCompact`                         | `heartbeat` {"compact"}                       |
| claude   | `SubagentStart`                                      | `subagent_start` {agent_id, agent_type}       |
| claude   | `SubagentStop`                                       | `subagent_end` {agent_id}                     |
| claude   | `Stop`                                               | `turn_end` {last_assistant_message}           |
| claude   | `StopFailure`                                        | `turn_error` {error_type, error_message}      |
| claude   | `SessionEnd`                                         | `session_end`                                 |
| codex    | `SessionStart`                                       | `session_start` {model}                       |
| codex    | `UserPromptSubmit`                                   | `turn_start` {prompt}                         |
| codex    | `PermissionRequest`                                  | `attention` approval {command}                |
| codex    | `PostToolUse`                                        | `heartbeat` {tool summary}                    |
| codex    | `PreCompact` / `PostCompact`                         | `heartbeat` {"compact"}                       |
| codex    | `SubagentStart`                                      | `subagent_start` {agent_id, agent_type, model} |
| codex    | `SubagentStop`                                       | `subagent_end` {agent_id}                     |
| codex    | `Stop`                                               | `turn_end` {last_assistant_message}           |
| opencode | root `session.created`                               | `session_start`                               |
| opencode | root `chat.message`                                  | `turn_start` {user text}                      |
| opencode | `session.status` busy/retry                           | `session_focus` / `heartbeat`                 |
| opencode | `tool.execute.before`                                | `heartbeat` {tool summary}                    |
| opencode | `permission.asked` / `permission.replied`            | `attention` approval / `heartbeat`            |
| opencode | `session.status` idle                                | `turn_end` {last assistant text}              |
| opencode | `session.error`                                      | `turn_error` {error type/message}             |
| opencode | root `session.deleted`                               | `session_end`                                 |
| pi       | new `session_start`                                  | `session_start` {model}                       |
| pi       | resumed/reloaded `session_start`                     | `session_focus`                               |
| pi       | `before_agent_start`                                 | `turn_start` {prompt}                         |
| pi       | `agent_start`                                        | — (clear prior retry outcome)                 |
| pi       | `tool_execution_start`                               | `heartbeat` {tool summary}                    |
| pi       | `turn_end` / `agent_end`                             | — (cache final assistant outcome)             |
| pi       | `agent_settled` after successful final message       | `turn_end` {last assistant text}              |
| pi       | `agent_settled` after final assistant error          | `turn_error` {error message}                  |
| pi       | `session_shutdown` except reload                     | `session_end`                                 |
| pi       | `pi.events` → `ui:prompt:opened`                     | `attention` (kind approval/question) {summary} |
| pi       | `pi.events` → `ui:prompt:closed`                     | `heartbeat` {outcome} (Attention → Working; may decay to Stale if no later event) |
| grok     | `SessionStart`                                       | `session_start` {model}                       |
| grok     | `UserPromptSubmit`                                   | `turn_start` {prompt}                         |
| grok     | `Notification` (`permission_prompt`)                 | `attention` approval {message / tool}         |
| grok     | `PreToolUse` (`ask_user_question\|exit_plan_mode`)   | `attention` question {first question / plan}  |
| grok     | `PostToolUse`                                        | `heartbeat` {tool summary}                    |
| grok     | `PreCompact` / `PostCompact`                         | `heartbeat` {"compact"}                       |
| grok     | `SubagentStart`                                      | `subagent_start` {agentId, agentType, model}  |
| grok     | `SubagentStop`                                       | `subagent_end` {agentId}                      |
| grok     | `Stop` (`end_turn` or no reason)                     | `turn_end` {lastAssistantMessage}             |
| grok     | `StopFailure`                                        | `turn_error` {error, errorDetails}            |
| grok     | `SessionEnd`                                         | `session_end`                                 |

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
tool progress updates (too noisy). Attention arrives through the cooperative
`ui:prompt:opened` / `ui:prompt:closed` bus convention rather than a
provider-wide event: the bridge validates each payload and rejects unknown
`kind` values, but never chooses which prompts count as Attention.

OpenCode deliberately ignores child-session events, streaming deltas, file
events, todos, and LSP events. It observes permission events but does not use the
decision-capable `permission.ask` hook, preserving the observer-only boundary.

Deliberately unsubscribed on Grok: `PostToolUseFailure` and `PermissionDenied`
(the agent is still working; a deny is not a wait); `Notification` types other
than `permission_prompt` (`tool_execution` is activity noise); the session-end
`Stop` (`reason: channel_closed` / `shutdown`) — `SessionEnd` is the boundary.
`PreToolUse` is subscribed only for the question tools; every-tool PreToolUse
would fire whether or not a permission prompt is showing.
