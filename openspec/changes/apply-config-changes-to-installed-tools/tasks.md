# Tasks

## 1. Rust: installed check reflects toolchain, targets and components

- [ ] 1.1 Add failing unit tests in `src/modules/rust.rs` reproducing the bug: with rustup present but a configured target, component or toolchain missing (injected `rustup target list --installed` / `component list --installed` output, or toolchain query failing), the installed decision is "not installed"; today `is_installed` returns true. Verify they fail on the current code with `cargo test rust::`
- [ ] 1.2 Add pure helpers that parse rustup's `--installed` output and report missing targets/components (component matches `<name>` or `<name>-<host triple>`); unit-test exact match, suffixed match, empty output and unknown lines
- [ ] 1.3 Make `is_installed` return true only when rustup is found and the toolchain, targets and components are all installed, querying through `rustup_program()` with `RUSTUP_AUTO_INSTALL=0`, treating any query failure as not installed; verify 1.1 passes
- [ ] 1.4 Make `install()` incremental: run the rustup installer only when rustup is missing, `toolchain install` + `default` only when the toolchain is missing, and `target add` / `component add` only for missing entries; verify with unit tests over the command list built for given installed state (no rustup needed)
- [ ] 1.5 Update the README rust section if it says config changes need a manual `rustup` call; verify `grep -n rustup README.md` shows no stale claim

## 2. TypeScript: global packages applied on every `devy up`

- [ ] 2.1 Add a failing unit test in `src/modules/typescript.rs`: with Node already installed (mock pm `installed: true`) and no `.devy_ts_global_stamp`, `post_setup` attempts `npm install -g typescript …` (assert via a stamp-decision helper, e.g. `globals_to_install(dep, stamp_path) -> Option<Vec<String>>`, which today does not exist / returns nothing); verify it fails on current code
- [ ] 2.2 Move `npm install -g typescript <global_packages…>` from `install()` to `post_setup`, gated by `.devy_ts_global_stamp` (newline-joined list, config order) and written with `helpers::write_stamp_text`, reusing `npm_global_install_args`; run it even without `package.json`; verify 2.1 passes plus tests for unchanged list (skipped), added package (re-run) and hostile package rejected before npm
- [ ] 2.3 Add `.devy_ts_global_stamp` to `.gitignore`, to `tests/fixtures/hostile/symlinks.txt`, and to `setup_steps` output; verify the hostile-fixture CLI test refuses writing through the symlinked stamp (`cargo test --test cli hostile`)

## 3. Bun and deno: honor a pinned version after first install

- [ ] 3.1 Add failing unit tests in `src/modules/bun.rs` and `src/modules/deno.rs` for a `version_satisfied(pinned, installed)` decision used by `is_installed`: pinned `1.41.0` with installed `1.40.0` → not installed; today `is_installed` ignores the version. Verify they fail on current code
- [ ] 3.2 Implement `version_satisfied` (normalize leading `v`, and `bun-v` for bun; bun `latest`/`canary` and unpinned always satisfied; unreadable installed version → not satisfied) and use it in `is_installed` only when `dep.version` is set and `!dep.version_from_lock`, reading the version via `resolved_version`; verify tests for match, mismatch, `v`-prefix, `latest`, `canary`, lock-derived version and missing binary
- [ ] 3.3 Confirm `install()` still passes the pinned version to the verified installer when reinstalling over an existing binary; verify existing `installer_args` tests still pass

## 4. Ruby: install the version passed to `rbenv local`

- [ ] 4.1 Add a failing test in `src/modules/ruby.rs` reproducing the bug: no `version`, no `.ruby-version`, rbenv has only another Ruby; setup must ensure 3.3.6 is installed before `rbenv local 3.3.6`. Extract a pure planning helper (e.g. `setup_plan(dep, has_ruby_version_file, version_installed) -> Vec<RbenvStep>`) so the test needs no rbenv; verify it fails on current behavior (plan contains only `local`)
- [ ] 4.2 In `post_setup`, when `version_to_set` is not installed (`rbenv_version_installed`), run `rbenv install --skip-existing <version>` via `rbenv_outside_project` and `rbenv_version_arg` before `rbenv local`; verify 4.1 passes and a test that an installed version yields no install step
- [ ] 4.3 Keep the winget path unchanged; verify existing winget ruby tests pass

## 5. Specs and integration checks

- [ ] 5.1 Confirm the `dependency-modules` delta (Rust, TypeScript, Ruby, Script-installed runtimes) matches the implemented behavior and messages; verify `openspec validate apply-config-changes-to-installed-tools --strict` passes
- [ ] 5.2 Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`; all pass
- [ ] 5.3 Manual check on a machine with rustup: add `targets: [wasm32-unknown-unknown]` to a devy.yml with rust already set up, run `devy status` (rust not installed) then `devy up` (target added, no toolchain update) then `devy status` (installed)
- [ ] 5.4 Run `code-reviewer` and `security-analyst` on the diff and address every finding
