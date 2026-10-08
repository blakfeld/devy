## ADDED Requirements

### Requirement: Agent targets
`devy agent-setup` SHALL support two skill targets:
- **Claude**: `.claude/skills/devy/SKILL.md`, read by Claude Code.
- **Shared**: `.agents/skills/devy/SKILL.md`, the cross-tool directory read by Codex, Gemini CLI, Cursor, GitHub Copilot, Windsurf/Devin, OpenCode and Amp.

`--agent <name>` SHALL select targets explicitly and SHALL be repeatable. The accepted names are `claude`, `codex`, `gemini`, `cursor`, `copilot`, `windsurf`, `opencode` and `amp`. `claude` selects the Claude target, and every other name selects the shared target. Naming several agents that map to the same target SHALL write that target once. Any other name SHALL be a usage error that exits 2 and lists the accepted names. `--all` SHALL select both targets. `--agent` and `--all` SHALL conflict, and combining them SHALL be a usage error that exits 2.

When neither `--agent` nor `--all` is given, devy SHALL select:
- the Claude target, always
- the shared target, when at least one of these exists at the project root: `.agents/`, `.codex/`, `.gemini/`, `GEMINI.md`, `.cursor/`, `.cursorrules`, `.github/copilot-instructions.md`, `.github/instructions/`, `.github/skills/`, `.windsurf/`, `.windsurfrules`, `.devin/`, `.opencode/`, `opencode.json`, `.amp/`, `AGENTS.md`

Detection SHALL only check whether these paths exist and SHALL NOT read their contents or follow them outside the project.

Target selection SHALL NOT affect the `AGENTS.md` and `GEMINI.md` blocks, which follow their own rules.

#### Scenario: Claude only by default
- **WHEN** a project has none of the detection markers and the user runs `devy agent-setup`
- **THEN** `.claude/skills/devy/SKILL.md` is written and `.agents/` is not created

#### Scenario: Detected agent gets the shared skill
- **WHEN** a project has a `.cursor/` directory and the user runs `devy agent-setup`
- **THEN** both `.claude/skills/devy/SKILL.md` and `.agents/skills/devy/SKILL.md` are written with identical content
- **AND** stdout contains `✓ wrote .agents/skills/devy/SKILL.md`

#### Scenario: AGENTS.md triggers the shared skill
- **WHEN** a project has `AGENTS.md` and the user runs `devy agent-setup`
- **THEN** `.agents/skills/devy/SKILL.md` is written and `AGENTS.md` gains the devy block

#### Scenario: Explicit agent only
- **WHEN** the user runs `devy agent-setup --agent codex` in a project without `.claude/`
- **THEN** `.agents/skills/devy/SKILL.md` is written and `.claude/` is not created

#### Scenario: Repeated agents sharing a target
- **WHEN** the user runs `devy agent-setup --agent cursor --agent gemini`
- **THEN** `.agents/skills/devy/SKILL.md` is written once and reported on one line

#### Scenario: All targets
- **WHEN** the user runs `devy agent-setup --all` in a project with no detection markers
- **THEN** both skill files are written

#### Scenario: Unknown agent name
- **WHEN** the user runs `devy agent-setup --agent emacs`
- **THEN** the process exits 2, stderr lists the accepted names, and nothing is written

### Requirement: Symlinked shared skill directory
When `.agents`, `.agents/skills` or `.agents/skills/devy` under the project root is a symlink, `devy agent-setup` SHALL NOT write the shared skill through it. It SHALL print `○ skipped .agents/skills/devy/SKILL.md: <dir> is a symlink`, where `<dir>` is the first symlinked directory, continue with the other files, and SHALL NOT treat the skip as a failure. This applies whether the shared target was detected or requested explicitly.

#### Scenario: Shared skills linked to Claude skills
- **WHEN** `.agents/skills` is a symlink to `.claude/skills` and the user runs `devy agent-setup --all`
- **THEN** `.claude/skills/devy/SKILL.md` is written, stdout reports that the shared skill was skipped because `.agents/skills` is a symlink, and the process exits 0

## MODIFIED Requirements

### Requirement: Agent skill file
`devy agent-setup` SHALL write the devy skill to each selected skill target (see *Agent targets*) under the project root, creating directories as needed. It SHALL print `✓ wrote <path>` for each file it creates, where `<path>` is the target's path relative to the project root. Every target SHALL receive byte-identical content. The skill SHALL begin with YAML frontmatter containing exactly `name: devy` and a `description`, of at most 1024 characters, saying it applies in projects with a `devy.yml`. The body SHALL:
- tell the agent to learn the project with `devy status --json` and `devy services --json`
- tell it to run project tools through `devy exec -- <program>`
- tell it to start a stopped service with `devy start <name>` and inspect failures with `devy logs <name>` and `devy check --json`
- tell it to run project commands as `devy <command>`
- tell it not to edit `devy.lock` or `.shadowenv.d` by hand

