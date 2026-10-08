# Proposal

## Why

On Windows, Rust's `Command::new("npm")` looks only for `npm.exe`: Rust's std appends `.exe` to a bare name and does not consult `PATHEXT`. The tools devy runs in `post_setup` are `.cmd`/`.bat` shims under WinGet (`npm.cmd`, `pnpm.cmd`, `yarn.cmd`, `bundle.bat`, `mvn.cmd`, `gradle.bat`, `mix.bat`, `rebar3.cmd`), so they all fail with "program not found". Also, `./gradlew` and `./mvnw` are POSIX shell scripts that Windows cannot run. As a result, `devy up` fails during project setup for every Node, TypeScript, Ruby, Java, Kotlin, Elixir and Erlang project on Windows.

## What Changes

- Module setup commands are resolved to a full path before they run. The lookup honours `PATHEXT` on Windows, so `npm` resolves to `...\npm.cmd`, and it skips relative and project-local PATH entries, as `which_outside_project` already does. This covers both the per-module `Command::new(<bare name>)` calls and the shared `run_cmd` helper (`npm install -g`, gcloud, rustup).
- When a tool cannot be found, devy fails with a clear `` `<tool>` not found on PATH `` error instead of the raw OS "program not found" error.
- On Windows, the Java and Kotlin modules use the project's `mvnw.cmd` / `gradlew.bat` wrapper. When only the POSIX wrapper exists, they fall back to `mvn` / `gradle` from PATH. On macOS and Linux they keep using `mvnw` / `gradlew`.
- No new configuration is added. Behaviour on macOS and Linux is unchanged, except that setup tools are no longer taken from relative or project-local PATH entries.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `dependency-modules`: adds a requirement that setup tools are resolved to a full path, including Windows command shims, before they run. Also changes the Java and Kotlin wrapper selection on Windows.

## Impact

- Code: `src/modules/helpers.rs` (`run_cmd` and a new shared resolver), `src/modules/node.rs`, `typescript.rs`, `ruby.rs`, `java.rs`, `kotlin.rs`, `elixir.rs` and `erlang.rs`. `src/fs_safe.rs` is reused as is (`which_outside_project`).
- Composes with the sibling change `use-installed-toolchain-in-post-setup`. That change adds the installed toolchain's directories to the same resolver's search path; this change owns how names are resolved (PATHEXT, wrappers).
- Tests: `cfg(windows)` unit tests that run on the existing `windows-latest` CI job.
- No new dependencies: the `which` crate is already used and honours `PATHEXT`.
