# Tasks

## 1. Shared groundwork

- [ ] 1.1 Extract the `.devy-lock` guard from `up::run` into a helper in `src/commands/shared.rs` (same error messages) and use it from `up`; verify `cargo test` passes and the "Concurrent runs queue" behavior is unchanged
- [ ] 1.2 Add `tar` and `flate2` (default `miniz_oxide` backend) to `Cargo.toml`; verify `cargo build` succeeds and `cargo tree -e normal` shows no C-backed compression crate
- [ ] 1.3 Add an optional extra-env parameter to `exec::spawn_cmd` (existing callers pass none); verify existing exec tests pass and a new test shows a child sees an injected variable

## 2. Snapshot core (nix-managed services)

- [ ] 2.1 Implement snapshot-name validation and `.devy/snapshots/` layout helpers (`.gitignore` with `*`, manifest read/write with creation time, devy version, pm, per-service canonical name + lock version); verify with unit tests for valid/invalid names (including `../escape`) and manifest round-trip
- [ ] 2.2 Implement archiving a data dir to `<svc>.tar.gz` (relative paths, permissions preserved, Unix sockets and `*.pid` skipped) and safe extraction (reject absolute/`..` entries); verify with temp-dir tests including a socket file and a 0700 directory
- [ ] 2.3 Implement eligibility (nix backend only, with the specified messages for ineligible, none-eligible and unknown services); verify with `MockPackageManager` tests for nix vs brew
- [ ] 2.4 Implement `snapshot save` per the spec: stop → archive → restart-if-was-running, staging at `.<name>.saving/`, `--force` replacement, partial cleanup and restart on failure, "no data yet — skipped" warning, `.devy-lock` held; verify with tests covering running/stopped services, existing-name refusal, and failure cleanup
- [ ] 2.5 Implement `snapshot restore`: confirmation prompt (`IsTerminal`), `--yes`, non-tty refusal, rename-aside → extract → swap → delete-old, rollback on extraction failure, restart-if-was-running, warning on leftover `.restore-old`/`.restoring` dirs; verify with tests for decline, non-tty, truncated archive rollback, and success
- [ ] 2.6 Implement the major-version guard with `--force` override; verify tests for `15.6`→`16.2` refusal, `7.2.4`→`7.2.5` pass-through, and unknown versions
- [ ] 2.7 Implement `snapshot list` (newest first, size, `(invalid)` entries, `○ no snapshots`) and `snapshot delete`; verify with unit tests
- [ ] 2.8 Wire `snapshot save|restore|list|delete` into `src/cli.rs`; add `tests/cli.rs` cases for `devy snapshot` (exit 2 usage), invalid name, and `snapshot list` in an empty project; document snapshots in README (eligibility, stop/restart behavior, `.devy/snapshots/`) and verify the README examples match the CLI help

## 3. Seed configuration and loaders

- [ ] 3.1 Parse the `seed` extra value into path / path list / command forms and accept `seed` for every service module in the `check.rs` allowlist logic; verify `devy check` accepts `seed` on postgres and still rejects it on `node`
- [ ] 3.2 Add the `Module` file-seed hook and implement it for postgresql (`psql -X -v ON_ERROR_STOP=1 … -f`), mysql/mariadb (`mysql -u root`, stdin), redis (`redis-cli`, stdin) and mongodb (`mongosh`); verify unit tests assert exact argv per module and resolved port, and that no `sh -c` is used
- [ ] 3.3 Implement missing-tool errors (`seeding needs <tool> on PATH`, mongosh hint) using devy's setup-tool PATH; verify with a test using an empty PATH
- [ ] 3.4 Add seed validation to `devy check` (bad form, missing file, file seed on command-only module, wrong extension); verify each message via `check_impl` tests and that each counts as an issue

## 4. Seed execution and tracking

- [ ] 4.1 Implement the seed fingerprint (FNV-1a over canonical definition + file bytes) and record storage (`.devy/data/<svc>/.devy-seed.json` under nix, `.devy/state/seeds/<svc>.json` otherwise); verify tests for fingerprint stability and change detection, and record location per backend
- [ ] 4.2 Implement command seeds with the merged project env (HOST/PORT, module vars, `environment:`) via `spawn_cmd` extra env; verify a test command observes `REDIS_PORT`
- [ ] 4.3 Integrate seeding into the `devy up` service phase after readiness (apply / `already seeded` / drift warning / skip-when-unready warning / `Failed to seed <dep>` abort); pass merged env to phase 2; verify `up_impl` tests for each branch, including that `after_up` does not run after a seed failure
- [ ] 4.4 Implement `devy seed [<service>]... [--force]` (running + healthy required, `has no seed configured`, `○ no seeds configured`, `.devy-lock` held) and wire it into `src/cli.rs`; verify unit tests and a `tests/cli.rs` case for `devy seed` with no seeds
- [ ] 4.5 Document `seed:` in README's devy.yml reference and service section (file types per module, run-once semantics, nix vs shared-backend tracking, `devy seed --force`); verify the documented YAML parses with `devy check` in a temp project

## 5. Shell integration

- [ ] 5.1 Add `snapshot` (actions, `--service`, `--force`, `--yes`) and `seed` (`--force`, `--generate`, `--out`) to the zsh, bash and fish completion scripts in `src/commands/hook.rs`; rebase onto `add-docker-service-manager`'s Tab completion text if it has been archived first; verify hook tests assert the new candidates for all three shells

## 6. Docker-managed snapshots (after `add-docker-service-manager` is archived)

- [ ] 6.1 Extend eligibility to docker-managed services and implement save/restore of the named volume via a throwaway container producing the same `<svc>.tar.gz` layout; verify with tests on the generated container commands and a manual round-trip against a local docker redis
- [ ] 6.2 Update the `service-snapshots` eligibility wording, scenarios and README for docker; verify `openspec validate add-service-snapshots` passes

## 7. AI seed generation (after `add-ai-init` provides `ai-assist`)

- [ ] 7.1 Implement schema-only extraction for postgresql (`pg_dump --schema-only`) and mysql/mariadb (`mysqldump --no-data`), and the unsupported-module error; verify unit tests on argv and that no data-reading flags are passed
- [ ] 7.2 Implement `devy seed --generate <service> [--out]` through `ai-assist`: write to `db/seed.generated.sql` by default, refuse to overwrite, print the suggested `seed:` config, never execute; verify with a stubbed `ai-assist` client test and the not-enabled path sending nothing
- [ ] 7.3 Document `--generate` in README (what is sent, opt-in, review-before-use); verify the doc matches `devy seed --help`

## 8. Integration checks

- [ ] 8.1 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`; all pass
- [ ] 8.2 Manual end-to-end on macOS or Linux with nix: `devy up` with a postgres `seed:`, `devy snapshot save base`, mutate data, `devy snapshot restore base --yes`, confirm the original rows and that `devy up` reports `already seeded`
