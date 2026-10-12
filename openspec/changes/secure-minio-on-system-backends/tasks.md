# Tasks

## 1. Reproduce the bug with failing tests

- [ ] 1.1 In `src/modules/minio.rs` tests, add `start_under_brew_does_not_invoke_service_manager`. With `MockPackageManager { name: "brew", .. }` and a dep that sets `access_key`/`secret_key`, assert that `MinioModule.start(...)` returns `Err` and `pm.started_launches` is empty. Verify it FAILS on current code (today it records `[None]`, meaning `brew services start minio` ran with no credentials or loopback address).
- [ ] 1.2 Add the same test for `name: "apt"` (`start_under_apt_does_not_invoke_service_manager`). Verify it fails on current code.
- [ ] 1.3 In `src/commands/check.rs` tests, add `check_reports_issue_for_minio_under_brew`, modelled on `check_impl_emits_config_warnings_for_minio_with_credentials`, with a brew mock pm. Assert that an issue (not a warning) mentions `every interface` and `docker: true`. Verify it fails on current code.
- [ ] 1.4 Record the three failing test names and their failure output in the PR description as the reproduction.

## 2. Backend-aware config issue hook

- [ ] 2.1 Add `fn backend_config_issues(&self, _dep: &Dependency, _pm: &dyn PackageManager) -> Vec<String>` to the `Module` trait in `src/modules/mod.rs`, defaulting to `vec![]`, with a doc comment. Verify with `cargo build`, and with a unit test that `get("redis").backend_config_issues(..)` is empty under brew.
- [ ] 2.2 In `src/commands/check.rs`, push `backend_config_issues` results as `Note::Issue` (prefixed `<dep>: `) for deps with `!dep.docker`. Verify that test 1.3 now passes, and add `check_no_minio_backend_issue_when_docker` (brew pm, `docker: true`), which asserts no such issue.
- [ ] 2.3 In `src/commands/up.rs`'s "validate config" loop (non-docker deps, before ports/install), bail on the first `backend_config_issues` entry with `"<dep>: <issue>"`. Verify with a unit test, or with the CLI test in 3.3, that `devy up` fails before any install call (the mock pm's install log is empty).

## 3. MinIO refusal

- [ ] 3.1 In `src/modules/minio.rs`, add a helper that returns the refusal message for `pm.name()` in `brew` (`Homebrew`) or `apt` (`apt`), else `None`. Use the message text from design.md Decision 5. Implement `backend_config_issues` with it. Verify with unit tests: brew and apt return one issue; nix, winget and docker deps return none.
- [ ] 3.2 Make `MinioModule::start` bail with the same message under brew or apt before `start_via_pm`. If `pm.is_service_running("minio")` is `Ok(true)`, append `(an existing Homebrew/apt MinIO is still running — stop it with brew services stop minio / systemctl stop minio)`. Verify that tests 1.1 and 1.2 now pass, and add a test that the hint appears when the mock reports the service running.
- [ ] 3.3 In `tests/cli.rs`, add a CLI test: a temp project with `.git`, a `devy.yml` with `package_manager: brew` and `minio` (no docker), then `devy check`. Assert a non-zero exit and that the stderr/stdout contains the refusal. Gate the test with `#[cfg(unix)]` if brew/apt selection is rejected on Windows. Verify with `cargo test --test cli`.
- [ ] 3.4 Confirm the existing nix and docker MinIO tests still pass unchanged (`cargo test minio`).
- [ ] 3.5 In `README.md`, update the `minio` row (line ~709) or add a footnote: under brew/apt, MinIO requires `docker: true` or the nix backend because the system service binds every interface with default credentials. Verify that the rendered table still parses (`markdownlint`, if configured, or a visual check).

## 4. Integration checks

- [ ] 4.1 Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`. All must pass.
- [ ] 4.2 Run the `code-reviewer` and `security-analyst` subagents on the diff, as required by `.claude/rules/review-before-done.md`, and address every finding.
