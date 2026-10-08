# Design

## Context

Module `post_setup` steps start tools by bare name: `Command::new(pm_cmd)` in `node.rs` / `typescript.rs`, `Command::new("mix")` in `elixir.rs`, `Command::new("rebar3")` in `erlang.rs`, the `"bundle"` fallback in `ruby.rs`, and `"mvn"` / `"gradle"` in `java.rs` / `kotlin.rs`. `run_cmd("npm", …)` in `helpers.rs` works the same way. On Windows, Rust std searches PATH for the name and appends `.exe` only when the name has no extension ("Files with other extensions must include the extension, otherwise they will not be found", `std::process::Command::new`). It never consults `PATHEXT`, so `npm.cmd` and its siblings are never found. Java and Kotlin also pick `"./mvnw"` / `"./gradlew"`, POSIX scripts that `CreateProcessW` cannot run.

devy already has PATH lookups that honour `PATHEXT`: `fs_safe::which_outside_project` (through `which::which_in`, which applies `PATHEXT` on Windows) and `which_with_project_profile`. Since Rust 1.77.2 (CVE-2024-24576, "BatBadBut"), std detects a `.bat` / `.cmd` program, runs it through `cmd.exe` with batch-safe escaping, and returns `ErrorKind::InvalidInput` for any argument it cannot escape safely. The crate's edition 2024 requires Rust 1.85 or later, so that mitigation is always present.

The sibling change `use-installed-toolchain-in-post-setup` makes setup tools resolve against the installed toolchain's directories (for example the nix profile `bin`) as well as devy's PATH.

## Goals / Non-Goals

**Goals:**
- Every setup tool devy starts by name is resolved to an absolute path, with `PATHEXT`, before it is spawned.
- Java and Kotlin pick a wrapper that can run on the current OS.
- Provide one resolver that the sibling change extends instead of duplicating.

**Non-Goals:**
- Changing which directories are searched (the sibling change's scope).
- Commands run through the shell (`install_cmd`, project commands, hooks), which already go through `cmd /C` on Windows.
- Package-manager backends (`winget`, `brew`, `nix`), which are `.exe` binaries or are already resolved.

## Decisions

### D1. One resolver, `helpers::setup_tool(name) -> Result<PathBuf>`
A single function in `src/modules/helpers.rs` returns the absolute path for a tool name. It calls `fs_safe::which_with_project_profile(name)`, which already skips relative and project-local entries, honours `PATHEXT`, and accepts a verified project nix profile. If nothing is found, it fails with `` `<name>` not found on PATH ``. Every bare-name `Command::new` in module setup code, and `run_cmd`, goes through it. `run_cmd` resolves names without a path separator and passes absolute paths, such as the rbenv shim, gcloud or rustup paths, through unchanged.
- *Alternative*: hand-roll `PATHEXT` handling or append `.cmd` on `cfg(windows)`. Rejected: this duplicates `which`, and a hard-coded extension is wrong for `bundle.bat` and `gradle.bat`.
- *Alternative*: spawn through `cmd /C <name>`. Rejected: it would bypass std's BatBadBut escaping and reopen command injection through `global_packages`.
- *Composition*: the sibling change adds its toolchain directories inside `setup_tool`, for example `setup_tool_in(name, extra_dirs)`. Call sites stay the same. Whichever change lands second rebases onto the shared function.

### D2. Resolve on every platform, not only Windows
On macOS and Linux, resolving first gives the same clear error and the same project-local exclusion. Resolving only under `cfg(windows)` would leave two code paths, and the Windows one would get no test coverage on Unix CI. The cost is that a tool placed only in a relative or project-local PATH entry is no longer used. That matches the hardening already applied to `rbenv` and `shadowenv`.

### D3. Wrapper selection is per OS and uses absolute paths
`wrapper_for(project_root, posix, windows)` returns `project_root.join("gradlew.bat")` / `project_root.join("mvnw.cmd")` on Windows and `project_root.join("gradlew")` / `project_root.join("mvnw")` elsewhere, but only when that file exists. Otherwise the code falls back to `setup_tool("gradle")` / `setup_tool("mvn")`. The wrapper is project-local on purpose, as it is today, and is not searched for on PATH. Messages keep the short display name (`gradlew.bat dependencies`).

### D4. Rely on std for batch-file arguments
Arguments are passed with `.arg()` / `.args()`, never `raw_arg`. All setup arguments are fixed literals except npm `global_packages`, which `validate::list_entry` already restricts. When std refuses an argument with `InvalidInput`, the step fails with its existing ``Failed to run `…` `` context. It is never retried through a shell.

## Risks / Trade-offs

- [A tool exists only as a project-local or relative PATH entry, for example `node_modules/.bin` on PATH] → The step now fails with `not found on PATH`. This is intended, and it is called out in the proposal.
- [Unix CI cannot exercise `PATHEXT`] → `cfg(windows)` unit tests create `npm.cmd` / `gradlew.bat` fixtures in temp dirs and run on the existing `windows-latest` CI job.
- [Merge conflict with the sibling change on the same call sites] → Both changes route through the D1 resolver. The second to land only changes the resolver's search directories.
- [`npm.cmd` with `^` or `>` in package specs] → std's batch escaping quotes them. A Windows test asserts that the arguments arrive unchanged, using a `.cmd` fixture that echoes `%*` to a file.

## Open Questions

- The Ruby module uses the `$RBENV_ROOT/shims/bundle` shortcut only when rbenv exists, which never happens on WinGet. Should that shortcut move into the resolver as an extra search directory, in the sibling change? This change leaves it in place.
