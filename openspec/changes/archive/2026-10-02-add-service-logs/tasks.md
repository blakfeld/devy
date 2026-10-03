# Tasks

## 1. Log source abstraction

- [x] 1.1 Add `LogSource` (`Files`, `Command`, `Unsupported`) and a defaulted `PackageManager::log_source(name, lines, follow)` in `src/package_manager/mod.rs` that returns the generic `Unsupported` message; verify with `cargo build`, and check that the existing test doubles in `package_manager/mod.rs` compile unchanged
- [x] 1.2 Add a `Module::log_source(pm, dep, lines, follow)` default that delegates with `self.service_name(dep)`, plus a defaulted `Module::extra_log_paths(data_dir) -> Vec<PathBuf>`, in `src/modules/mod.rs`; verify with a unit test that the default passes postgresql's backend name `postgresql` through to a recording fake PM
- [x] 1.3 Override `extra_log_paths` for nginx (`error.log`, `access.log`), elasticsearch and opensearch (`logs/`), kafka (`app-logs/`) and rabbitmq (`log/`); verify each with a unit test in its module file

## 2. Backend log sources

- [x] 2.1 Nix on macOS: extract the existing `$TMPDIR/devy-<name>.log` path into one helper that both `write_launchagent` and `log_source` use, and return `Files([path])`; verify with a unit test that the two paths are identical
- [x] 2.2 Nix on Linux: return `Command journalctl --user -u devy-<name>.service -n N --no-pager -o cat [-f]`; verify with a unit test on the generated argv for both values of follow
- [x] 2.3 Brew: parse `log_path` and `error_log_path` from `brew services info --json <name>` into `Files`, and return the `Unsupported` brew hint when both are absent; verify the parsing with unit tests on JSON fixtures that have both keys, one key, and none
- [x] 2.4 Apt: return `Command journalctl -u <name> -n N --no-pager -o cat [-f]` without sudo; verify with a unit test on argv
- [x] 2.5 Winget: return `Unsupported("Logs are not available for winget-managed services — check Windows Event Viewer or the service's own log directory")`; verify with a unit test on the exact message

## 3. Tailing and the `devy logs` command

- [x] 3.1 Implement last-N-lines file reading (backwards block scan) and polling follow with truncation reset in `src/commands/logs.rs`; verify with temp-file unit tests covering: fewer than N lines, exactly N, a missing trailing newline, an appended line seen by follow, and truncation restarting from 0
- [x] 3.2 Implement command sources: run `journalctl` and `docker logs` to completion for tail mode, and stream them for follow; classify empty output as "no logs yet", and classify a journal permission error as the apt hint; verify classification with unit tests on fake stdout/stderr/exit triples
- [x] 3.3 Add the `Logs { name: Option<String>, -n/--lines (u32 ≥ 1, default 100), -f/--follow }` subcommand in `src/cli.rs`, resolving names with the same lookup and errors as `service::resolve` (factor it out if needed); verify with `tests/cli.rs` cases for `'nosuch' not found in devy.yml dependencies`, `'node' is not a service`, and `-n zero` exiting 2
- [x] 3.4 Implement single-service output, then `· also see <path>` hints for existing extra paths (nix only, not in follow), and `· No logs yet for <name>` (with the expected path on nix on macOS); verify with `logs_impl` unit tests against a fake PM that returns temp-file sources
- [x] 3.5 Implement all-services mode: sections with `output::header` in declaration order, `No services defined.` when there are none, and a threaded follow with `<name> | ` prefixes; verify ordering and prefixes with a unit test using two temp-file sources and a bounded follow
- [x] 3.6 Add the `ctrlc` dependency and make Ctrl-C during follow exit 0; verify manually with `devy logs -f` against a running nix redis, then Ctrl-C and `echo $?` prints `0`
- [x] 3.7 Document `devy logs` (flags, per-backend sources, the winget limitation and the apt journal hint) in README.md, and verify that the documented examples run as written against a nix project

## 4. Pointing failures at `devy logs`

- [x] 4.1 Append `— run devy logs <name>` to the health-timeout warnings in `src/commands/service.rs` (start and restart), and change the `wait_for_stopped` message to `… try stopping it manually or run devy logs <name>`; verify by updating the affected unit tests in `service.rs` and `modules/mod.rs` and running `cargo test`

## 5. Completion and help

- [x] 5.1 Add `logs` and `ask` to the zsh, bash and fish completion in `src/commands/hook.rs`, along with the `logs` flags `--follow --lines --explain --show-context` and `ask --show-context`, and service names (from `devy services`-style output, or a new hidden `_services` subcommand) after `logs`; verify with the hook unit tests that assert on each snippet. If `add-docker-service-manager` was archived first, keep its `--volumes` entry
- [x] 5.2 Verify that `devy --help` lists `logs` and `ask`, with a `tests/cli.rs` assertion

## 6. Docker log source (with or after `add-docker-service-manager`)

- [x] 6.1 In the docker backend, return `Command <container_cli> logs --tail N [-f] devy-<project>-<service>`, and treat "No such container" as no logs yet; verify with unit tests on argv for `docker` and `podman`, and on the no-container classification

## 7. Environment assistant (requires `add-ai-init` applied)

- [x] 7.1 Implement `collect_context(config, root, pm, scope)`, where scope is the whole project or one service. It assembles `devy.yml`, `devy.lock`, OS, the package manager, the install and run status table, and logs (50 lines per service, with a 5 s per-source timeout and failures recorded as notes), then applies `ai-assist` redaction and the roughly 60 KB cap that drops the oldest log lines first. Verify with unit tests that a secret env value is redacted, that a winget note is present, and that the cap is enforced
- [x] 7.2 Add `devy ask "<question>" [--show-context]` in `src/cli.rs` and `src/commands/ask.rs` with the fixed ask system prompt. `--show-context` prints the request text and returns before `ai-assist` is called. Verify with a unit test using the `ai-assist` mock transport that the question and context reach the request, and with a `tests/cli.rs` test that `--show-context` succeeds with no API key set and a missing question exits 2
- [x] 7.3 Add `--explain` and `--show-context` to `devy logs`. `--explain` requires a name and conflicts with `--follow` (clap usage errors exit 2), uses `--lines` for the log size, and skips the API with `· No logs yet` when the logs are empty. Verify with `tests/cli.rs` for both usage errors, and with a mock-transport unit test for the request contents and the printed diagnosis
- [x] 7.4 Show a progress indicator only when stdout is a TTY, and map `ai-assist` failures to `error: …` with exit 1; verify with a mock-transport unit test for the error chain and an `ask … > file` check that the file holds only the answer
- [x] 7.5 Document `devy ask` and `devy logs --explain` in README.md, including exactly what context is sent, redaction, and `--show-context` for auditing; verify that the documented `--show-context` example runs without an API key

## 8. Integration check

- [x] 8.1 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`, and confirm `openspec validate add-service-logs --strict` passes

## 9. Doctor log retrieval

- [x] 9.1 Replace `devy doctor`'s `NoLogs` placeholder with log retrieval through the service runners (the same `log_source` + `logs::collect` path as `devy logs`, 50 lines, 5 s per-source timeout), so affected services' logs reach the diagnosis as the environment-doctor spec requires; services with no logs, unsupported backends and collection failures stay `no logs available`. Verify with a unit test that a stopped service's log file reaches the doctor request through a fake package manager, and rerun the task 8.1 checks
