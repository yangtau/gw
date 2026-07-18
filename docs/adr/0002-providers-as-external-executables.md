# ADR 0002: Providers are external executables speaking a pure-translator protocol

## Status

Accepted (2026-07-13)

## Context

Supported providers include private, internal CLIs (traex) whose hook payload schemas must not appear in this public codebase. Candidate plugin shapes: declarative TOML manifests (field-mapping DSL grows into a bad programming language once payload differences get non-trivial), compiled-in provider traits for official providers plus an external path for private ones (two implementation paths, protocol becomes second-class), or external executables for everyone.

A second axis: who writes the event log. If hook configs invoke the plugin directly, log paths, locking, and the JSONL format all become plugin-protocol surface that every plugin must reimplement and the core can never change.

## Decision

A provider is a standalone executable named `gw-provider-<id>`, discovered on PATH / in the plugin directory. **All** providers go through this protocol — the claude, codex, and amp plugins ship with `gw` and are built in the same workspace, but get no private fast path.

Plugins are pure translators with no side effects:

- `manifest` — prints static description: process match rules, launch command, hook/config and managed-file install specs.
- `normalize` — reads one provider hook payload on stdin, prints unified events on stdout.

Hook commands installed into provider configs—or observer bridges installed as hash-protected managed files—invoke the **core** (`gw hook <provider>`); the core spawns the plugin's `normalize` and owns all integration-file I/O, event-log writing, notification side effects, and storage layout.

## Consequences

- The internal traex repository implements two small pure subcommands in any language, against a protocol whose living documentation and test suite are the official plugins (dogfooding guarantees the protocol suffices).
- Storage format and paths stay private to the core and can evolve freely.
- Each hook event costs one extra process spawn (hooks already spawn a process; milliseconds, negligible).
- The protocol needs versioning from day one (`manifest` carries a protocol version).
