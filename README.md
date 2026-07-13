# gw

A tmux-native status panel for coding agents. One popup shows every claude / codex / (your own) agent running in the current tmux session, what state it's in, and jumps you to the one that needs you.

- **Discovery-based** — any pane running a known agent CLI shows up, however it was started.
- **Hook-driven status** — Attention / Working / Idle / Stale, derived purely from provider hook events. No pane scraping, no key injection, no daemon.
- **Pluggable providers** — a provider is a standalone `gw-provider-<id>` executable speaking a small pure-translator protocol; private CLIs plug in from their own repositories. See [docs/protocol.md](docs/protocol.md).

## Setup

```sh
cargo install --path crates/gw --path crates/gw-provider-claude --path crates/gw-provider-codex
gw setup        # installs hooks into provider configs (backed up, surgical, reversible)
```

Bind a key in `~/.tmux.conf` for the switcher posture:

```tmux
bind g popup -E 'GW_POPUP=1 gw'
```

## Keys

| key | action |
|---|---|
| `enter` | jump to the selected agent (popup closes) |
| `n` | launch a new agent in a new window |
| `r` | recently ended sessions — `enter` resumes |
| `tab` | select the next agent needing attention |
| `j`/`k` | move |
| `q` | quit |

Attention events also fire a desktop notification, panel open or not.

## Layout

- `crates/gw` — the binary: panel TUI, `gw hook` ingest, `gw setup`.
- `crates/gw-core` — domain: discovery, correlation, status derivation, event store, tmux/ps wrappers.
- `crates/gw-plugin-protocol` — serde types of the plugin protocol, for Rust plugin authors.
- `crates/gw-provider-claude`, `crates/gw-provider-codex` — official provider plugins.

Design notes live in [docs/design.md](docs/design.md); vocabulary in [CONTEXT.md](CONTEXT.md); load-bearing decisions in [docs/adr/](docs/adr/).
