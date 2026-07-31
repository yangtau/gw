# Contributing

Thanks for helping improve gw. Bug reports, provider compatibility reports,
documentation fixes, and focused pull requests are welcome.

## Before opening a change

- Use GitHub Issues for public bug reports and feature proposals.
- Keep changes focused and explain the user-visible behavior they affect.
- Do not include hook payloads, transcripts, credentials, or private project
  paths in issues, tests, or screenshots.
- Discuss large protocol or architecture changes in an issue first.

## Development

gw supports macOS and Linux and requires a recent stable Rust toolchain. Run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Tests that change environment variables serialize through a shared lock. Avoid
adding tests that depend on a developer's real tmux server, provider config, or
home directory.

## Pull requests

Describe the problem, the chosen behavior, and how you verified it. Update the
README or files under `docs/` when behavior or the provider protocol changes.
By submitting a contribution, you agree that it is licensed under the MIT
License used by this repository.

The files under `.scratch/` and `docs/agents/` support maintainer planning and
automation. External contributors do not need to use that workflow.
