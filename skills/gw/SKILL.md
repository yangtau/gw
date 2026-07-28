---
name: gw
description: References other coding-agent sessions on this machine. Use when asked to check on, read, wait for, or continue another agent session (Claude Code, Codex, Amp), or to coordinate work across parallel agent sessions.
compatibility: Requires gw on PATH with provider hooks installed by gw setup.
---

# gw: session referencing

`gw` observes coding-agent sessions on this machine through provider hooks.
Sessions are addressed as `provider:session-id` (for example
`claude:279b0f33-…`); a bare session id or a unique prefix of at least 4
characters also works. Ambiguous or unknown addresses are errors — list
first, then reference.

Only sessions gw has observed are visible; this is not the provider's full
session universe.

## List sessions

```
gw ls --json
```

Prints `{"agents":[…],"sessions":[…]}`: `agents` are live, pane-bound
sessions with a current status (`working`, `approval`, `question`, `error`,
`stale`, `done`, `idle`); `sessions` ended but may be resumable.

## Read a session

```
gw show <addr> --json      # status, cwd, activity timeline
gw show <addr> --transcript  # provider-native transcript (JSONL or Markdown)
```

## Wait for a session (bounded, level-triggered)

```
gw wait <addr> --timeout 45 --json
```

Returns immediately if the target is already settled; otherwise blocks until
it settles or the timeout expires. `result` is one of: `done`, `attention`,
`error`, `stale`, `idle`, `ended`, `timeout`.

Rules:

- Never wait unbounded. Use a loop: wait with a short timeout, on `timeout`
  re-check and decide whether to keep waiting or do other work.
- `attention` means the session needs a human (approval or question) — tell
  the user; do not treat it as completion.
- `--timeout 0` queries once without blocking.
- Waiting on your own session is rejected.

## Continue a session

```
gw resume <addr>             # reopen an ended session in a new tmux window
gw resume <addr> "<prompt>"  # with an initial prompt (where supported)
gw resume <addr> --fork      # branch into a new session; allowed while live
```

Plain resume refuses a live target; fork (where the provider supports it)
branches without disturbing the running agent. Capabilities differ per
provider — the error messages say what is supported.
