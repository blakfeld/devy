# Tasks

## 1. JSON output

- [ ] 1.1 Extend `shared::DepRow` with `backend` (`Package`/`Docker`, derived from the runner) and, for services, `port` plus `port_source` from `ResolvedPort`. Keep `label` and the text tables unchanged. Verify that existing `shared`, `status` and `check` tests pass, plus a new test that a docker-managed service row reports `Docker` and a locked port reports source `lock`.
- [ ] 1.2 Add `services_report` returning `Vec<ServiceInfo>` (resolving ports with `PortMode::ReadOnly`) and render `devy services` text from it. Add `--json` to `Services` in `src/cli.rs` and a serializable services document with `version: 1`. Verify with unit tests using `MockPackageManager` and `FakeRunner` for the spec scenarios: locked port, docker service, unassigned port, no services. Also verify that `devy.lock` is not written.
- [ ] 1.3 Add `status_report` returning `StatusReport` (dependencies, environment read-back, `environment_written`, PATH entries, sorted commands) and turn `print_dep_table`, `print_env_table` and `print_path_table` into renderers over it. Add `--json` to `Status`. Verify with unit tests for "status before up" (`environment_written: false`, `null` values), the commands list ordering and fields, and that no lock or `.shadowenv.d` is created. Existing text `status` tests must pass unchanged.
- [ ] 1.4 Add `--json` to `Check`. Serialize `collect_findings`' `issues()`/`warnings()` into `{version, passed, issues, warnings}`, return `SilentExit(1)` when issues exist without printing the stderr summary, and keep hard errors as `error: …`. Verify with unit tests for pass, two issues, a warning-only result, and a port conflict still being a hard error.
- [ ] 1.5 Apply `ai::redact::value` to `environment` values in the status document only. In JSON mode, disable colors before any output. Verify with unit tests for `STRIPE_SECRET_KEY`, `DATABASE_URL` with a password, and `LOG_LEVEL`, and that text `status` still shows the raw value.
- [ ] 1.6 Document `--json` for `status`, `services` and `check` in the README command sections, including the document fields, the `version` rule and that redaction is best-effort. Verify that the documented fields match the serialized structs (field names copied from a test's output).

## 2. Shared project environment resolver

- [ ] 2.1 Create `src/project_env.rs` with `resolve(config, deps, pm, project_root) -> ProjectEnv`, moving the env and PATH computation out of `up_tracked` (package manager prepends, module `env_vars`/`path_prepends`, `<SERVICE>_HOST`/`_PORT`, `merge_env`). Make `up` use it. First confirm that every `Module::env_vars`/`path_prepends` implementation is a pure function of the dependency and project root, and record any exception in design.md. Verify that the existing `up`, `merge_env` and shadowenv tests pass, plus a new test asserting the resolver output equals what `MockEnvManager.last_vars` receives from `up` for the same config.

## 3. `devy exec`

- [ ] 3.1 Add `Exec { argv }` to `src/cli.rs` with `trailing_var_arg`, `allow_hyphen_values` and `required = true`. Verify with `cli.rs` parse tests: `exec cargo test --help` keeps `--help` in argv, a leading `--` is stripped, and bare `exec` is a usage error.
- [ ] 3.2 Implement `src/commands/exec_env.rs`:
  - load config and lock, apply lock pins, resolve ports read-only, and call `project_env::resolve`
  - set the vars, and set PATH to the prepends joined ahead of the inherited PATH
  - spawn `argv[0]` directly with inherited stdio
  - map the exit code to `SilentExit(code)`, signal termination to `SilentExit(1)`, and a spawn failure to an `error:` naming the program

  Verify with unit tests for the env and PATH composition and that no files are written. Add `tests/cli.rs` cases for `devy exec env` showing `LOG_LEVEL` and `REDIS_PORT` from a lock, `sh -c 'exit 3'` exiting 3 with no `error:`, a missing program exiting 1, and a literal `$HOME; …` argument. Include a Windows-gated test that a program found only on the computed PATH runs; if it fails, resolve with `which::which_in`.
- [ ] 3.3 Document `devy exec` in the README. Cover no shell, exit-code pass-through, `devy.yml` vs. the activated shell, and using `sh -c` for pipelines. Verify that the examples run as written against a temp project.

## 4. `devy agent-setup`

- [ ] 4.1 Write the skill template and the AGENTS.md block template under `src/commands/agent_setup/`, with `{bin}` substitution and the generated marker. The skill must have `name`/`description` frontmatter and cover each point in the agent-setup spec. Verify with a unit test that the rendered skill parses as frontmatter plus body and mentions `status --json`, `services --json`, `exec`, `start`, `logs` and `check --json`.
- [ ] 4.2 Implement `devy agent-setup` (`AgentSetup { force, agents_md, print }`, with `print` conflicting with the other two):
  - resolve the project root
  - write `.claude/skills/devy/SKILL.md` atomically
  - apply the overwrite rules: marker means overwrite, identical content means "up to date", no marker without `--force` means fail
  - update or append the AGENTS.md block between the markers, failing on unbalanced markers

  Verify with unit tests in a temp dir for every agent-setup spec scenario, plus an unbalanced-marker test and a subdirectory-invocation test.
- [ ] 4.3 Make `devy init` call the shared agent setup with default options after writing `devy.yml` in each mode. Turn every non-success outcome (not-ours skill, unbalanced markers, I/O error) into a stderr warning that suggests `devy agent-setup`, keeping exit 0. Verify with extended `init` tests for each init scenario in the agent-setup spec: skill written, existing `AGENTS.md` updated but none created, hand-written skill left unchanged with a warning, nothing written on the existing-`devy.yml` failure, and nothing written with `--show-context`. Update any `tests/cli.rs` init test that asserts the exact set of files created.
- [ ] 4.4 Add the README "Using devy with coding agents" section, and a line in the `devy init` section saying it also writes the skill. Cover `devy agent-setup` and its flags (for existing projects and after upgrading devy), what the skill tells agents, the suggested `.claude/settings.json` allow/ask rules, and the warning that allow-listing `devy exec` allows any command. Note that the MCP server is not provided. Also note in the changelog or release notes that `exec` and `agent-setup` are now reserved names. Verify that the settings snippet is valid JSON and the commands match `devy agent-setup --help`.

## 5. Shell completion

- [ ] 5.1 Update the zsh, bash and fish snippets in `src/commands/hook.rs`: `exec` and `agent-setup` as subcommands, `--json` after `status`/`services`/`check`, `--force`/`--agents-md`/`--print` after `agent-setup`, and command-name completion after `exec`. Verify that the `all_builtin_subcommands_appear_in_*_snippet` tests pass (they enumerate `builtin_subcommands()`), plus new assertions for the flag completions in all three snippets.

## 6. Integration checks

- [ ] 6.1 Add a `tests/cli.rs` test that runs `status --json`, `services --json` and `check --json` against a temp project. Assert that each stdout parses as one JSON object with `version: 1` and no ANSI escapes, that the field sets match the spec, and that exit codes match the non-JSON commands. Run `devy agent-setup` in that project and assert the files exist.
- [ ] 6.2 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`, and verify all pass. Manually run `devy agent-setup` in a sample project, start Claude Code there, and confirm the devy skill loads and the agent uses `devy services --json` and `devy exec` when asked to run the tests.
