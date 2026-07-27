# 03 gw show

Status: resolved

`gw show <addr> [--transcript] [--json]`. Timeline from interpret().
Transcript locator chain: meta.transcript_path -> manifest.transcript argv
-> transcript_glob newest match. Provider-native output contract.

## Comments

Implemented: `gw show <addr> [--transcript] [--json]`. Default output is
header + activity timeline replayed by `session::interpret`; JSON includes
status/since/detail/ended/cwd/transcript_path/waiting_on/activity.
`--transcript` resolves hook-captured path → manifest `transcript` command →
`transcript_glob` (newest match; homegrown single-`*` glob with tests).
Verified: claude glob `~/.claude/projects/*/{session_id}.jsonl`, codex
`~/.codex/sessions/*/*/*/rollout-*{session_id}.jsonl`, amp
`amp threads markdown {session_id}` (checked against `amp threads --help`).
