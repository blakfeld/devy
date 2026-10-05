# Tasks

## 1. Remove the trust gate and `devy allow`

- [x] 1.1 Delete `src/commands/allow.rs`, the `Allow` subcommand in `src/cli.rs`, and its module registration
- [x] 1.2 Delete `src/trust.rs`; move the per-user state directory (location, 0700 creation, owner check, "not inside the project" refusal) to `src/state_dir.rs` for the shadowenv copies
- [x] 1.3 Remove the gate from `up`, `down`, `start`/`restart` and `exec`, and the `NotAllowed` handling in the failure record
- [x] 1.4 Remove trust refresh after devy's own lock write (`LockFile::write` no longer returns bytes) and after accepted `doctor` fixes; keep `--yes` refusing executable-field changes
- [x] 1.5 AI `init`: keep the interactive review/strip of executable fields; stop recording trust on confirm

## 2. Shadowenv and export

- [x] 2.1 Drop the `trust` flag from `EnvManager::setup`: `devy up` runs `shadowenv trust` whenever `.shadowenv.d` holds only devy's own files; keep the foreign-entry refusal and the managed-path untrust
- [x] 2.2 Drop gate-only helpers (`forget_env_file`, the gate's replaced-`500_devy.lisp` untrust); keep the per-prompt guard, nonce and copies
- [x] 2.3 `devy export` writes `environment` entries as escaped attributes again; remove the commented-out path and warning

## 3. Completions, docs, CI

- [x] 3.1 Remove `allow`/`--revoke` from the bash, zsh and fish completion snippets and their tests
- [x] 3.2 README: remove "Trusting a project" and other `devy allow` references; agent skill and AGENTS.md block no longer mention `devy allow`
- [x] 3.3 Remove `devy allow` from `.github/workflows/integration.yml` and `.github/scripts/doctor-e2e.sh` (keep `XDG_STATE_HOME` isolation for the state directory)

## 4. Tests

- [x] 4.1 Delete trust unit and CLI tests; move state-directory tests to `state_dir.rs`
- [x] 4.2 Hostile fixture: `up`/`exec` on the valid-but-hostile config are still refused by the managed-path checks before hooks run, without the not-allowed and declined-prompt steps
- [x] 4.3 CLI tests: `up` runs hooks without a prompt; `allow` is not a subcommand; `export` writes environment entries
- [x] 4.4 `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` pass

## 5. Specs

- [x] 5.1 Retire `project-trust` (REMOVED requirements, `retire_capabilities: true`)
- [x] 5.2 MODIFIED requirements in cli, environment-up, shell-environment, shell-integration, nix-export, project-config, environment-doctor, filesystem-safety and package-managers; ADDED "Executable entry listing" to project-config
- [x] 5.3 `openspec validate remove-project-trust` passes
