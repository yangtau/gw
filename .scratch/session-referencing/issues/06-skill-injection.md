# 06 Skill distribution

Status: resolved

Publish `skills/gw/SKILL.md` for explicit installation through `npx skills`.
It teaches ls/show/bounded wait loop/resume without becoming a provider setup
target or setup-health requirement.

## Comments

Implemented: shared body at `crates/providers/gw-skill.md` (frontmatter
`name: gw`), installed via `managed_files` with `<!--` / ` -->` header at:
claude `~/.claude/skills/gw/SKILL.md`, codex `~/.agents/skills/gw/SKILL.md`
(official USER scope per developers.openai.com/codex/skills), amp
`~/.config/amp/skills/gw/SKILL.md` (Amp-native dir per ampcode.com/manual).
Teaches ls/show/bounded wait-recheck loops/resume; attention = fetch a human.

2026-07-28: default injection was removed from all three manifests. Until an
opt-in path is designed, `gw setup` installs only runtime hooks and Amp's
observer plugin.

Resolved: moved the skill to the standard `skills/gw/SKILL.md` discovery path.
Install globally with `npx skills add yangtau/gw --skill gw -g`; use
`-a amp -a claude-code -a codex` to target all official providers.