The skill content SHALL NOT depend on the project's `devy.yml` or on the target, apart from using the binary's own name, and SHALL contain a marker line identifying it as generated by devy.

#### Scenario: Fresh setup
- **WHEN** the user runs `devy agent-setup` in a devy project without `.claude/` and without detection markers
- **THEN** `.claude/skills/devy/SKILL.md` exists with `name: devy` frontmatter and mentions `devy status --json` and `devy exec`
- **AND** the process exits 0

#### Scenario: Outside a project
- **WHEN** the user runs `devy agent-setup` with no `devy.yml` in the current directory or its parents
- **THEN** stderr contains `error: devy.yml not found` and nothing is written

### Requirement: Skill overwrite rules
For each selected skill file, devy SHALL apply these rules independently:
- When the file already exists and contains devy's generated marker, devy SHALL overwrite it.
- If the content is unchanged, devy SHALL print `○ <path> is up to date` and leave the file untouched.
- When the file exists without the marker, devy SHALL leave it unchanged and report `<path> was not written by devy. Use --force to overwrite.`, unless `--force` is given. `--force` SHALL apply to every selected skill file.

`devy agent-setup` SHALL still process the remaining files and SHALL exit 1 if any skill file was refused.

#### Scenario: Regenerate after upgrade
- **WHEN** a devy-generated `SKILL.md` from an older devy exists
- **THEN** `devy agent-setup` replaces it with the current content

#### Scenario: Hand-written skill protected
- **WHEN** `.claude/skills/devy/SKILL.md` exists without devy's marker
- **THEN** `devy agent-setup` exits 1 without changing it, and `devy agent-setup --force` overwrites it

#### Scenario: One target refused, the other written
- **WHEN** `.agents/skills/devy/SKILL.md` exists without devy's marker and `.claude/skills/devy/SKILL.md` does not exist
- **THEN** `devy agent-setup --all` writes the Claude skill, leaves the shared skill unchanged, stderr names `.agents/skills/devy/SKILL.md`, and the process exits 1

### Requirement: AGENTS.md block
When `AGENTS.md` exists at the project root, `devy agent-setup` SHALL make sure it contains exactly one block between `<!-- devy:begin -->` and `<!-- devy:end -->` with a short summary of the same guidance. When `GEMINI.md` exists at the project root, devy SHALL maintain the same block in it, under the same rules. For each of these files, devy:
- SHALL replace an existing block in place, otherwise append the block at the end of the file, and SHALL leave all content outside the markers unchanged
- SHALL count a marker only on a line of its own outside fenced code blocks
- SHALL write the block with the file's line endings
- SHALL update a symlink's target and leave the link in place when the file, or `SKILL.md`, is a symlink, provided the target is inside the project and outside `.git`. For `AGENTS.md` and `GEMINI.md` the target SHALL also have a file name ending in `.md` (case-insensitive) and SHALL NOT be a devy skill file. For a skill file the target's file name SHALL be exactly `SKILL.md`. Otherwise, even with `--force`, devy SHALL leave both the link and its target unchanged, report a problem naming the file, and continue with the other files
- SHALL report each file written with `✓ updated <path>` or `✓ wrote <path>`

When `AGENTS.md` does not exist, devy SHALL NOT create it unless `--agents-md` is given. `--agents-md` creates the file containing only the block. devy SHALL NOT create `GEMINI.md`, and SHALL treat a `GEMINI.md` symlink whose target does not exist as absent. When `AGENTS.md` and `GEMINI.md` resolve to the same file, devy SHALL update that file once and print `○ GEMINI.md is the same file as AGENTS.md`. Likewise, when both selected skill files resolve to the same file, whichever of them is the symlink, devy SHALL process that file once under the skill path that is not a symlink and print `○ <link> is the same file as <other>` for the other, which is not a problem. devy SHALL NOT create a file through a dangling skill symlink unless its target is the other selected skill file. These blocks SHALL be maintained regardless of `--agent` and `--all`.

#### Scenario: Existing AGENTS.md gains block
- **WHEN** `AGENTS.md` contains other guidance and no devy block
- **THEN** after `devy agent-setup` the original content is unchanged and the devy block is appended

#### Scenario: Block replaced in place
- **WHEN** `AGENTS.md` already has a devy block followed by other content
- **THEN** `devy agent-setup` replaces only the text between the markers

#### Scenario: Symlinked AGENTS.md
- **WHEN** `AGENTS.md` is a symlink to `CLAUDE.md`
- **THEN** after `devy agent-setup`, `AGENTS.md` is still a symlink and `CLAUDE.md` contains the devy block

#### Scenario: AGENTS.md not created by default
- **WHEN** no `AGENTS.md` exists and the user runs `devy agent-setup`
- **THEN** no `AGENTS.md` is created

#### Scenario: AGENTS.md created on request
- **WHEN** no `AGENTS.md` exists and the user runs `devy agent-setup --agents-md`
- **THEN** `AGENTS.md` is created containing the devy block

