# Tasks

## 1. Reproduce the bug with failing tests

- [ ] 1.1 In `src/modules/java.rs` tests, build a fake Homebrew prefix in a temp dir (`opt/openjdk` → `Cellar/openjdk/21.0.5/`, with `libexec/openjdk.jdk/Contents/Home/bin/java` and `bin/java` linking to it) and a brew-named `MockPackageManager` whose package bin dir points into it; assert that `JAVA_HOME` and the Java PATH entry are that `Contents/Home` and its `bin`. Verify the test fails on the current code (it returns the system JDK or nothing)
- [ ] 1.2 Add a pinned-version test (`version: "17"`, fake `opt/openjdk@17` plus `opt/openjdk`) asserting the `openjdk@17` home is chosen, and a not-installed test asserting no `JAVA_HOME` and no PATH entry under brew; verify the pinned test fails on the current code (it never returns the fake `openjdk@17` home) and the not-installed test fails on any machine with a system JDK
- [ ] 1.3 Add a `src/project_env.rs` test with a brew-named mock whose package bin dir for `postgresql@16` is `<tmp>/opt/postgresql@16/bin` and a `postgres` dependency with `version: "16"`; assert that directory is in `path_prepends`. Verify it fails on the current code

## 2. Package-manager per-package bin directory (package-managers)

- [ ] 2.1 Add `PackageManager::package_bin_dir(&self, pkg: &Dependency) -> Option<PathBuf>` with a default of `None`, plus a settable field on `MockPackageManager`; verify `cargo build` and the existing `mod.rs` tests pass
- [ ] 2.2 Implement it for Homebrew (D1): prefix from the parent of `brew_bin()`'s directory, refuse a project-local prefix, return `<prefix>/opt/<brew_formula_name(pkg)>/bin` only when it is a directory, never running `brew`; verify unit tests for an existing dir, a missing dir, a `name@version` pin, a lock-sourced version (no `@`), and a project-local prefix (returns `None`)
- [ ] 2.3 Implement it for nix as the verified profile bin; verify a unit test that a non-store profile link yields `None`

## 3. Modules expose their backend package (dependency-modules)

- [ ] 3.1 Add `Module::backend_package(&self, pm, dep) -> Option<Dependency>` (default `None`) and `Module::backend_path_prepends(&self, dep, pm, project_root) -> Vec<String>` (default empty); implement `backend_package` for `GenericModule`; verify `cargo build`
- [ ] 3.2 Implement `backend_package` in every module whose `install` calls `pm.install_package`, and rewrite those `install`/`is_installed` bodies to call it (D2); verify with `grep -rn "install_package(" src/modules` that every call site goes through `backend_package`, and that the existing per-module install argv and installed-check tests still pass
- [ ] 3.3 Add a test that `backend_package` matches the dependency passed to `install_package` for node (`node@22`), postgres (`postgresql@16`), mysql (`mysql@8.4`) and java (`openjdk@17`) on a brew mock; verify it passes

## 4. Java derives JAVA_HOME from the installed JDK (dependency-modules)

- [ ] 4.1 Replace `JavaModule::env_vars`/`path_prepends` with `backend_env_vars`/`backend_path_prepends` that call a `java_home(pm, dep)` helper (D3): brew and nix canonicalize `<package_bin_dir>/java`, go up two levels, require `bin/java`, require `/nix/store/` under nix, and never fall back; apt and winget keep `detect_java_home()`. Verify tasks 1.1 and 1.2 now pass
- [ ] 4.2 Add a nix test with a fake store layout (`<tmp>/store/.../lib/openjdk/bin/java` behind the profile link) checking the store-prefix guard rejects a home outside `/nix/store`, and an apt test showing `/usr/lib/jvm/default-java` behavior is unchanged; verify they pass
- [ ] 4.3 Update the Java section of README.md (and `docs/` if it mentions `JAVA_HOME`) to say `JAVA_HOME` follows the installed JDK and pinned version under brew and nix; verify the text matches the spec delta

## 5. One deduplicated PATH list (shell-environment)

- [ ] 5.1 In `project_env::resolve`, append each dependency's `pm.package_bin_dir(backend_package)` after `pm.path_prepends`, then module `path_prepends` and `backend_path_prepends`, and drop later duplicates (D4); verify task 1.3 passes and a new test that a nix profile bin returned twice appears once, first
- [ ] 5.2 Switch `src/commands/check.rs` to take its PATH list from `project_env::resolve(..., PortMode::ReadOnly)`; verify a check unit test where brew contributes `opt/<formula>/bin` and the written file matches, so no stale-environment issue is reported
- [ ] 5.3 Update the `up.rs` tests that assume brew writes no environment file (for example `up_impl_skips_env_section_when_no_env_and_no_path_prepends`) to use a mock with no package bin dirs, and add one where a brew package bin dir makes `up` write the file; verify `cargo test`

## 6. Integration

- [ ] 6.1 Add a `tests/cli.rs` case (macOS/Linux only) with `HOMEBREW_PREFIX` pointing at a fake prefix outside the project containing `bin/brew` (a stub) and `opt/openjdk/bin/java` → `libexec/.../bin/java`, running `devy check --json` for a `java` dependency; assert the expected PATH entries include the opt bin and the JDK home's `bin`. Verify it passes
- [ ] 6.2 Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`; verify all pass
- [ ] 6.3 Manually on macOS with Homebrew and no system JDK: `devy up` a project with `java` under brew, then `devy exec -- java -version` and `devy exec -- sh -c 'echo $JAVA_HOME'`; verify the Homebrew JDK is used (with `use-installed-toolchain-in-post-setup` applied, `mvn dependency:resolve` also uses it)
