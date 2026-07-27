# 02 Address model + gw ls

Status: resolved

`gw-core/src/address.rs`: parse `provider:id` | bare id/prefix (>=4 chars,
unique across known sessions); resolve against store records. Errors name
ambiguity candidates.

`gw ls [--json]`: reuse discover::snapshot (needs tmux topology; degrade to
sessions-only outside tmux). Human table + JSON object {agents, sessions}.

## Comments

Implemented in `gw-core/src/address.rs` (canonical `provider:session-id`, bare
id, unique prefix ≥ 4 chars, exact beats prefix, ambiguity errors naming all
candidates) with unit tests, plus `gw ls [--json]` in `gw/src/sessions.rs`
printing `{"agents":[…],"sessions":[…]}` from the discovery snapshot. Smoke
verified end-to-end via `gw hook claude` fixtures.
