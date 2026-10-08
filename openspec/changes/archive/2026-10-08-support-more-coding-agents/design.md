# Design

## Context

`src/commands/agent_setup.rs` hard-codes a single skill target (`SKILL_PATH`) and a single block file (`AGENTS_MD`). `Report` has one field for each, `skill` and `agents_md`, and `Options` has two booleans, `force` and `agents_md`. `devy init` calls `run_for_init(root)` with `Options::default()`. Writes go through `fs_safe::ensure_dir_in`, which refuses any symlinked directory below the root, and through `resolve_in_root`/`write_atomic`, which follow a file symlink only when its target stays inside the root and outside `.git`. Every agent the proposal targets reads `.agents/skills/<name>/SKILL.md` with the frontmatter devy already emits: only `name` and `description`, and `name` matches the directory. Research sources are listed in *References*.

## Goals / Non-Goals

**Goals:**
- Turn the hard-coded skill and block files into small static tables, so adding a target later is a one-line change.
- Keep today's output byte-for-byte for projects with no detection markers.
- Keep every existing filesystem-safety guarantee for the new paths.

**Non-Goals:**
- A plugin or config mechanism for user-defined targets.
- Different skill content per agent.

## Decisions

### Two targets, keyed by agent name
`--agent` is a `clap::ValueEnum` with `Claude, Codex, Gemini, Cursor, Copilot, Windsurf, Opencode, Amp`. Each value maps to a `Target`, either `Claude` or `Shared`. The selected targets are a deduplicated, ordered set: Claude first, then Shared. That keeps output order stable.

*Alternatives considered:*
- **`--agent claude|shared`.** Simpler, but users think in tool names, and the mapping would end up documented in the README instead of the CLI.
- **Per-tool directories** (`.cursor/skills`, …). This duplicates files that the shared directory already covers. Some tools also read both their own directory and `.agents/skills`, which would show the skill twice or three times.

### Detection is an existence check on a fixed list
`detect(root) -> bool` returns true when any marker in the spec's list exists at the root. It uses `symlink_metadata`, so a dangling or out-of-root link still counts as "present", and it never follows the link or reads its contents. `AGENTS.md` is included because the file is the de facto signal that non-Claude agents are used, and devy already treats it specially.

The Claude target is always selected by default. This keeps behavior unchanged for current users, and Claude remains the only agent that doesn't read `.agents/skills`.

*Alternative considered:* also gating Claude on `.claude/` or `CLAUDE.md`. A new project created by `devy init` would then get no skill at all, which is a regression.

### Generalized report
`Report` becomes `skills: Vec<(&'static str, Outcome)>` and `blocks: Vec<(&'static str, Outcome)>`. `print` and `problems` iterate over both. Problem messages are formatted with the concrete path, so `init`'s warning names the file at fault. A new `Outcome::SkippedSymlink { dir: String }` prints the `○ skipped …` line and is not a problem. Unit tests that assert `report.skill` move to a lookup helper.

### Symlinked shared directory is skipped, not an error
Before writing the shared target, devy walks `.agents`, `.agents/skills` and `.agents/skills/devy` with `symlink_metadata`. On the first symlink it returns `SkippedSymlink` and never calls `ensure_dir_in`. The common case is `.agents/skills -> .claude/skills`. In that case the Claude target already writes the file the link points to, so failing would be noise, and writing through the link would bypass the reason `ensure_dir_in` refuses links: a committed link can point anywhere.

The Claude target keeps its current behavior: an `ensure_dir_in` error fails the command. That is the hardened behavior from `harden-untrusted-repo-security`, and this change has no reason to relax it.

### Block files as a list
`BLOCK_FILES = [("AGENTS.md", CreateWith::AgentsMdFlag), ("GEMINI.md", CreateWith::Never)]`. `apply_agents_md` becomes `apply_block(root, name, create)`. Before writing, devy resolves each existing file through `resolve_in_root` and skips any path whose resolved target was already handled in this run, reporting it `UpToDate`. Without that, a `GEMINI.md -> AGENTS.md` link would be processed twice. The block content is unchanged; the same summary works for Gemini.

### CLI surface
`AgentSetup` gains `#[arg(long = "agent", value_enum)] agents: Vec<AgentName>` and `#[arg(long, conflicts_with = "agents")] all: bool`. The `print` conflicts list grows to `["force", "agents_md", "agents", "all"]`. `Options` gains a `selection: Selection` field, with variants `Detect`, `Explicit(targets)` and `All`. `Default` is `Detect`, so `run_for_init` needs no signature change.

## Risks / Trade-offs

- **[Risk]** Cursor, Copilot, OpenCode and Amp read both `.claude/skills` and `.agents/skills`, so they may list `devy` twice. → The content is identical and the name is the same; tools either deduplicate by name or show two equivalent entries. Users who mind can run `--agent claude` or `--agent codex`, or link one directory to the other, which devy now tolerates. The README documents this.
- **[Risk]** Existing projects with `AGENTS.md` get a new `.agents/` directory the next time they run `devy init`, or `devy agent-setup` with no flags. → It's reported on stdout like every other write. The README notes it, and `--agent claude` opts out.
- **[Risk]** The detection list goes stale as tools change their conventions. → The list is a single constant with a unit test. Missing a marker only means the user has to pass `--agent`.
- **[Trade-off]** `GEMINI.md` is never created, even with `--agents-md`. Gemini users without a `GEMINI.md` rely on the shared skill. That avoids devy creating files named after a vendor that the user didn't ask for.

## Migration Plan

No migration. Rollback is a plain revert: files devy wrote under `.agents/skills/devy/` and its `GEMINI.md` block stay behind and carry devy's marker, so a later devy would manage them again.

## References

- Codex skills: https://learn.chatgpt.com/docs/build-skills
- Gemini CLI skills: https://geminicli.com/docs/cli/skills/
- Cursor skills: https://cursor.com/docs/skills
- VS Code / Copilot agent skills: https://code.visualstudio.com/docs/agent-customization/agent-skills
- Windsurf/Devin skills: https://docs.devin.ai/desktop/cascade/skills
- OpenCode skills: https://opencode.ai/docs/skills/
- Amp skills: https://ampcode.com/docs/customize/skills
