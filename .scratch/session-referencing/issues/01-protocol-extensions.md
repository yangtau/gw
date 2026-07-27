# 01 Protocol extensions

Status: resolved

Additive changes to `gw-plugin-protocol` (protocol stays v1):

- `Manifest`: `resume_prompt: Option<Command>`, `fork: Option<Command>`,
  `transcript: Option<Command>`, `transcript_glob: Option<String>`.
- `Event`: `transcript: Option<String>` (skip_serializing_if none).
- `EventKind`: `WaitStart { wait_id, target }`, `WaitEnd { wait_id, outcome }`.
- `ManagedFile`: `comment_suffix: String` (serde default empty).

Fixups: SDK `normalize` passes `transcript_path` through generically;
`setup.rs` renders/validates the suffix; all in-repo provider manifests and
test literals gain the new fields; store meta records `transcript_path`.

## Comments

Implemented. `Manifest` gains `resume_prompt`/`fork`/`transcript`/`transcript_glob`,
`Event` gains `transcript`, `EventKind` gains `WaitStart`/`WaitEnd`, `ManagedFile`
gains `comment_suffix` (all additive, protocol stays v1). SDK passes
`transcript_path` through; store meta records the latest transcript path
(test `meta_keeps_the_latest_transcript_path`); setup renders/validates the
suffix (tests `managed_markdown_suffix_closes_the_ownership_header`,
`managed_multiline_comment_markers_are_rejected`). All provider manifests and
test literals updated. `cargo test --workspace` green.
