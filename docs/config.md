# gw configuration

gw reads an optional TOML file at `~/.config/gw/config.toml` (override the config
directory with `GW_CONFIG_DIR`, mirroring `GW_STATE_DIR` for the store). The file is
entirely optional: a missing file, a missing section, or a missing key all fall back
to defaults. A malformed file is reported on stderr and then treated as absent —
config never blocks a hook or a panel launch.

```toml
[debug]
hooks = true
```

## `[debug]`

| Key     | Type | Default | Effect                                                             |
| ------- | ---- | ------- | ------------------------------------------------------------------ |
| `hooks` | bool | `false` | Dump every incoming hook payload alongside its normalized events.  |

### `debug.hooks` — hook payload dump

When on, `gw hook <provider>` writes one JSONL record per incoming payload, pairing
the **raw hook payload** with the **normalized events** it produced. This is the tool
for debugging the provider-hook → gw-event mapping (`docs/provider-hooks.md`) against
real payloads — including payloads that map to nothing, which are otherwise dropped
silently.

It is a manual switch: turn it on, reproduce, read the files, turn it off. There is
no rotation or size cap.

**Where records land** (under the store dir, `~/.local/state/gw/sessions/`):

- Payloads that normalize to at least one event → `<stem>.debug.jsonl`, where
  `<stem> = <date>-<cmd>-<provider>-<sid>` (`<cmd>` is the innermost directory
  of the agent's cwd, `<sid> = sha256("<provider>:<session>")[..16]`) — the same
  stem as the session's `.jsonl` / `.meta.json`, so the three files sit side by
  side. This file is swept together with its session once the session ages out.
- Payloads that normalize to **zero** events, or where the provider plugin itself
  errored (no session to attribute) → `_unmapped.debug.jsonl`. Never swept.

**Record shape** (one JSON object per line):

| Field         | When                          | Value                                                    |
| ------------- | ----------------------------- | -------------------------------------------------------- |
| `ts`          | always                        | RFC3339 UTC, stamped by gw at ingest                     |
| `provider`    | always                        | provider id (`claude` / `codex` / `agy`)                 |
| `events`      | normalize succeeded           | the normalized events verbatim (their `ts` is `null`); `[]` if empty |
| `error`       | normalize failed              | the error string; replaces `events`                      |
| `payload`     | raw payload is valid JSON     | the payload embedded as JSON (so it is `jq`-able)        |
| `payload_raw` | raw payload is not valid JSON | UTF-8 lossy string; replaces `payload`                   |

`events` is the mapping output as produced by the provider plugin — before the store
stamps timestamps or throttles heartbeats — so it reflects the mapping logic, not the
stored result.

Examples:

```json
{"ts":"2026-07-14T09:00:00Z","provider":"agy","events":[{"v":1,"session":"c1","kind":"turn_start"}],"payload":{"conversationId":"c1","invocationNum":1}}
{"ts":"2026-07-14T09:00:01Z","provider":"agy","events":[],"payload":{"conversationId":"c1","unknownField":9}}
{"ts":"2026-07-14T09:00:02Z","provider":"claude","error":"gw-provider-claude normalize failed: ...","payload_raw":"not json"}
```

Reading:

```sh
# Follow all mappings as they happen
tail -f ~/.local/state/gw/sessions/*.debug.jsonl ~/.local/state/gw/sessions/_unmapped.debug.jsonl

# Only one provider
grep '"provider":"agy"' ~/.local/state/gw/sessions/*.debug.jsonl

# Payloads that mapped to nothing — the interesting ones
jq 'select(.events == [])' ~/.local/state/gw/sessions/_unmapped.debug.jsonl
```
