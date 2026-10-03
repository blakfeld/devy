# Design

## Context

See proposal.md for motivation. The current code has these properties that shape the design:

- **`check` already separates data from rendering.** `collect_findings` returns a findings struct with `issues()` and `warnings()` as `Vec<String>`. Only `print()` and `check_with_runtime` write output.
- **`status` and `services` print as they go.** `status_impl` calls `shared::print_dep_table`, `print_env_table` and `print_path_table`, which `println!` directly. `service::list_impl` prints its own `●`/`○` lines.
- **Dependency rows are only partly data.** `shared::dep_rows` already returns `DepRow { dep, label, installed, running }`, but nothing records the backend or port. The backend is visible only through `service_runner::display_name`'s `(docker)` suffix.
- **Port resolution is shared and data-returning.** `ports::resolve_ports(…, PortMode::ReadOnly)` returns a `ResolvedPort` per dependency and never writes the lock.
- **The project environment is computed inline in `up_tracked`.** That code starts from `pm.path_prepends`, adds each module's `env_vars` and `path_prepends`, adds `<SERVICE>_HOST`/`_PORT`, then calls `merge_env` with `config.environment`. Nothing else can reuse it. `status` recomputes only the module path prepends.
- **Built-in names shadow project commands.** `allow_external_subcommands` dispatches only names clap doesn't know. `builtin_subcommands()` feeds `init`'s detection and AI prompt, so new built-ins are excluded there automatically.
- **Redaction exists.** `ai::redact::value(key, value)` implements the rules the json-output spec reuses.

## Goals / Non-Goals

**Goals:**
- JSON and text output built from the same data, so they can't disagree.
- One environment resolver used by both `up` and `exec`, so `devy exec` sees exactly what shadowenv would.
- Guidance files that stay correct as `devy.yml` changes, with no state to keep in sync.

**Non-Goals:**
- A general `--format` system or JSON for every command.
- Capturing, timing out or capping `devy exec` output. The caller (a shell or an agent harness) owns that. This is why `add-mcp-server`'s captured-spawn task 1.5 isn't needed here.

## Decisions

### D1. Data helpers, then two renderers per command
Each of `status`, `services` and `check` gets a function that returns a report struct, plus a text renderer and a JSON renderer.

- **`services`:** `services_report(config, runners, lock) -> Vec<ServiceInfo>` with `name`, `backend`, `running`, `port`, `port_source`. It resolves ports with `PortMode::ReadOnly`. `list_impl` renders the existing `●`/`○` lines from it. The text output is unchanged except that it now has the port data available. Port display in text is out of scope.
- **`status`:** `status_report(...) -> StatusReport` composes `dep_rows` (extended with `backend` and port info for services), the env and PATH read-back, and the sorted commands. The existing `print_*_table` functions become renderers over these rows.
- **`check`:** reuses `collect_findings`. The JSON renderer serializes `issues()`/`warnings()` and returns `SilentExit(1)` when there are issues, so it never prints the stderr summary.
- **Types:** report structs derive `serde::Serialize`, with field names matching the spec. The JSON is printed with `serde_json::to_string_pretty` plus a newline. A `JSON_VERSION: u32 = 1` constant is injected at the top level.
- **Backend:** `DepRow` gains `backend: Backend { Package, Docker }`, derived from the runner rather than parsed from the label.
- **Color:** in JSON mode devy calls `colored::control::set_override(false)` before any work, so warnings on stderr are plain too. The JSON itself never goes through `colored`.

**Alternative considered:** a global `--json` flag on `Cli`. That would make `--json` legal on `up`, `logs` and the rest and then need rejecting there. Per-subcommand flags keep `--help` accurate.

### D2. A shared project environment resolver
A new `src/project_env.rs` has `resolve(config, deps, pm, project_root) -> ProjectEnv { vars: HashMap<String,String>, path_prepends: Vec<String> }`. It assumes `deps` has already been port-resolved, and contains exactly the logic now inline in `up_tracked`: package manager prepends first, module `env_vars` and `path_prepends` per dependency, `<SERVICE>_HOST`/`_PORT`, then `merge_env`.

- **`up`** keeps its install loop but takes the env from the resolver after installs. For a failed install, `up` already aborts before writing the environment, so building env from the full dependency list afterwards matches today's behavior.
- **`exec`** loads config and lock, applies `apply_lock_from_source`, resolves ports read-only, and calls the same resolver.
- **Equivalence test:** for one config, the resolver's output equals what `MockEnvManager.last_vars` receives from `up`.
- **Assumption to verify in task 2.1:** `Module::env_vars` and `path_prepends` are pure functions of the dependency and project root, and don't need the package to be installed. If some module probes installed state, exec documents the gap for that module instead of installing anything.

**Alternative considered:** reading the shadowenv file back (`read_vars`/`read_path_prepends`). It's simpler and matches the shell exactly, but it fails before the first `devy up`. It also fails for projects where `up` writes no file because there is nothing to write, and it is stale after `devy.yml` edits. Fresh computation is what the spec requires.

