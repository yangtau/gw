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
  "process": { "argv0": ["claude"] },
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
  ]
}
```

- `protocol` — protocol version; the core rejects manifests with a version it doesn't support.
- `process.argv0` — an agent process is recognized when its argv[0] **basename** equals one of these.
- `launch.argv` / `resume.argv` — command templates. Placeholders expanded by the core: `{session_id}`, `{cwd}`.
- `hooks` — declarative install spec, applied by `gw setup`:
  - `pointer` is a JSON-Pointer-style path (for TOML it addresses nested tables).
  - `mode: "ensure"` — the pointer addresses an array; setup guarantees it contains `value` (deep equality, no duplicates) and removes exactly that element on uninstall.
  - `mode: "set"` — setup writes `value` at the pointer (e.g. a feature flag) and leaves it in place on uninstall.
  - Setup edits are surgical: unrelated keys, ordering, and (for TOML) formatting are preserved; the target file is backed up before the first write; the operation is idempotent.

### `normalize`

Reads **one** raw hook payload (whatever the provider POSTs to its hook command) on stdin and prints zero or more unified events, one JSON object per line:

```json
{"v":1,"session":"<native-session-id>","kind":"turn_start"}
{"v":1,"session":"<native-session-id>","kind":"attention","attention":"approval","summary":"Bash: rm -rf build"}
```

- `session` — the provider-native session id extracted from the payload. Required.
- `ts` — RFC 3339 timestamp; optional, the core stamps arrival time when absent.
- `kind` — one of:

| kind | meaning |
|---|---|
| `session_start` | a native session began |
| `turn_start` | the agent started working on a turn |
| `turn_end` | the turn finished; agent is idle |
| `attention` | the agent needs the user; `attention` is `approval` \| `question` \| `notification`, `summary` is an optional one-liner |
| `heartbeat` | still working (e.g. after each tool use); emit sparingly |
| `session_end` | the native session ended |

Unknown payloads must produce zero events and exit 0 — never fail the provider's hook.

## How the core drives plugins

- Provider hook configs invoke `gw hook <id>`; the core pipes the payload to the plugin's `normalize`, stamps missing timestamps, correlates the event to a tmux pane (ppid-chain walk from the hook process to the provider process, tty → pane), and appends to its event log.
- Manifests are consulted for discovery (pane scanning), `n`/launch, resume, and `gw setup`.

## Versioning

Bump `protocol` only on breaking changes. The core supports the current version; plugins should print a clear error on `manifest` if invoked by an incompatible core.
