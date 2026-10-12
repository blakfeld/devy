# Tasks

## 1. Reproduce the bug

- [ ] 1.1 Extend `MockPackageManager` in `src/package_manager/mod.rs` to record the full `Dependency` (name and version) passed to `install_package` and `is_package_installed`. Verify that existing tests still pass with `cargo test`
- [ ] 1.2 Add failing unit tests in `src/modules/dotnet.rs`. With a mock named `apt` and `version: 8`, `install` must pass `dotnet-sdk-8.0` with version `None`, and `is_installed` must query it with version `None`. With a mock named `winget` and `version: 8.0.100`, the package must be `Microsoft.DotNet.SDK.8` with version `None`. Also add an `apt::install_args` check that the resulting args contain `dotnet-sdk-8.0` and no `=`. Verify that they fail on current `main`
- [ ] 1.3 Add failing unit tests in `src/modules/java.rs`. apt with `version: 17` must install `openjdk-17-jdk` with no version. apt without a version must install `default-jdk`. winget with `version: 17` must install `Microsoft.OpenJDK.17` with no version. Verify that they fail on current `main`
- [ ] 1.4 Add failing unit tests in `src/modules/ruby.rs`. winget without a version must install `RubyInstallerTeam.Ruby.3.3` with no version. winget with `version: 3.4.1` must install `RubyInstallerTeam.Ruby.3.4` with no version. winget with `version: 3` must error with `ruby: WinGet needs a major.minor Ruby version such as 3.3, got '3'` and record no install. Verify that they fail on current `main`
- [ ] 1.5 Add guard tests that must pass both before and after the fix. dotnet and java on a mock named `brew` with `version: 8` / `17` keep the version. `pm_dep_replaces_name_preserves_other_fields` in `src/modules/mod.rs` is unchanged. Verify with `cargo test`

## 2. Fix

- [ ] 2.1 Add the unversioned variant of `pm_dep` in `src/modules/helpers.rs` (design D1), returning `version: None` and `version_from_lock: false`, and give it a unit test in `helpers.rs` tests. Verify with `cargo test helpers`
- [ ] 2.2 Use it in `src/modules/dotnet.rs` for apt and winget in both `is_installed` and `install`. Verify that the 1.2 tests pass
- [ ] 2.3 In `src/modules/java.rs`, map apt to `openjdk-<major>-jdk` when a version is pinned (else `default-jdk`), and use the unversioned dependency for apt and winget. Verify that the 1.3 and 1.5 tests pass
- [ ] 2.4 In `src/modules/ruby.rs`, make `winget_package_id` return `Result<String>` and build `RubyInstallerTeam.Ruby.<major>.<minor>` from the pinned version or `DEFAULT_RUBY_VERSION`, failing with the D4 message for a major-only version. Use the unversioned dependency in `is_installed` and `install`. Verify that the 1.4 tests pass

## 3. Docs and checks

- [ ] 3.1 Update the README package-name table: java apt shows `default-jdk` / `openjdk-<N>-jdk` when pinned, and ruby WinGet shows `RubyInstallerTeam.Ruby.3.3`. Add a note that on apt and WinGet the dotnet, java and ruby version selects the release line only. Verify with `rg 'RubyInstallerTeam.Ruby.3\b' README.md`, which must find nothing
- [ ] 3.2 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`, and verify that all pass
- [ ] 3.3 Run the `code-reviewer` and `security-analyst` subagents on the diff, then address or rebut every finding
