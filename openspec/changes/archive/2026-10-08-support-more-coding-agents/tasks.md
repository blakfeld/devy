# Tasks

## 1. Targets, selection and detection

- [x] 1.1 In `src/commands/agent_setup.rs`, add a `Target` (`Claude`, `Shared`) with relative paths `.claude/skills/devy/SKILL.md` and `.agents/skills/devy/SKILL.md`, an `AgentName` value enum that maps each name to a target, and a `Selection` (`Detect` as the default, `Explicit`, `All`) on `Options`. Verify with unit tests that every agent name maps as the spec says and that selection deduplicates targets and keeps them in order (Claude, then Shared).
- [x] 1.2 Implement `detect(root)` over the spec's marker list using `symlink_metadata`, so links are never followed. Verify with unit tests for each marker, for no markers, and for a dangling `.cursor` symlink, which counts as present.
- [x] 1.3 Add a unit test asserting that the skill's `description` is at most 1024 characters and that the frontmatter `name` equals the skill's parent directory name for both targets. Verify that `cargo test agent_setup` passes.

## 2. Multi-file writes and reporting

- [x] 2.1 Replace `Report { skill, agents_md }` with per-path lists of skills and blocks. Generalize `print` and `problems` so messages name the concrete path, and keep today's wording for `.claude/skills/devy/SKILL.md`. Update the existing unit tests to use a lookup helper, and verify they pass unchanged in intent.
- [x] 2.2 Write every selected skill target under the existing overwrite rules, with `--force` applying to all of them. Process the remaining targets after one is refused. Verify with a unit test that a refused shared skill plus a fresh Claude skill writes the Claude skill and reports one problem.
- [x] 2.3 Add `Outcome::SkippedSymlink`: before writing the shared target, check `.agents`, `.agents/skills` and `.agents/skills/devy` for symlinks, print `○ skipped .agents/skills/devy/SKILL.md: <dir> is a symlink`, and don't count the skip as a problem. Verify with a unix-only unit test (`.agents/skills -> .claude/skills`) that nothing is written through the link and the run succeeds. Also verify that the Claude target still errors on a symlinked `.claude`.
- [x] 2.4 Generalize the `AGENTS.md` logic to a block-file list that includes `GEMINI.md` (never created, even with `--agents-md`). Skip files whose resolved target was already handled in the run. Verify with unit tests for: `GEMINI.md` gaining a block, `GEMINI.md` absent and not created, `GEMINI.md -> AGENTS.md` ending with exactly one block, and CRLF line endings preserved in `GEMINI.md`.

## 3. CLI, init and completion

- [x] 3.1 In `src/cli.rs`, add the repeatable `--agent <name>` (value enum) and `--all` (which conflicts with `--agent`), extend `--print`'s conflicts to both, update the `AgentSetup` doc comment and help text, and wire the flags into `Options`. Verify with `cli.rs` parse tests: `--agent` repeats, `--agent` with `--all` exits 2, `--print` with `--agent` exits 2, and an unknown name is rejected with a message listing the accepted values.
- [x] 3.2 Confirm that `run_for_init` uses default (detecting) options, and that its warnings name each problem's path. Add an `init.rs` unit test: a `.gemini/` marker makes init write both skills, and a hand-written shared skill produces a warning while init still succeeds.
- [x] 3.3 In `src/commands/hook.rs`, add `--agent` (with values `claude codex gemini cursor copilot windsurf opencode amp`) and `--all` to the zsh, bash and fish snippets. Update `snippets_complete_json_exec_and_agent_setup` to assert the new flags and values.

## 4. CLI tests and docs

- [x] 4.1 Add `tests/cli.rs` tests that run the real binary for each new spec scenario:
  - no markers: Claude only, and no `.agents/`
  - a `.cursor/` marker: both skills, with the `✓ wrote .agents/skills/devy/SKILL.md` line
  - `--agent codex`: no `.claude/`
  - `--all`: both skills
  - `--agent emacs`: exit 2
  - the symlinked `.agents/skills` skip (unix only)
  - `GEMINI.md` updated

  Verify with `cargo test --test cli agent_setup`.
- [x] 4.2 Update the README's "Using devy with coding agents" section and the `init` paragraph:
  - the supported agents and which file each reads
  - the detection markers
  - `--agent` and `--all`
  - the `GEMINI.md` block
  - the duplicate-skill note for tools that read both directories
  - opting out with `--agent claude`

  Verify by running each documented command in a scratch project and checking it behaves as written.
- [x] 4.3 Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`, and verify that all three pass.
