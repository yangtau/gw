# gw

A tmux-native status panel for coding agents. Summon one popup and see every
Claude / Codex / Amp / Pi / (your own) agent running across your tmux sessions — what
each is doing, which one is blocked on you — then press `enter` to jump straight
to it.

```
   Claude  ● approval  Run: kubectl apply -f deplo…  ~/code/api-gateway    · feat/rate-limiter      · 1:api    · 4m
   Amp     ✗ error     rate_limit: provider thrott…  ~/code/infra          · chore/staging-cluster  · 4:infra  · 5m
 ❯ Claude  ● working   Edit · Migrate dashboard to…  ~/code/web-dashboard  · feat/server-components · 2:web    · 6m
   ↳ Explore · haiku · map component tree · 6m
   ↳ Plan · sequence the migration · 5m
   Codex   ● done      Added idempotency keys + te…  ~/code/billing-worker · main                   · 3:worker · 4m

 ╭ web-dashboard: claude ─────────────────────────────────────────────────────────────────────────────────────────╮
 │  7m session    claude-sonnet-4                                                                                 │
 │  6m turn       Migrate dashboard to server components                                                          │
 │  6m subagent+  Explore · map component tree                                                                    │
 │  5m subagent+  Plan · sequence the migration                                                                   │
 │  3m tool       Edit                                                                                            │
 ╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
```

Each row is one agent: provider, status, one-line detail, and a dim right cluster
(working directory · git branch · tmux window · age). The most urgent agents sort
to the top. Selecting a row opens its **Activity** timeline below — the same event
stream that drives status — so you can tell what an agent has been up to without
leaving the panel.

## Why

Running several agents at once, the hard question is never "what are they all
doing" — it's "which one needs me right now". `gw` answers that in one keystroke:

- **Discovery-based** — any pane running a known agent CLI shows up, however it
  was started. tmux panes are the single source of truth; `gw` keeps no registry.
- **Hook-driven status** — Attention / Error / Stale / Working / Done / Idle,
  derived purely from provider hook events. No pane scraping, no key injection,
  no daemon. The event log is the only persistent state, and status is a pure
  function of replaying it.
- **Attention routing** — `a` jumps to the next agent blocked on you (a pending
  approval or question); attention also fires a desktop notification whether the
  panel is open or not.
- **Pluggable providers** — a provider is a standalone `gw-provider-<id>`
  executable speaking a small pure-translator protocol; private CLIs plug in from
  their own repositories. See [docs/protocol.md](docs/protocol.md).

## Status model

Statuses are **eventually consistent**: attention is cleared by later activity,
never by being acknowledged. Rows sort most-urgent-first in this order:

| Status | Meaning |
|---|---|
| **Attention** | Blocked mid-turn on you: a pending **approval** or a **question** the agent asked. |
| **Error** | The last turn aborted with a provider-reported failure (rate limit, billing, auth). |
| **Stale** | Working, but silent past a threshold — a suspected hang or quiet death. |
| **Working** | A turn is in progress. |
| **Done** | The last turn finished; its result awaits you. Cleared only by the next turn. |
| **Idle** | Alive with no active turn, or newly discovered with no events yet. |

Which statuses a provider can reach depends on the hook events it emits; the
model is sized to the richest provider and degrades gracefully per-provider.

## Setup

Via nix (installs `gw` and the official provider plugins):

```sh
nix profile install .        # or reference this repo as a flake input
```

Or with cargo:

```sh
cargo install --path crates/gw
cargo install --path crates/providers/claude
cargo install --path crates/providers/codex
cargo install --path crates/providers/amp
cargo install --path crates/providers/opencode
cargo install --path crates/providers/pi
```

Then install the provider hooks (backed up, surgical, reversible):

```sh
gw setup
```

`gw setup` detects which agent CLIs are available locally, shows the matching
provider integrations and target files, and asks for explicit confirmation
before changing anything. Use `gw setup --yes` for non-interactive installs and
`gw setup --remove` to remove installed integrations. Setup installs runtime
integrations only; it never installs agent skills.
Optionally install the `gw` skill through the open skills ecosystem:

```sh
npx skills add yangtau/gw --skill gw -g
```

