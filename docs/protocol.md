# Provider plugin protocol (v1)

A provider plugin is a standalone executable named `gw-provider-<id>`, found on `PATH` or in `~/.config/gw/providers/bin/`. Plugins are **pure translators**: no file I/O, no network, no side effects. The core owns all storage, notification, and provider-config editing.

Rust plugins can use the `gw-plugin-protocol` crate for the types below; the protocol itself is plain JSON over stdin/stdout, so any language works.

## Subcommands

### `manifest`

Prints a single JSON object describing the provider statically:

```json
{
  "protocol": 1,
  "id": "claude",
  "label": "Claude",
  "color": "#D97757",
  "process": { "argv0": ["claude"], "exclude_args": [] },
  "launch": { "argv": ["claude"] },
  "resume": { "argv": ["claude", "--resume", "{session_id}"] },
  "hooks": [
    {
      "path": "~/.claude/settings.json",
      "format": "json",
      "patches": [
        {
          "pointer": "/hooks/Stop",
          "mode": "ensure",
          "value": { "hooks": [{ "type": "command", "command": "gw hook claude" }] }
        }
      ]
    }
  ],
  "managed_files": []
}
```

- `protocol` — protocol version; the core rejects manifests with a version it doesn't support.
- `process.argv0` — an agent process is recognized when the basename of one of its first few argv tokens equals one of these (the wider window tolerates wrappers such as `node /path/to/claude`).
- `process.exclude_args` — optional exact argv tokens that disqualify a process after its executable matches. Amp uses this to exclude `--no-tui`, `-x`, and `--execute`.
- `launch.argv` / `resume.argv` — command templates. Placeholders expanded by the core: `{session_id}`, `{cwd}`.
- `hooks` — declarative install spec, applied by `gw setup`:
  - `pointer` is a JSON-Pointer-style path (for TOML it addresses nested tables).
  - `mode: "ensure"` — the pointer addresses an array; setup guarantees it contains `value` (deep equality, no duplicates) and removes exactly that element on uninstall.
  - `mode: "set"` — setup writes `value` at the pointer (e.g. a feature flag) and leaves it in place on uninstall.
  - Setup edits are surgical: unrelated keys, ordering, and (for TOML) formatting are preserved; the target file is backed up before the first write; the operation is idempotent.
- `managed_files` — optional whole files needed to connect a provider that has no JSON/TOML hook config. Each entry has `path`, `content`, and a single-line `comment_prefix`. The core prepends an ownership header containing a hash of `content`; setup upgrades or removes the file only when that header belongs to the same provider and the body still matches its hash. Unrelated or user-modified files are rejected, never overwritten. Amp uses this to install its system TypeScript observer plugin.

### `normalize`

Reads **one** raw hook payload (whatever the provider POSTs to its hook command) on stdin and prints zero or more unified events, one JSON object per line:

```json
{"v":1,"session":"<native-session-id>","kind":"turn_start"}
{"v":1,"session":"<native-session-id>","kind":"attention","attention":"approval","summary":"Bash: rm -rf build"}
```

- `session` — the provider-native session id extracted from the payload. Required.
- `ts` — RFC 3339 timestamp; optional, the core stamps arrival time when absent.
- `kind` — one of:

| kind | extra fields | meaning |
|---|---|---|
| `session_focus` | | this provider-native session became the pane's foreground session; updates correlation but is status-neutral |
| `session_start` | `model?` | a native session began |
| `turn_start` | `summary?` (prompt excerpt) | the agent started working on a turn |
| `heartbeat` | `activity?` (e.g. tool name) | still working (e.g. after each tool use); emit sparingly |
| `attention` | `attention`: `approval` \| `question`, `summary?` | blocked mid-turn on the user: a permission dialog (`approval`) or an explicit question (`question`) |
| `turn_end` | `summary?` (final message excerpt) | the turn finished normally |
| `turn_error` | `reason?` (e.g. provider error type), `summary?` | the turn aborted with a provider-reported failure |
| `subagent_start` | `agent` (provider-native subagent id), `agent_type?`, `model?`, `summary?` (task excerpt) | a subagent spawned inside this session started running |
| `subagent_end` | `agent` | that subagent finished |
| `session_end` | | the native session ended |

Excerpt fields (`summary`, `activity`) are display one-liners; plugins truncate them (~120 chars) — the core stores what it is given. Every field beyond `session` and `kind` is optional (exception: `subagent_start`/`subagent_end` require `agent` — without an id there is nothing to correlate): emit what the provider knows, omit what it doesn't.

Subagent events use the **parent's** native session id and are status-neutral: they never clear Attention, revive Done, or affect staleness. The panel replays start/end pairs into the running-subagent list shown under the agent's row; a `session_start`/`session_end` clears the list.

Unknown payloads must produce zero events and exit 0 — never fail the provider's hook.

## How the core drives plugins

- Provider hook configs or managed observer plugins invoke `gw hook <id>`; the core pipes the payload to the plugin's `normalize`, stamps missing timestamps, correlates the event to a tmux pane (ppid-chain walk from the hook process to the provider process, tty → pane), and appends to its event log.
- Manifests are consulted for discovery (pane scanning), `n`/launch, resume, and `gw setup`.

## Versioning

Bump `protocol` only on breaking changes. Adding event kinds or optional fields is **not** breaking: both when ingesting plugin output and when replaying a log, the core skips lines it cannot parse, so vocabulary can evolve without migrating stored events or lock-stepping plugins. The core supports the current version; plugins should print a clear error on `manifest` if invoked by an incompatible core.
