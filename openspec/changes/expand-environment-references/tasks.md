# Tasks

## 1. Reproduce the bug

- [ ] 1.1 Add a failing unit test in `src/project_env.rs`. It calls `resolve` for a config with `redis` (port locked to 52113) and `REDIS_URL: "redis://${REDIS_HOST}:${REDIS_PORT}"`, and asserts the value is `redis://127.0.0.1:52113`. Verify that `cargo test project_env` fails on current main and shows the literal `${REDIS_HOST}`.
- [ ] 1.2 Add a failing shadowenv test in `src/env_manager/shadowenv.rs` (or up's tests) that runs the up path end to end on a mock package manager. Assert the written file contains `(env/set "REDIS_URL" "redis://127.0.0.1:52113")` and no `${`. Verify that it fails before the fix.
- [ ] 1.3 Add a failing `tests/cli.rs` test. It writes `devy.yml` with `environment: { A: x, B: "${A}-y" }`, runs `devy exec env`, and asserts `B=x-y`. Verify that it fails before the fix.

## 2. Reference syntax validation (project-config)

- [ ] 2.1 Implement a reference parser that accepts `${NAME}` and the `$${` escape and keeps other `$` literal. Hook it into `DevyConfig::validate` so a malformed `${` fails with ``environment.<KEY>: invalid reference "<text>"; write $${ for a literal ${`` (control characters escaped). Verify with unit tests for an unclosed brace, `${1X}`, `${FOO:-x}`, `$${X}` accepted, and `pa$word` accepted.
- [ ] 2.2 Verify that `check::static_issues` (used by AI init and `init --detect`) rejects a malformed reference. Add a unit test, and confirm that the `init --detect` output (`${POSTGRESQL_HOST}:${POSTGRESQL_PORT}`) still validates.

## 3. Expansion in the project environment (shell-environment, environment-exec)

- [ ] 3.1 Implement the pure function `expand(config_env, base_env, pending_ports) -> Result<HashMap>` in `src/project_env.rs`. It does recursive expansion of `environment` entries, no rescanning of substituted text, and cycle detection, and returns distinct errors for undefined, cycle and unassigned port. Verify with unit tests for each case:
  - env-to-env reference
  - module var reference
  - service HOST/PORT
  - `environment` overriding a module var that another entry references
  - self-reference
  - two-key cycle
  - undefined name, including that `${HOME}` is undefined even when set in the process environment
  - unassigned port
  - module value containing `${` left literal
- [ ] 3.2 Make `project_env::resolve` return `Result<ProjectEnv>` using `expand`, collecting the unassigned `<SERVICE>_PORT` names. Update the callers in `up.rs` (fail before writing the environment file), `exec_env.rs` and `status.rs` (the report error, no panic). Verify that tests 1.1 and 1.2 now pass, along with a test that `devy up` with an undefined reference writes no `.shadowenv.d` file.
- [ ] 3.3 In `devy exec`, print the resolution error and exit 1 without spawning. Verify that test 1.3 passes, and add `tests/cli.rs` cases for an undefined reference (exit 1, error text) and for an unassigned port before `up` (error names `REDIS_PORT` and suggests `devy up`).

## 4. Export (nix-export)

- [ ] 4.1 Change `devy export` to compute values from the project environment via `exec_env::project_environment` (read-only ports, no file writes besides the export), keep emitting only `environment` keys, and keep the existing Nix escaping. Update the existing `hi ${USER}` test to `hi $${USER}` → `hi \${USER}`. Verify with tests for an expanded `REDIS_URL` with a locked port, for an undefined reference (exit 1, no file written), and that `devy.lock` is unchanged.

## 5. Check (environment-check)

- [ ] 5.1 In `devy check`, run the expansion over the resolved module and service names and fail with the hard error for undefined references and cycles, while ignoring unassigned-port references. Verify with unit tests in `src/commands/check.rs` for `${NOPE}` (exit 1, message) and for `${REDIS_PORT}` under nix before `up` (no error), and confirm the existing "Environment not yet written" scenario still passes.

## 6. Documentation

- [ ] 6.1 Update the README environment section. Document `${NAME}`, what names are available, `$${`, that host variables are not expanded, the errors, and that `devy export` snapshots the current ports. Verify that the README example config passes `devy check` in a temp project.
- [ ] 6.2 Check that the AI init schema comment in `src/ai/init_prompt.rs` matches the implemented rules: references to other vars, no host vars. Adjust its wording if needed, and verify that the `init_prompt` tests pass.

## 7. Integration

- [ ] 7.1 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`, and confirm that all pass.
- [ ] 7.2 Run the `code-reviewer` and `security-analyst` reviews on the diff and address every finding per `.claude/rules/review-before-done.md`.
