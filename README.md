# gw

A tmux-native status panel for coding agents. One popup shows every Claude / Codex / Amp / (your own) agent running in tmux, what state it's in, and jumps you to the one that needs you.

- **Discovery-based** — any pane running a known agent CLI shows up, however it was started.
- **Hook-driven status** — Attention / Working / Idle / Stale, derived purely from provider hook events. No pane scraping, no key injection, no daemon.
- **Pluggable providers** — a provider is a standalone `gw-provider-<id>` executable speaking a small pure-translator protocol; private CLIs plug in from their own repositories. See [docs/protocol.md](docs/protocol.md).

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
```

Then install the provider hooks (backed up, surgical, reversible):

```sh
gw setup
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
| `tab` | toggle between the current and global tmux-session views |
| `a` | select the next agent needing attention |
| `j`/`k`, arrows | move |
| `?` | open the full keyboard-shortcuts page; press `?` or `esc` to return |
| `esc` | return/cancel, or quit from the main panel |
| `ctrl-c` | quit from anywhere |

Attention events also fire a desktop notification, panel open or not. Configure
which events notify with `notify = ["attention", "turn_end"]`, or disable them with
`notify = false`, in `~/.config/gw/config.toml`.

## Layout

- `crates/gw` — the binary: panel TUI, `gw hook` ingest, `gw setup`.
- `crates/gw-core` — domain: discovery, correlation, status derivation, event store, tmux/ps wrappers.
- `crates/gw-plugin-protocol` — serde types of the plugin protocol, for Rust plugin authors.
- `crates/providers/claude`, `crates/providers/codex`, `crates/providers/amp` — official provider plugins.

Amp support targets its interactive TUI. One Amp pane remains one gw Agent row,
tracking that TUI's foreground thread; runner/execute modes and background
threads are intentionally excluded. `gw setup` installs the observer plugin at
`~/.config/amp/plugins/gw.ts`; restart Amp or run `plugins: reload` in an
already-running TUI after setup.

Design notes live in [docs/design.md](docs/design.md); vocabulary in [CONTEXT.md](CONTEXT.md); load-bearing decisions in [docs/adr/](docs/adr/).
