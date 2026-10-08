# Tasks

## 1. Reproduce the bug

- [ ] 1.1 Add `#[cfg(windows)]` tests to `src/modules/helpers.rs`. In a temp dir on PATH, create an `npm.cmd` fixture that writes `%*` to a file. Assert that `run_cmd("npm", &["install", "-g", "--", "pkg@>=1.0", "@a/b@^1"])` succeeds and that the file holds the arguments unchanged. Verify the test fails on `windows-latest` CI before the fix, with "program not found"
- [ ] 1.2 Add `#[cfg(windows)]` tests for `post_setup` of node (`package.json` plus the `npm.cmd` fixture), elixir (`mix.bat`) and java (`pom.xml` plus `mvnw` and a `mvnw.cmd` fixture). Verify they fail on Windows CI before the fix

## 2. Shared resolver

- [ ] 2.1 Add `helpers::setup_tool(name) -> Result<PathBuf>` on top of `fs_safe::which_with_project_profile`, failing with `` `<name>` not found on PATH `` (design D1). Verify with unit tests: a tool found in a temp PATH dir, a missing tool giving the exact message, and a tool only in `<project_root>/bin` being ignored. On Windows, also verify that a `.cmd` fixture is found with the default `PATHEXT`
- [ ] 2.2 Make `run_cmd` resolve bare names through `setup_tool` and pass absolute paths unchanged. Verify with the existing `run_cmd_*` tests in `src/modules/mod.rs` and task 1.1 now passing

## 3. Module call sites

- [ ] 3.1 Route `pm_cmd` in `node.rs` and `typescript.rs` `post_setup`, and the `"bundle"` fallback in `ruby.rs`, through `setup_tool`. Verify with the node part of task 1.2 passing, plus the existing node, typescript and ruby tests
- [ ] 3.2 Route `mix` (`elixir.rs`) and `rebar3` (`erlang.rs`) through `setup_tool`. Verify with the elixir part of task 1.2 passing
- [ ] 3.3 Add `wrapper_for(project_root, posix, windows)` (design D3) and use it in `java.rs` and `kotlin.rs`, with a fallback to `setup_tool("mvn")` / `setup_tool("gradle")`. Verify with tests on Unix (absolute `mvnw`) and Windows (`gradlew.bat` chosen, and a POSIX-only `gradlew` falling back to `gradle.bat`), and with the java part of task 1.2 passing
- [ ] 3.4 Grep `src/modules` for any remaining `Command::new("<bare name>")` in setup paths and route each one through `setup_tool` or record why it is exempt. Verify with `rg 'Command::new\("[a-z]' src/modules`, reviewing the output
- [ ] 3.5 Update the README wherever it says `./gradlew` / `./mvnw` to mention `gradlew.bat` / `mvnw.cmd` on Windows. Verify with `rg 'gradlew|mvnw' README.md`

## 4. Integration

- [ ] 4.1 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` locally. Confirm the Windows CI job passes the tests that failed in group 1
- [ ] 4.2 Run `openspec validate resolve-windows-command-shims --strict` and confirm it reports no errors
- [ ] 4.3 Before landing, check whether `use-installed-toolchain-in-post-setup` has merged. If it has, rebase so that its search directories feed `setup_tool` instead of using a separate lookup. Verify that both changes' tests pass together
