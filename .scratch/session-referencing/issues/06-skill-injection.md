# 06 Skill injection

Status: resolved

Shared skill body crates/providers/gw-skill.md (frontmatter name gw), added
as managed_files to claude/codex/amp manifests at their global skill paths,
comment_prefix "<!--" / comment_suffix " -->". Teaches ls/show/bounded
wait loop/resume.

## Comments

Implemented: shared body at `crates/providers/gw-skill.md` (frontmatter
`name: gw`), installed via `managed_files` with `<!--` / ` -->` header at:
claude `~/.claude/skills/gw/SKILL.md`, codex `~/.agents/skills/gw/SKILL.md`
(official USER scope per developers.openai.com/codex/skills), amp
`~/.config/amp/skills/gw/SKILL.md` (Amp-native dir per ampcode.com/manual).
Teaches ls/show/bounded wait-recheck loops/resume; attention = fetch a human.
