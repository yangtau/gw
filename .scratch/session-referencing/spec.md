# Session referencing v1: ls / show / wait / resume

Status: implemented

Give agents (and humans) an Amp-threads-style ability to reference other
coding agent Sessions through gw CLI primitives, read-only plus launch —
no pane scraping, no key injection, no daemon. Design agreed in Amp thread
T-019fa3ab-5b51-73d8-9304-8b9a561c81e9.

## Invariants preserved

- Never scrape pane content, never inject keys, no daemon.
- Plugins stay pure translators; the core owns all file writes.
- Dynamic Status stays a pure replay of **provider** events. The Event Log
  additionally admits **core-written operational annotations**
  (`wait_start`/`wait_end`), which are status-neutral — same class as
  subagent/focus events.

## Address model

Canonical address: `provider:session-id` (e.g. `claude:279b0f33-…`,
`amp:T-0199…`). A bare id (or unique id prefix, ≥ 4 chars) is accepted only
when it matches exactly one known Session. Scope is honest: sessions gw has
observed via hooks — not the provider's full universe.

## Commands

### `gw ls [--json]`

The session address book. Distinguishes live **Agents** (pane-bound) from
ended but resumable **Sessions**. `--json` prints one object:
`{"agents":[…],"sessions":[…]}` with address, provider, session id, status,
detail, cwd, tmux location (agents), ended_at (sessions).

### `gw show <addr> [--transcript] [--json]`

Default: header (provider, session, status, cwd) plus the Activity timeline
replayed from the Event Log. `--transcript` emits the provider's native
transcript (content is provider-native by contract: claude/codex JSONL, amp
Markdown; normalization deferred to v2). Locator resolution order:

1. hook-captured `transcript_path` (Event gains an optional `transcript`
   field; the store records the latest into the meta sidecar),
2. manifest `transcript` argv template (amp: `amp threads markdown {session_id}`),
3. manifest `transcript_glob` fallback (`{session_id}` placeholder, newest match).

### `gw wait <addr> [--timeout <secs>]`

Level-triggered bounded wait: evaluate immediately, block only while the
target is alive and Working. Result enum:
`done | attention | error | stale | idle | ended | timeout` — Attention is a
distinct result so the waiter fetches a human instead of treating it as
completion. Default timeout 45s (below every provider's tool-timeout floor:
amp 60s / claude 120s / codex 600s); `--timeout 0` = single query; self-wait
rejected. Skills teach a bounded wait-recheck loop, never an unbounded block.

Wake sources: 1s poll of the target's event log (covers fs change, the Stale
timer, which is time-derived with no event) plus a periodic process-liveness
check (codex can die silently leaving no event). Note: the agreed plan named
fs-watch; a 1s poll is equivalent for a CLI and avoids a watcher thread.

Waiting-on visualization: the core appends paired
`wait_start{wait_id,target}` / `wait_end{wait_id,outcome}` events to the
**waiter's** Event Log (waiter identity via the existing ppid ancestor-chain
mechanism). Status-neutral. Replayed into a `waiting_on` list; leftover edges
(missed wait_end) are cleared by the waiter's next provider event as a
fallback. v1 visualization: entries in the Activity timeline.

### `gw resume <addr> [prompt] [--fork]`

Capability-gated relaunch in a new tmux window (same mechanism as panel
resume). The manifest splits into three independent optional capability
templates — model on the richest provider, degrade per provider:

| capability | claude | codex | amp |
|---|---|---|---|
| `resume` (ended only) | `claude --resume {session_id}` | `codex resume {session_id}` | `amp threads continue {session_id}` |
| `resume_prompt` (ended only) | `claude --resume {session_id} {prompt}` | `codex resume {session_id} {prompt}` | — |
| `fork` (independent; live target allowed) | `claude --resume {session_id} --fork-session` | `codex fork {session_id}` | — (amp removed fork) |

Plain resume refuses a live target (claude double-resume interleaves one
transcript; codex refuses with "already has an active writer") and points at
`--fork` where available. v1 fork takes no prompt.

## Protocol changes (all additive, protocol stays v1)

- `Manifest`: `resume_prompt`, `fork`, `transcript` (argv templates),
  `transcript_glob` (glob template) — all optional.
- `Event`: optional `transcript` field (provider-native transcript path);
  the SDK extracts `transcript_path` generically from hook payloads.
- `EventKind`: `wait_start {wait_id, target}`, `wait_end {wait_id, outcome}` —
  core-written, status-neutral.
- `ManagedFile`: optional `comment_suffix` so the ownership header can be a
  closed HTML comment (`<!-- … -->`) inside Markdown skill files.

## Skill injection

`gw setup` installs a gw-owned standalone skill file per provider via the
existing `managed_files` ownership mechanism (never a user-shared file like
`~/.codex/AGENTS.md`, never the deprecated amp toolbox):

- claude: `~/.claude/skills/gw/SKILL.md`
- codex: `~/.agents/skills/gw/SKILL.md` (official USER scope)
- amp: `~/.config/amp/skills/gw/SKILL.md`

One shared skill body teaching: address other sessions via `gw ls`, read via
`gw show`, flow-control via bounded `gw wait` loops, continue work via
`gw resume`. Known risk: the ownership header occupies line 1, so YAML
frontmatter starts at line 2; agents fall back to directory-name +
first-paragraph metadata if their frontmatter parser requires line 1.

## Deferred to v2

In-place steer (pane injection), approval channel, transcript
normalization, `forked_from` lineage, richer waiting-on panel edges.

## Issues

1. `01-protocol-extensions` — manifest/event/managed-file protocol additions
2. `02-address-and-ls` — address parse/resolve + `gw ls`
3. `03-show` — timeline + transcript locator chain
4. `04-wait` — wait loop, wait events, status-neutral derivation, activity
5. `05-resume` — capability gating + launch
6. `06-skill-injection` — shared SKILL.md + per-provider managed files
7. `07-docs` — protocol.md, CONTEXT.md wording, ADR 0005
