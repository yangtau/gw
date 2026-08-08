# gw

A tmux-native status panel for coding agents. Summon one popup and see every
Claude / Codex / Amp / OpenCode / Pi / (your own) agent running across your tmux sessions —
then press `enter` to jump straight to it.

```
   Claude   ~/code/api-gateway    · feat/rate-limiter      · 1:api
   Amp      ~/code/infra          · chore/staging-cluster  · 4:infra
 ❯ Claude   ~/code/web-dashboard  · feat/server-components · 2:web
   Codex    ~/code/billing-worker · main                   · 3:worker
```

Each row is one agent: provider label and a dim right cluster
(working directory · git branch · tmux window). Selecting a row opens its
detail below.

## Why

Running several agents at once, you need one place to see them all and jump
between them:

- **Discovery-based** — any pane running a known agent CLI shows up, however it
  was started. tmux panes are the single source of truth; `gw` keeps no registry.
- **Pluggable providers** — a provider is a standalone `gw-provider-<id>`
  executable that prints a static manifest describing how to recognize and
  launch its CLI. See [docs/protocol.md](docs/protocol.md).

## Requirements

gw supports macOS and Linux. It requires `tmux`, a provider CLI, and the
matching `gw-provider-*` executable on `PATH`. Process discovery also uses
standard Unix `ps` and `lsof` commands. Windows is not supported.

The project is currently pre-1.0. CLI and configuration compatibility may
still change between releases.

## Install

There are no tagged binary releases yet. Install the latest development version
directly from GitHub with Nix (installs `gw` and all official provider plugins):

```sh
nix profile install github:yangtau/gw
```

Or clone the repository and install with Cargo:

```sh
git clone https://github.com/yangtau/gw.git
cd gw
cargo install --path crates/gw
cargo install --path crates/providers/claude
cargo install --path crates/providers/codex
cargo install --path crates/providers/amp
cargo install --path crates/providers/opencode
cargo install --path crates/providers/pi
```

Bind a key in `~/.tmux.conf` for the switcher posture — summon, pick, jump, gone:

```tmux
bind g popup -E 'GW_POPUP=1 gw'
```

`gw` runs equally well in a persistent pane (dashboard mode) or a `display-popup`
(switcher mode); the only difference is whether it exits after a jump.

It can also run from a terminal outside tmux when a tmux server is already
running. Select an Agent and press `enter`; `gw` restores the terminal, then
attaches it directly to that Agent's tmux session and pane. Detaching returns
to the same `gw` panel.

## Keys

| key | action |
|---|---|
| `enter` | jump to the selected agent (popup closes) |
| `n` | launch a new agent in a new window |
| `f` | fork the selected agent into a new window (source pane untouched) |
| `r` | recently ended sessions — `enter` resumes |
| `tab` | toggle between the current and global tmux-session views |
| `j`/`k`, arrows | move |
| `?` | open the full keyboard-shortcuts page; press `?` or `esc` to return |
| `esc` | return/cancel, or quit from the main panel |
| `ctrl-c` | quit from anywhere |

The starting view (`current` or `global`) is configurable in
`~/.config/gw/config.toml`.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. gw is
available under the [MIT License](LICENSE).

## Layout

- `crates/gw` — the binary: panel TUI.
- `crates/gw-core` — domain: discovery, tmux/ps wrappers, launch templates.
- `crates/gw-plugin-protocol` — serde types of the plugin manifest, for Rust
  plugin authors.
- `crates/providers/claude`, `crates/providers/codex`, `crates/providers/amp`,
  `crates/providers/opencode`, `crates/providers/pi` — official provider plugins.

Design notes live in [docs/design.md](docs/design.md); vocabulary in
[CONTEXT.md](CONTEXT.md); load-bearing decisions in [docs/adr/](docs/adr/).
