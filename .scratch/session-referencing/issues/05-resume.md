# 05 gw resume

Status: resolved

`gw resume <addr> [prompt] [--fork]`. Gating: resume/resume_prompt only for
ended targets (live -> error, suggest --fork); fork independent, no prompt
in v1; missing capability -> clear per-provider error. Launch via
tmux::new_window with {session_id}/{prompt}/{cwd} expansion, cwd from meta.

## Comments

Implemented: `gw resume <addr> [prompt] [--fork]` with independent capability
templates expanded by shared `gw_core::launch::expand_argv` ({session_id},
{prompt}, {cwd}; prompt-less templates drop `{prompt}` args). Plain resume
refuses a live target (points at --fork when available); fork allows live;
`--fork` + prompt rejected (v1). Launch = new tmux window only. TUI resume
now reuses the same expansion helper. Provider commands verified against
docs/help: `claude --resume <id> [prompt]` / `--fork-session`,
`codex resume <id> [prompt]` / `codex fork <id>` (TUI command — fine in a
tmux window), `amp threads continue <id>`. Smoke-tested fork launch in a
real tmux server.
