# 07 Docs

Status: resolved

protocol.md: new manifest fields, event transcript field, wait_* kinds,
comment_suffix. CONTEXT.md: Event Log admits core-written operational
annotations; dynamic Status derived only from provider events (locked).
docs/adr/0005: read-only session referencing. config.md if flags added.

## Comments

Implemented: protocol.md documents the new manifest capability/transcript
fields, `{prompt}` expansion, Event `transcript`, `wait_start`/`wait_end`
(core-written, status-neutral) and `comment_suffix`; CONTEXT.md Event Log
entry states the operational-annotation admission with the locked
provider-events-only Status invariant, and adds an Address term; new
docs/adr/0005-read-only-session-referencing.md. config.md untouched (no new
config keys).

Follow-up: README gains a "Session referencing (CLI)" section documenting
`gw ls/show/wait/resume`, addressing, wait result words, and the read-only /
new-window-only guarantees. Provider manifests re-verified against upstream:
codex `resume <SESSION_ID> [PROMPT]` and `fork <SESSION_ID>` confirmed in
openai/codex codex-rs/cli/src/main.rs (ResumeCommand/ForkCommand + flattened
TUI prompt positional).
