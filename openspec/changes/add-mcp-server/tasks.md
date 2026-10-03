# Tasks

## 1. Output sink and data-returning helpers

- [ ] 1.1 Add a stderr sink mode to `src/output.rs`. In this mode every `output::*` function writes to stderr with colors disabled. Verify with a unit test that captures the sink and checks that nothing goes to stdout.
- [ ] 1.2 Factor the service list into a data helper that returns name plus running state, and have `service::list_impl` render from it. Verify that existing `service` tests pass and a new test covers the helper with `MockPackageManager`.
- [ ] 1.3 Factor `status_impl` and `check_impl`, and the `shared::print_*_table` functions they use, to render into an `impl Write`. `check_impl` should also return the issue count. The CLI keeps writing to stdout. Verify that existing `status` and `check` tests pass unchanged, plus a test that renders into a `Vec<u8>`.
- [ ] 1.4 Extract one shared resolver for the project environment: module `env_vars` merged with config `environment` through `merge_env`, plus `path_prepends` and lock-resolved ports. Have the shadowenv writing path in `up.rs` use it. Verify that existing `up` and shadowenv tests pass and a new test asserts that resolver output matches what shadowenv receives.
- [ ] 1.5 Add a captured, deadline-aware variant of `exec::spawn_cmd`. It needs piped stdio drained on threads, a 64 KiB tail cap, process-group kill on Unix and `Child::kill` on Windows. Verify with unit tests for normal exit, non-zero exit, truncation and a timeout using `sleep`.

## 2. MCP protocol core

- [ ] 2.1 Add `src/commands/mcp.rs` with a newline-delimited JSON-RPC loop over stdin and a single locked stdout writer. It should handle `initialize` with version negotiation over `["2025-06-18", "2025-03-26"]`, `notifications/initialized`, `ping`, `-32700` for bad JSON, `-32601` for unknown methods, and exit 0 on EOF. Verify with unit tests that feed byte buffers in and assert the response lines.
- [ ] 2.2 Add the `Mcp { allow_mutations: bool }` variant to `src/cli.rs`. It switches the output sink to stderr before serving. Verify with `devy --help` listing `mcp`, `devy mcp --help` showing `--allow-mutations`, and a `cli.rs` parse test.
- [ ] 2.3 Implement `tools/list` and `tools/call` dispatch: input schemas, tool results with text plus `structuredContent`, `isError` for tool failures, and the "devy.yml not found" tool error when no project exists. Verify with unit tests for list contents with and without mutations, and for a call made outside a project.

## 3. Read-only tools

- [ ] 3.1 Implement `services`, `ports` and `list_commands` from the data helpers. Re-read `devy.yml` and `devy.lock` on every call. Verify with unit tests that use `MockPackageManager` and a temp project, including a config edit between calls.
- [ ] 3.2 Implement `get_env` using the shared resolver with redaction (name patterns, ignoring case, and URL userinfo passwords). Verify with unit tests for the spec scenarios `STRIPE_SECRET_KEY`, `DATABASE_URL` with a password, and `LOG_LEVEL`.
- [ ] 3.3 Implement `status` and `check` using the rendered `impl Write` output. `check` returns `{issues, passed}`. Verify with a unit test showing that a failing check is a normal result, not a JSON-RPC error, and that no `devy.lock` or `.shadowenv.d` is created.

## 4. Mutating tools

- [ ] 4.1 Implement gating. Without `--allow-mutations`, mutating tools are left out of `tools/list` and `tools/call` returns an error naming `--allow-mutations`. Verify with unit tests.
- [ ] 4.2 Implement `start_service`, `stop_service`, `restart_service` and `up` by running `current_exe()` as a child with `NO_COLOR=1`, closed stdin and captured output. `up` never passes `--bootstrap` or `--update`. Log each call to stderr. Verify with tests that a missing service name and a missing `devy.yml` return `isError` with the CLI's message.
- [ ] 4.3 Implement `run_command`. It accepts only names from `commands:`, appends `args` with `sh_quote`, runs with the shared project environment, uses the default 600 s timeout that `timeout_secs` can lower, and reports exit status and output. Verify with unit tests for the spec scenarios: arbitrary name rejected without a spawn, args injection kept as one argument, env var present, and timeout.

## 5. Shell integration and docs

- [ ] 5.1 Add `mcp` to the zsh, bash and fish completion lists in `src/commands/hook.rs`, with `--allow-mutations` completed after `mcp`. Verify that the updated `hook.rs` tests assert `mcp` appears in all three snippets.
- [ ] 5.2 Add a README section, "Using devy with coding agents". It should cover the `claude mcp add devy -- devy mcp` registration, the `--allow-mutations` trade-off (agents can run hooks and project commands), the tool list, and the fact that redaction is best-effort. Verify that the documented commands match `devy mcp --help`.

## 6. Integration checks

- [ ] 6.1 Add a `tests/cli.rs` test that spawns the real `devy mcp` in a temp project. It sends `initialize`, `tools/list`, and calls to every read-only tool, then closes stdin. Verify that every stdout line parses as JSON-RPC, that the process exits 0, and that no files are written.
- [ ] 6.2 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`, and verify that all pass. Manually register with Claude Code and confirm that the tools appear and `services` returns data.