### D3. `devy exec` runs the program directly
- **Parsing:** `Exec { #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)] argv: Vec<String> }`. A leading `--` is accepted and stripped by clap.
- **Spawning:** `Command::new(&argv[0]).args(&argv[1..])` with `envs(project_env.vars)` and `PATH` set to the prepends joined ahead of the inherited `PATH`, using `std::env::join_paths`. There is no shell, so the `sh_quote` and shell-allowlist machinery doesn't apply.
- **Program lookup:** on Unix, Rust's `Command` resolves a bare program name against the child's `PATH` when one is set through `env`. On Windows, recent Rust versions also search the child's `PATH`. A Windows test in task 3.2 covers this. If it fails, devy resolves the program with `which::which_in` (already a dependency) against the computed PATH before spawning.
- **Exit status:** stdio is inherited. The exit code passes through via a `SilentExit(code)` error, the same mechanism `check` uses to skip the `error:` line. A signal termination (`code()` is `None`) becomes `SilentExit(1)`. A spawn failure is an ordinary error, so it prints `error: failed to run '<program>': …`.
- **Signals:** devy doesn't install handlers. Ctrl-C reaches the child through the shared process group, as with project commands today.

**Alternative considered:** `devy exec "<shell string>"` through the project's default shell. That's convenient for pipes, but it reintroduces quoting and injection concerns. Agents that need a pipeline can run `devy exec sh -c '…'` explicitly.

### D4. `devy agent-setup` writes static, embedded content
- **Templates:** the skill and the AGENTS.md block are `include_str!` templates under `src/commands/agent_setup/`, with `{bin}` substituted like the hook snippets do.
- **Generated marker:** the skill carries a `<!-- generated by devy agent-setup; rerun to update -->` line, which drives the overwrite rules.
- **No project content:** the skill doesn't embed project details. It tells the agent to run `devy status --json`. So it never goes stale relative to `devy.yml`, only relative to devy's own version, and rerunning `agent-setup` after an upgrade fixes that.
- **AGENTS.md:** the block is updated by finding the first `<!-- devy:begin -->` … `<!-- devy:end -->` span. If the markers are unbalanced (a begin without an end), devy fails with an error instead of guessing.
- **Writes:** files are written atomically (temp file plus rename in the same directory), so an interrupted run can't truncate a user's `AGENTS.md`.
- **Location:** `agent-setup` resolves the project root from `devy.yml`, so running it from a subdirectory writes at the root. `init` uses the directory where it wrote `devy.yml`, which is the current directory.

**Both `devy init` and `devy agent-setup`:** new projects get the skill with no extra step, because `init` calls the same `agent_setup::apply(root, Options::default())` after writing `devy.yml`. `agent-setup` stays because `init` refuses to run when `devy.yml` exists, so existing projects and devy upgrades need another way in.

Inside `init`, a failed agent setup becomes a warning rather than an error. `devy.yml` is already on disk, and failing would make a successful init look broken. Each outcome is a variant of an `Outcome` enum:
- `Wrote`
- `UpToDate`
- `NotOurs`: the skill exists without devy's marker
- `UnbalancedMarkers`

`agent-setup` maps `NotOurs` to an error, while `init` maps it to a warning, so the two callers share all the write logic. `init` never creates `AGENTS.md`, since that's a top-level file in the user's repo and only the explicit `--agents-md` flag creates it.

**Alternative considered:** writing `CLAUDE.md` instead of a skill. `CLAUDE.md` is loaded into every session, while a skill loads only when relevant. The skill is what Claude Code uses. The optional `AGENTS.md` block is for other agents (Codex, Cursor, Copilot and similar), which read `AGENTS.md` but don't consistently support skills. Claude Code reads `CLAUDE.md` rather than `AGENTS.md`, so the block adds little for Claude. That's why devy only writes it when the project already has an `AGENTS.md` or the user passes `--agents-md`.

### D5. Permission guidance lives in the README only
The README suggests a `.claude/settings.json` snippet:
- **allow:** `Bash(devy status:*)`, `Bash(devy services:*)`, `Bash(devy check:*)`, `Bash(devy logs:*)`
- **ask:** `Bash(devy up:*)`, `Bash(devy start:*)`, `Bash(devy stop:*)`, `Bash(devy restart:*)`, `Bash(devy exec:*)`

devy doesn't write settings. Permission choices belong to the person, and settings files are often shared through git.

## Risks / Trade-offs

- **[Risk]** Allow-listing `Bash(devy exec:*)` is equivalent to allow-listing every command. → **Mitigation:** the README says so explicitly and puts `exec` under "ask". The skill tells agents to prefer `devy <command>` for defined project commands.
- **[Risk]** The JSON shape becomes a contract that limits refactoring. → **Mitigation:** additive-only within `version: 1`, with an integration test that pins the field set for each document.
- **[Risk]** Name-based redaction misses secrets, for example `API_URL=https://x?apikey=…`. → **Mitigation:** documented as best-effort. Note that `devy exec env` can reveal everything anyway, so redaction keeps secrets out of casual transcripts but isn't a security boundary.
- **[Trade-off]** `exec` recomputes the environment on every call, including package manager detection. This costs a few milliseconds, which is negligible next to the commands it wraps.
- **[Risk]** `exec` and `agent-setup` shadow existing project commands of the same name (**BREAKING**). → **Mitigation:** a release note. A follow-up could have `devy check` warn when a project command is shadowed by a built-in, which would cover `status`, `logs` and others too.
- **[Trade-off]** The `exec` env can differ from an activated shell's env when `devy.yml` changed since the last `up`. `exec` follows `devy.yml`, which is what an agent editing the config wants. `devy check` still reports the shell's drift.

## Migration Plan

This is additive except for the two newly reserved names. To roll back, remove the subcommands and flags. Text output of existing commands is unchanged.

## Open Questions

- Whether the text `devy services` should also show ports, now that the data is available. This is cosmetic and can follow later without changing the JSON.
