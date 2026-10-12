# Proposal

## Why

Coding agents such as Claude Code work inside devy projects but can't see what devy knows: which services are running, which ports were assigned, which environment variables the project needs, and which project commands exist. So they guess at ports, scrape colored tables, and run `npm test` without the shadowenv environment, because an agent's shell usually never runs the shadowenv hook.

`add-mcp-server` proposed exposing this state through an MCP server. Every agent devy targets already has a shell, though, and already has permission rules for shell commands. This change puts the fix in the CLI instead: structured output, a way to run commands in the project environment, and shipped guidance that teaches agents to use both. People and scripts benefit too, and devy keeps one interface rather than two.

## What Changes

- **`--json` on `devy status`, `devy services` and `devy check`.** Each prints one JSON document to stdout instead of the human tables. The documents share a top-level `version` field.
  - `services --json` includes each service's backend (`package` or `docker`), running state, host, port and where the port came from. This replaces a separate `ports` command.
  - `status --json` also lists the project commands from `devy.yml`. This replaces making the hidden `_commands` completion helper agent-facing.
  - Environment values in JSON output are redacted when they look like secrets. The human `devy status` output is unchanged.
- **New `devy exec -- <program> [args…]`.** It runs a program with the project environment: `devy.yml` `environment`, module variables, `<SERVICE>_HOST`/`_PORT` from the lock, and module PATH entries. This is the same environment `devy up` writes to shadowenv, computed fresh and without needing a shadowenv-activated shell. The program runs directly, not through a shell. stdio is inherited and the child's exit code becomes devy's exit code.
- **New `devy agent-setup`.** It writes a Claude Code skill to `.claude/skills/devy/SKILL.md` that tells agents to:
  - learn the project with `devy status --json` and `devy services --json`
  - run tools through `devy exec`
  - start missing services with `devy start`
  - read failures with `devy logs` and `devy check --json`

  The skill covers Claude Code. For other agents, which read `AGENTS.md`, it also maintains a marked devy block in `AGENTS.md` when that file exists. `--agents-md` creates `AGENTS.md` if it is missing, and `--print` prints the skill without writing anything. `devy init` runs the same setup automatically after writing `devy.yml`, so new projects get the skill without an extra step. `devy agent-setup` remains the way to add or refresh it in existing projects.
- **Tab completion** covers `exec` and `agent-setup`, their flags, and `--json` after `status`, `services` and `check`.
- **README** gains a "Using devy with coding agents" section. It covers `agent-setup` and suggests Claude Code permission rules: allow the read-only commands, ask before `devy up`, `start`, `stop`, `restart` and `exec`. It warns that allow-listing `devy exec` allows arbitrary commands.
- **BREAKING:** `exec` and `agent-setup` become built-in names. A `devy.yml` project command with either name is shadowed and can no longer be run as `devy exec` / `devy agent-setup`.

## Non-goals

- The MCP server. It stays in `add-mcp-server` as a possible follow-up. The data helpers built here would serve it unchanged.
- JSON output for `up`, `logs`, `doctor` or `ask`.
- Writing permission rules into `.claude/settings.json` for the user.
- Project-specific skill content. The skill is static and tells the agent to query devy, so it never goes stale as `devy.yml` changes.

## Capabilities

### New Capabilities
- `json-output`: `--json` for `status`, `services` and `check`. Covers document shapes, the version field, redaction, stdout/stderr and exit-code rules.
- `environment-exec`: `devy exec`. Covers how the project environment is computed, running without a shell, stdio and exit-code pass-through, and errors.
- `agent-setup`: `devy agent-setup` and the agent setup `devy init` runs. Covers the skill file, the `AGENTS.md` block, overwrite rules and `--print`.

### Modified Capabilities
- `cli`: adds `exec` and `agent-setup` to the built-in subcommands, and exempts `devy exec` from the child-failure-maps-to-exit-1 rule.
- `shell-integration`: tab completion includes `exec`, `agent-setup`, their flags, and `--json`.

## Impact

- **Code:**
  - `src/cli.rs`: new `Exec` and `AgentSetup` variants, plus `--json` flags.
  - `src/commands/status.rs`, `service.rs`, `check.rs` and `shared.rs`: data-returning helpers with text and JSON renderers.
  - New `src/project_env.rs`: the shared environment resolver, which `up.rs` adopts.
  - New `src/commands/exec_env.rs` (`devy exec`). The existing `exec.rs` keeps project commands.
  - New `src/commands/agent_setup.rs` with an embedded skill template.
  - `src/commands/init.rs` (runs agent setup) and `src/commands/hook.rs` (completion).
- **Dependencies:** none new. Uses the existing `serde_json` and `ai::redact`.
- **Docs:** README sections for `--json`, `devy exec`, `devy agent-setup` and coding agents.
- **Interaction with `add-mcp-server`:** both modify `shell-integration`'s *Tab completion* requirement. Whichever archives second must rebase its MODIFIED block. If MCP is picked up later, its tasks 1.2–1.4 are already done by this change.
- **Docker:** `add-docker-service-manager` is archived, so the `backend` field is reported from the existing service runner state. No coordination is needed.