To install it into several common agent skill directories non-interactively:

```sh
npx skills add yangtau/gw --skill gw -g -y \
  -a amp -a claude-code -a codex
```

Bind a key in `~/.tmux.conf` for the switcher posture — summon, pick, jump, gone:

```tmux
bind g popup -E 'GW_POPUP=1 gw'
```

`gw` runs equally well in a persistent pane (dashboard mode) or a `display-popup`
(switcher mode); the only difference is whether it exits after a jump.

It can also run from a terminal outside tmux when a tmux server is already
running. Select an Agent and press `enter`; `gw` restores the terminal, then
attaches it directly to that Agent's tmux session and pane.

## Keys

| key | action |
|---|---|
| `enter` | jump to the selected agent (popup closes) |
| `n` | launch a new agent in a new window |
| `f` | fork the selected agent into a new window (source pane untouched) |
| `r` | recently ended sessions — `enter` resumes |
| `tab` | toggle between the current and global tmux-session views |
| `a` | select the next agent needing attention |
| `j`/`k`, arrows | move |
| `?` | open the full keyboard-shortcuts page; press `?` or `esc` to return |
| `esc` | return/cancel, or quit from the main panel |
| `ctrl-c` | quit from anywhere |

Configure which events notify with `notify = ["attention", "turn_end"]`, or
disable them with `notify = false`, in `~/.config/gw/config.toml`. The starting
view (`current` or `global`) is configurable there too.

## Session referencing (CLI)

Beyond the panel, `gw` answers questions about sessions from the command line —
built for agents referencing other agents and equally usable by humans.
Sessions are addressed as `provider:session-id`; a bare id or unique prefix
(≥ 4 chars) works too. Ambiguous or unknown addresses are errors.

| command | what it does |
|---|---|
| `gw ls [--json]` | The address book: live agents (pane-bound) and ended-but-resumable sessions. |
| `gw show <addr> [--transcript] [--json]` | Status header plus the Activity timeline; `--transcript` emits the provider-native transcript. |
| `gw wait <addr> [--timeout <secs>]` | Bounded level-triggered wait: returns `done \| attention \| error \| stale \| idle \| ended \| timeout` (default 45s; `0` = single query). |
| `gw resume <addr> [prompt] [--fork]` | Relaunch an ended session in a new tmux window; `--fork` branches (required if the session is live). |

Everything is read-only over the event log — no pane scraping, no key
injection, no daemon. `resume` only ever starts a new process in a new window;
it never touches an existing pane.

## Layout

- `crates/gw` — the binary: panel TUI, `gw hook` ingest, `gw setup`.
- `crates/gw-core` — domain: discovery, correlation, Session interpretation
  (Status, Subagents, Activity), event store, tmux/ps wrappers.
- `crates/gw-plugin-protocol` — serde types of the plugin protocol, for Rust
  plugin authors.
- `crates/providers/claude`, `crates/providers/codex`, `crates/providers/amp`,
  `crates/providers/opencode`, `crates/providers/pi` — official provider plugins.
- `skills/gw` — optional agent guidance, installed separately with `npx skills`.

Amp support targets its interactive TUI. One Amp pane remains one gw Agent row,
tracking that TUI's foreground thread; runner/execute modes and background
threads are intentionally excluded. `gw setup` installs the observer plugin at
`~/.config/amp/plugins/gw.ts`; restart Amp or run `plugins: reload` in an
already-running TUI after setup.

Pi support likewise targets interactive mode and follows the TUI's current
Session across `/new`, `/resume`, `/fork`, and `/clone`. `gw setup` installs the
observer extension at `~/.pi/agent/extensions/gw.ts`; restart Pi or run `/reload` in an
already-running TUI after setup.

OpenCode support targets its interactive TUI; commands such as `run`, `serve`,
`web`, and `attach` are excluded from pane discovery. `gw setup` installs the
observer plugin at `~/.config/opencode/plugins/gw.ts`; restart OpenCode after setup.

Design notes live in [docs/design.md](docs/design.md); vocabulary in
[CONTEXT.md](CONTEXT.md); load-bearing decisions in [docs/adr/](docs/adr/).
