# Tasks

Prerequisites: `add-ai-init` (`ai-assist`) and `add-service-logs` are implemented.

## 1. Check findings as data

- [x] 1.1 Extract a `Findings` collector from `check_impl` in `src/commands/check.rs` (issues, warnings, optional hard error), with `check_impl` printing from it. Verify that all existing `check.rs` unit tests and `tests/cli.rs` check tests pass unchanged.
- [x] 1.2 Add unit tests for the collector that capture hard errors (port conflict, multi-key dependency) as `hard_error` instead of returning `Err`. Verify with `cargo test check::`.

## 2. Failure record and hint in `devy up`

- [x] 2.1 Add `UpProgress` step and dependency tracking in `up_impl` at each phase boundary and inside the install and service loops. Verify with a unit test using a mock package manager whose install fails, asserting step `install` and the dependency name.
- [x] 2.2 In `up::run`, split config location from parsing. On error, write `.devy/last-up-failure.json` atomically (mode 0600 on Unix). On success, delete it. A write failure is a single warning. Verify with unit tests for write, replace, delete-on-success and an unwritable `.devy` directory.
- [x] 2.3 Add `HintedError` and update `main.rs` to print `error:` followed by `  · run devy doctor to diagnose this failure`. Verify with a `tests/cli.rs` test: a failing `before_up` hook leaves the record, stderr shows the hint after the error, and the exit code is 1. Also verify that `devy up --dry-run` writes no record.

## 3. `devy doctor` offline path

- [x] 3.1 Add the `Doctor { yes, no_ai }` subcommand in `src/cli.rs` and create `src/commands/doctor.rs` with the header, the `Checks` section from the collector and the `Last devy up failure` section. Verify with `devy --help` listing `doctor`, and with a cli test for the header and the outside-project error.
- [x] 3.2 Implement the healthy path (`✓ no problems found`, no AI call) and the no-AI path (`· AI diagnosis unavailable — <reason>`). Verify with cli tests: no key, `--no-ai`, invalid YAML reported as a finding, all exiting 0 with no network use (transport mock asserts zero calls).

## 4. AI diagnosis

- [x] 4.1 Build the request bundle (failure record, findings, `devy.yml`, `devy.lock`, platform, backend, version, last 50 log lines of affected services only) through `ai-assist` redaction and consent. Verify with a unit test asserting the bundle includes failed-service logs and excludes healthy-service logs and the environment file.
- [x] 4.2 Write the system prompt (devy.yml schema summary, module known keys, backend notes) and parse the structured `{summary, likely_cause, steps, devy_yml}` response, retrying once on malformed JSON. Verify with unit tests on canned responses, including malformed-then-valid and twice-malformed (warns `AI diagnosis failed`, exits 0).
- [x] 4.3 Print the `Diagnosis` section with numbered steps, never executing commands. Verify with a unit test asserting no process is spawned for a response containing commands.

## 5. Suggested fix

- [x] 5.1 Implement `validate_proposed_config(text)` covering parse, normalization, `validate_config`, known keys, shells and read-only ports. Verify with unit tests for valid, multi-key-entry, unknown-key and port-conflict proposals.
- [x] 5.2 Implement the in-crate line diff and print it under `Suggested fix`. Verify with unit tests for added, removed and changed lines and an identical file (no fix offered).
- [x] 5.3 Implement the apply flow: `--yes`; tty prompt defaulting to No; non-tty prints the `--yes` hint; atomic write preserving permissions; `error:` and exit 1 on write failure. Verify with unit tests using an injected reader and tty flag for `y`, `YES`, Enter and non-tty, plus a read-only file case.

## 6. Completions and docs

- [x] 6.1 Add `doctor` with `--yes` and `--no-ai` to the zsh, bash and fish snippets in `src/commands/hook.rs`. Verify with the existing hook tests, extended to assert the new candidates.
- [x] 6.2 Document `devy doctor`, the failure record, the AI data sent and the `.devy/` gitignore recommendation in `README.md`. Verify that the documented commands run as written against a sample project.

## 7. Integration check

- [x] 7.1 End to end on macOS and Linux CI: break a project (a port conflict via `devy.lock`), run `devy up` (record and hint), then `devy doctor --no-ai` (findings shown, exit 0), then fix and run `devy up` (record removed). Verify that `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` all pass.
  - Passed on the PR #17 merge (aa39351): `.github/scripts/doctor-e2e.sh` printed `doctor e2e passed` on Nix Linux and Nix macOS (Integration run 37079917949), and the CI run 37079917948 passed Test (Linux x86_64/arm64, macOS, Windows), Clippy and Format.
