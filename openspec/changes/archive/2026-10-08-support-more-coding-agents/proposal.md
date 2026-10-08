# Proposal

## Why

`devy agent-setup` (and `devy init`) only installs a Claude Code skill at `.claude/skills/devy/SKILL.md`. Other agents get the guidance only through the optional `AGENTS.md` block, which is a short summary and is never created by default. Gemini CLI doesn't read `AGENTS.md` at all by default. Codex, Gemini CLI, Cursor, GitHub Copilot, Windsurf/Devin, OpenCode and Amp all support Agent Skills now (the same `SKILL.md` format), and all of them read a shared project directory, `.agents/skills/`. One more file reaches all of them.

## What Changes

- **Shared skill.** `devy agent-setup` can also write the same skill content to `.agents/skills/devy/SKILL.md`, the cross-tool directory read by Codex, Gemini CLI, Cursor, Copilot, Windsurf/Devin, OpenCode and Amp. The Claude skill at `.claude/skills/devy/SKILL.md` is unchanged.
- **Auto-detection.** With no agent flags, devy always writes the Claude skill, as today. It also writes the shared skill when the project shows signs of another agent: `.agents/`, `.codex/`, `.gemini/`, `GEMINI.md`, `.cursor/`, `.cursorrules`, `.github/copilot-instructions.md`, `.github/instructions/`, `.github/skills/`, `.windsurf/`, `.windsurfrules`, `.devin/`, `.opencode/`, `opencode.json`, `.amp/` or `AGENTS.md`.
- **New `--agent <name>` flag (repeatable)** chooses targets explicitly and turns detection off. Names: `claude`, `codex`, `gemini`, `cursor`, `copilot`, `windsurf`, `opencode`, `amp`. Every name except `claude` selects the shared skill. **New `--all`** writes every target. `--agent` and `--all` conflict with each other and with `--print`.
- **Overwrite rules per file.** Each skill file gets today's rules: overwrite a devy-generated file, report "up to date" when unchanged, refuse a hand-written file unless `--force` is given. Messages name the file concerned.
- **Symlinked shared directory.** Some repositories link `.agents/skills` to `.claude/skills`. When a directory on the shared skill's path is a symlink, devy doesn't write through it. It reports the skip and continues without an error.
- **`GEMINI.md` block.** When `GEMINI.md` exists, devy maintains the same marked devy block in it that it maintains in `AGENTS.md`. devy never creates `GEMINI.md`.
- **`devy init`** runs the same default setup: detection included, no flags.
- **Tab completion** gains `--agent`, its values, and `--all` after `agent-setup`.
- **README** lists the supported agents and the files devy writes for each one.

## Non-goals

- Per-tool skill directories (`.cursor/skills`, `.gemini/skills`, `.github/skills`, and so on), and legacy rule formats (`.cursor/rules/*.mdc`, `.github/instructions/*.instructions.md`). The shared directory reaches every listed tool.
- Persisting the chosen agents in `devy.yml`.
- Agent-specific skill content or extra frontmatter, such as Codex's `agents/openai.yaml`.
- Creating `GEMINI.md`, or editing agents' settings files. For example, devy doesn't add `AGENTS.md` to Gemini's `context.fileName`.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `agent-setup`: adds the shared `.agents/skills` target, agent detection, `--agent`/`--all`, per-file overwrite reporting, the symlinked-directory skip, the `GEMINI.md` block, and has `devy init` apply detection.
- `shell-integration`: tab completion for `agent-setup` includes `--agent`, its values and `--all`.

## Impact

- **Code:** `src/commands/agent_setup.rs` gains the target list, detection, a multi-file report, and generalized block maintenance for `AGENTS.md` and `GEMINI.md`. `src/cli.rs` adds the `--agent` value enum and `--all`. `src/commands/init.rs` adopts the default options. `src/commands/hook.rs` adds completion.
- **Tests:** unit tests in `agent_setup.rs`, and CLI tests in `tests/cli.rs` covering detection, flags, the symlink skip and `GEMINI.md`.
- **Dependencies:** none new.
- **Compatibility:** projects with no other-agent markers behave exactly as before. Projects with an `AGENTS.md` now also get `.agents/skills/devy/SKILL.md` from `devy agent-setup` and `devy init`, a new file that teams may want to commit. Tools that read both `.claude/skills` and `.agents/skills` (Cursor, Copilot, OpenCode, Amp) see two identical `devy` skills. See design.md.
- **Docs:** README section "Using devy with coding agents", and the `init` paragraph that describes which agent files are written.
