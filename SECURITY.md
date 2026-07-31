# Security Policy

## Reporting a vulnerability

Please report vulnerabilities through GitHub's private vulnerability reporting
for this repository. Do not open a public issue containing credentials, hook
payloads, transcripts, local paths, or an unreleased exploit.

gw is currently pre-1.0. Security fixes are made on the latest `main` branch.

## Trust and data boundaries

- Provider plugins are executable programs discovered from `GW_PLUGIN_DIR`,
  `~/.config/gw/providers/bin/`, and `PATH`. Install plugins only from sources
  you trust. gw executes matching `gw-provider-*` programs to read manifests
  and normalize hook payloads.
- `gw setup` modifies provider configuration only after confirmation, unless
  `--yes` is supplied. It creates backups before changing shared config files.
- Session event logs are stored under `~/.local/state/gw/` by default and may
  contain summaries, working directories, session IDs, and transcript paths.
- `debug.hooks = true` stores raw provider payloads. These may contain prompts,
  responses, commands, source paths, or other sensitive data. Use this setting
  only briefly and delete debug files when finished.
- `gw show --transcript` prints provider-native transcripts, which may contain
  sensitive source code or conversation content.
