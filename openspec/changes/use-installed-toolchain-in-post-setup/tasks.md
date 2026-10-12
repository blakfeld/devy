# Tasks

## 1. Reproduce the bug

- [ ] 1.1 Add `#[cfg(unix)]` failing tests that use `MockPackageManager { path_prepends_result: [<tmp>/bin] }` and fake executable scripts in `<tmp>/bin` that record their argv and `$PATH` to a marker file. Cover:
  - node `post_setup` with `package.json` runs `<tmp>/bin/npm install`;
  - elixir `post_setup` with `mix.exs` runs `<tmp>/bin/mix`;
  - the recorded `$PATH` of the npm child starts with `<tmp>/bin`;
  - python `post_setup` with `version: "3.12"` and no `.venv` invokes `<tmp>/bin/python3.12 -m venv`.

  Verify with `cargo test` that all of them fail on the current code, before any fix.
- [ ] 1.2 Add a failing python test: `.venv/pyvenv.cfg` with `version = 3.9.6`, plus a fake `python3.12` reporting `Python 3.12.1`. Verify it fails because no `-m venv --clear` call is recorded.

## 2. Toolchain directories and helper

- [ ] 2.1 Add `PackageManager::toolchain_dirs` (default empty). Nix returns the verified profile bin without warning. The mock returns `path_prepends_result`. Verify unit tests:
  - nix with a valid fake store link returns the bin;
  - nix with a plain directory returns empty and prints nothing;
  - the brew/apt/winget defaults return empty.
- [ ] 2.2 Make `fs_safe::executable_in` `pub(crate)`. Add `helpers::toolchain_command` and `run_toolchain_cmd` (D2). Verify unit tests:
  - a hit in the toolchain dir returns its absolute path;
  - a miss returns the bare name;
  - the child `PATH` is the toolchain dirs followed by the original PATH;
  - an empty toolchain list leaves PATH as is;
  - `run_toolchain_cmd` errors still name the program.

## 3. Switch the modules

- [ ] 3.1 Switch these call sites to the helper:
  - node (`<pm> install`, `npm install -g`);
  - typescript (`install`'s `npm install -g`, and the `post_setup` project install);
  - dart, zig, crystal, erlang, elixir and dotnet;
  - java (`mvn`, `gradle`, and PATH only for `./mvnw` and `./gradlew`);
  - kotlin.

  Verify the 1.1 node, elixir and PATH tests pass, and add one fake-tool test each for dart, dotnet and java `mvn`.
- [ ] 3.2 Check that no bare-name `Command::new` or `run_cmd` setup call remains in the listed modules. Verify with `grep -nE 'Command::new\("|run_cmd\("' src/modules/{node,typescript,dart,zig,crystal,erlang,elixir,dotnet,java,kotlin}.rs` returning only test code.

## 4. Python interpreter and venv rebuild

- [ ] 4.1 Implement the D3 candidate order (`python<M.m>` when versioned, then `python3`, then `python`; toolchain dirs before PATH). Verify:
  - the 1.1 python test passes;
  - a new test shows the profile's `python3` is preferred over a PATH `python3.12` when the profile lacks `python3.12`.
- [ ] 4.2 Implement the `pyvenv.cfg` version check (`version`, else `version_info`; major.minor compare) with a warning and `-m venv --clear`. Verify:
  - the 1.2 test passes;
  - a matching 3.12.4 vs 3.12.7 does not rebuild;
  - a `pyvenv.cfg` without a version does not rebuild;
  - after a rebuild the dependency install runs again (the stamp is gone).
- [ ] 4.3 Update the README's Python section to say which interpreter is used and when `.venv` is rebuilt. Verify the README states both.

## 5. npm global prefix under nix

- [ ] 5.1 Add `--prefix <root>/.devy/npm-global` to the global install args under nix (before `--`), in both node and typescript. Verify:
  - a fake-npm test under a nix-named mock records `--prefix <root>/.devy/npm-global`;
  - a brew-named mock records no `--prefix`;
  - `npm_global_install_args` validation tests still pass.
- [ ] 5.2 Under nix, node `post_setup` creates `.devy/npm-global/bin`. `backend_env_vars` adds `NPM_CONFIG_PREFIX` under nix, and `path_prepends` adds `.devy/npm-global/bin` when it exists. typescript does the same. Verify unit tests for each, plus a `project_env::resolve` test that the nix PATH order is the profile bin, then npm-global bin, then the other module entries.
- [ ] 5.3 Document the project-local npm prefix under nix in the README. Verify the README names `.devy/npm-global` and `NPM_CONFIG_PREFIX`.

## 6. Integration checks

- [ ] 6.1 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`, and verify all pass.
- [ ] 6.2 Manually, on macOS with the nix backend and no `node` or `python3.12` on PATH, run `devy up` in a project declaring `node` (with `package.json` and `global_packages: [cowsay]`) and `python` `version: "3.12"` (with `requirements.txt`). Verify:
  - it succeeds on the first run;
  - `.venv/pyvenv.cfg` records 3.12;
  - `cowsay` lands in `.devy/npm-global/bin`.
- [ ] 6.3 Get reviews from `code-reviewer` and `security-analyst`, with attention to the fake-profile path and `--clear`. Verify every finding is addressed.