#### Scenario: Existing GEMINI.md gains block
- **WHEN** `GEMINI.md` contains other guidance and no devy block
- **THEN** after `devy agent-setup` the original content is unchanged, the devy block is appended, and stdout contains `✓ updated GEMINI.md`

#### Scenario: GEMINI.md never created
- **WHEN** no `GEMINI.md` exists and the user runs `devy agent-setup --agents-md --all`
- **THEN** no `GEMINI.md` is created

#### Scenario: GEMINI.md linked to AGENTS.md
- **WHEN** `GEMINI.md` is a symlink to `AGENTS.md`
- **THEN** `devy agent-setup` leaves exactly one devy block in `AGENTS.md`

#### Scenario: Block file linked to a non-Markdown file
- **WHEN** `GEMINI.md` is a symlink to `devy.yml`
- **THEN** `devy agent-setup` leaves `devy.yml` unchanged, still writes the selected skill files, stderr names `GEMINI.md`, and the process exits 1

#### Scenario: Skill file linked to a non-skill file
- **WHEN** `.claude/skills/devy/SKILL.md` is a symlink to `devy.yml`
- **THEN** `devy agent-setup --force` leaves `devy.yml` unchanged, stderr names `.claude/skills/devy/SKILL.md`, and the process exits 1

#### Scenario: Claude skill linked to the shared skill
- **WHEN** `.claude/skills/devy/SKILL.md` is a symlink to `../../../.agents/skills/devy/SKILL.md`, which does not exist yet, and the user runs `devy agent-setup --all`
- **THEN** `.agents/skills/devy/SKILL.md` is written once, stdout contains `○ .claude/skills/devy/SKILL.md is the same file as .agents/skills/devy/SKILL.md`, and the process exits 0

### Requirement: Print without writing
`devy agent-setup --print` SHALL print the skill content to stdout and exit 0 without writing any file. It SHALL work outside a devy project. `--print` SHALL conflict with `--force`, `--agents-md`, `--agent` and `--all`, and combining them SHALL be a usage error that exits 2.

#### Scenario: Print only
- **WHEN** the user runs `devy agent-setup --print`
- **THEN** stdout contains the skill frontmatter and no file is created

#### Scenario: Print with agent selection
- **WHEN** the user runs `devy agent-setup --print --agent codex`
- **THEN** the process exits 2 and nothing is written

### Requirement: Init installs agent guidance
After `devy init` writes `devy.yml` in any mode, it SHALL perform the same writes as `devy agent-setup` with no flags, in the directory where it wrote `devy.yml`:
- it writes or refreshes the Claude skill
- it writes or refreshes the shared skill when a detection marker exists (see *Agent targets*)
- it updates the devy block in `AGENTS.md` and `GEMINI.md` only when those files already exist
- it never creates `AGENTS.md` or `GEMINI.md`

It SHALL report each write and skip with the same lines as `devy agent-setup`. When `devy init` writes nothing, because it failed, `devy.yml` already exists without `--force`, or `--show-context` was given, it SHALL NOT write agent files.

If agent setup cannot complete, `devy init` SHALL still succeed and exit 0, because `devy.yml` has already been written. In that case it SHALL print a warning to stderr for each problem, naming the problem and suggesting `devy agent-setup`. Problems include:
- a skill file not written by devy, which `init` SHALL leave unchanged
- unbalanced markers in `AGENTS.md` or `GEMINI.md`
- a write error

#### Scenario: Init installs the skill
- **WHEN** `devy init --detect` writes `devy.yml` in a directory without `.claude/` and without detection markers
- **THEN** `.claude/skills/devy/SKILL.md` is also written and the output reports it
- **AND** no `AGENTS.md` is created and no `.agents/` directory is created

#### Scenario: Init detects another agent
- **WHEN** a `.gemini/` directory exists and `devy init --detect` writes `devy.yml`
- **THEN** both `.claude/skills/devy/SKILL.md` and `.agents/skills/devy/SKILL.md` are written

#### Scenario: Init updates existing AGENTS.md
- **WHEN** `AGENTS.md` exists and `devy init --detect` writes `devy.yml`
- **THEN** `AGENTS.md` contains the devy block and its other content is unchanged

#### Scenario: Hand-written skill left alone
- **WHEN** `.claude/skills/devy/SKILL.md` exists without devy's marker and `devy init --detect --force` runs
- **THEN** `devy.yml` is written, the skill file is unchanged, stderr warns and mentions `devy agent-setup --force`, and the process exits 0

#### Scenario: Nothing written, no agent files
- **WHEN** `devy.yml` already exists and the user runs `devy init` without `--force`
- **THEN** init fails as before and no `.claude/` or `.agents/` directory is created

#### Scenario: Show context writes nothing
- **WHEN** the user runs `devy init --show-context`
- **THEN** no agent files are written
