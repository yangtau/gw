# ADR 0005: Read-only session referencing (ls / show / wait / resume)

## Status

Accepted (2026-07-27)

## Context

Agents (and humans) want to reference other coding-agent Sessions the way Amp
threads reference each other: list them, read their state, wait for them to
settle, and continue them. The candidate mechanisms ranged from pane scraping
and key injection (steering a live agent in place) to a coordination daemon.
All of those would break the invariants locked in ADR 0001: no pane content is
ever read, no keys are ever injected, and there is no daemon.

## Decision

Session referencing is **read-only plus launch**, built entirely on state gw
already owns:

- `gw ls` lists live Agents and ended, resumable Sessions from the existing
  discovery snapshot and event-log store.
- `gw show <addr>` replays the target's Event Log into the same
  status/activity interpretation the panel uses; `--transcript` emits the
  provider-native transcript located via hook-captured path → manifest
  `transcript` command → manifest `transcript_glob` (normalization deferred).
- `gw wait <addr>` is a level-triggered bounded wait: evaluate immediately,
  block only while the target is alive and Working, poll the target's event
  log (1s) plus a process-liveness check for silent deaths, and return a
  result word (`done | attention | error | stale | idle | ended | timeout`).
  Attention is a distinct result so a waiter fetches a human instead of
  treating it as completion. The default timeout (45s) sits below every
  provider's tool-timeout floor; skills teach a bounded wait-recheck loop.
- `gw resume <addr> [prompt] [--fork]` relaunches via manifest capability
  templates (`resume` / `resume_prompt` / `fork`) in a **new** tmux window —
  the same mechanism as panel resume. Plain resume refuses a live target;
  fork may branch one. Existing panes are never touched.

Sessions are addressed as `provider:session-id`, with bare ids and unique
prefixes (≥ 4 chars) accepted; ambiguity is an error, never a first match.

The Event Log gains **core-written operational annotations**: `gw wait`
appends paired `wait_start`/`wait_end` events to the *waiter's* log (waiter
identity via the existing ppid ancestor chain). These are status-neutral —
the same class as subagent/focus events — so the ADR 0001 invariant stands:
dynamic Status remains a pure replay of provider events only.

An agent-facing `gw` skill (shared `SKILL.md` body) is installed per provider
through the existing `managed_files` ownership mechanism at each provider's
global skill path; `comment_suffix` lets the ownership header be a closed
HTML comment inside Markdown.

## Consequences

- No new state, no daemon, no pane coupling: every command is a pure read of
  the store/topology, except `resume`, which only starts a new process.
- Historic Sessions are first-class: anything ever observed via hooks can be
  shown and (capability permitting) resumed, not just live panes.
- `wait` is honest about its blind spots: a provider that dies silently is
  caught by the liveness check; one that never located a pid falls back to
  events and the timeout.
- In-place steering, an approval channel, transcript normalization, and
  `forked_from` lineage are explicitly deferred (v2); the address model and
  result vocabulary are designed to survive those additions.
- The skill's ownership header occupies line 1 of `SKILL.md`, so YAML
  frontmatter starts at line 2; agents whose frontmatter parser requires
  line 1 fall back to directory-name + first-paragraph metadata.
