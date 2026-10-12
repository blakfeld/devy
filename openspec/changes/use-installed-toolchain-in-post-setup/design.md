# Design

## Context

- **How modules start tools today.** `post_setup` in python, node, typescript, dart, zig, crystal, erlang, elixir, dotnet, java and kotlin calls `Command::new("<bare name>")` or `run_cmd("npm", ..)`. python uses `which::which("python3")`. typescript's `install` also runs `run_cmd("npm", ..)` right after `pm.install_package`. None of these set the child's PATH.
- **Where the toolchain lives.** `PackageManager::path_prepends(project_root)` returns the verified `.devy/nix-profile/bin` under nix, warning and returning nothing when the profile fails `fs_safe::verified_nix_profile`. Other backends return nothing. Only `project_env::resolve` reads it, to write the shadowenv file in step 10 of `devy up`, after `post_setup` (step 8). Nothing in devy calls `std::env::set_var("PATH")`.
- **Existing pattern.** rust uses `fs_safe::which_outside_project`. ruby uses `which_with_project_profile`, which checks outside PATH first and the profile second. `post_setup` already receives `pm` (unused as `_pm` in most modules).
- **The virtualenv.** It is created only when `pyvenv.cfg` is missing, and its dependency stamp lives inside it (`<venv>/.devy_stamp`). CPython's `venv` writes `version = X.Y.Z` (≥3.11 may also write `version_info`). uv and virtualenv write `version_info`.

## Goals / Non-Goals

**Goals:**
- A setup step runs the toolchain devy installed for the project, on the first `devy up`.
- Subprocesses of setup steps see the same toolchain.
- A venv built by a different Python is rebuilt once.

**Non-Goals:**
- Changing devy's own process environment (`set_var`). It is process-global and unsafe with the threads tests and services use.
- rust, ruby, deno, bun and gcloud. They already resolve their tools.
- Windows `.cmd`/`.bat` shim spawning (`resolve-windows-command-shims`), and `JAVA_HOME` or keg-only brew bins (`derive-java-home-from-installed-jdk`). Both build on the helper added here: the shim change wraps the resolved path, and the Java change can add keg-only dirs to the toolchain directories.
- Tightening the fallback to ignore project-local PATH entries. See Open Questions.

## Decisions

### D1. A quiet `toolchain_dirs` on `PackageManager`
- **Method:** add `fn toolchain_dirs(&self, project_root: &Path) -> Vec<PathBuf>`, defaulting to empty.
- **Nix:** returns `verified_nix_profile(..).ok().flatten().map(|_| profile_bin)` without warning, since `project_env::resolve` already warns once per run.
- **Mock:** returns `path_prepends_result`, so tests can point it at a temp directory of fake tools.
- **Alternative rejected:** calling `pm.path_prepends` from each module. That repeats the "does not link into /nix/store" warning once per dependency.

### D2. One helper for resolve + child PATH
In `modules/helpers.rs`, add `toolchain_command(pm, project_root, name) -> Command`. It:
1. looks `name` up in each `toolchain_dirs` entry with `fs_safe::executable_in` (made `pub(crate)`; it uses `which::which_in`, so PATHEXT is honoured on Windows), and uses the absolute path when found, else the bare `name`;
2. sets `PATH` to `toolchain_dirs` joined ahead of `std::env::var_os("PATH")` with `std::env::join_paths`.

`run_cmd` gets a sibling, `run_toolchain_cmd(pm, root, name, args)`, keeping today's error messages. Every listed call site switches to these. `./mvnw` and `./gradlew` use `Command::new` with only the PATH step applied (they are paths, not names).
- **Why not only set the child PATH and rely on lookup:** Rust's std resolves a bare program against the child's PATH on Unix, but Windows behaviour differs by version. An explicit absolute path also makes error messages and tests unambiguous.
- **Why toolchain first, not `which_with_project_profile`'s order:** that function prefers the user's PATH, which is exactly the bug for setup steps.

### D3. Python interpreter choice and rebuild
- **Candidates:** with `dep.version`, `python<major>.<minor>` comes first, parsed with the same major.minor split as `nix_versioned_attr`. Then `python3`, then `python`.
- **Lookup order:** each candidate is tried in the toolchain dirs before any candidate on PATH. So the profile's `python3` beats a system `python3.12` when the profile has no `python3.12`.
- **Versions compared:** the interpreter's `--version` output against `pyvenv.cfg` `version`, else `version_info`, on major.minor.
- **Rebuild:** on a mismatch, warn and run `<python> -m venv --clear <venv>`. Never `remove_dir_all`: `--clear` is CPython's own, scoped to the venv dir. It runs only after the up-phase filesystem-safety check has verified the venv dir.
- **Unreadable version:** an unreadable or missing version leaves the venv untouched, so venvs made by other tools are not destroyed.
- **Alternative rejected:** a new version stamp file. `pyvenv.cfg` already records the version, and a separate stamp would disagree with venvs created before this change.

### D4. Project-local npm prefix under nix
- **Install:** under `pm.name() == "nix"`, node and typescript pass `--prefix <root>/.devy/npm-global` (inserted before `--`, so the validated package list is unchanged).
- **Directory:** node's `post_setup` creates `<root>/.devy/npm-global/bin` with `fs_safe` directory creation under the already-verified `.devy/`.
- **Environment:** `backend_env_vars` adds `NPM_CONFIG_PREFIX` under nix. `path_prepends` adds `<root>/.devy/npm-global/bin` when the directory exists, because `Module::path_prepends` has no `pm` argument. The directory only exists under nix, and environment composition runs after `post_setup`, so it is on PATH in the same run.
- **Alternatives rejected:**
  - `~/.npm-global`, which is shared across projects and conflicts between Node majors.
  - Leaving `-g` on system npm, which brings the original bug back.
  - Adding `pm` to `Module::path_prepends`, which touches every module for one caller.

## Risks / Trade-offs

- **A project that relied on a system tool shadowing the profile's.** Under nix, the profile now wins. → This is the intended behaviour; noted in the README.
- **Rebuilding a venv deletes packages installed by hand.** → Only on a Python major.minor change, where those packages are already broken (compiled for the old ABI). It is warned and is a one-off.
- **`NPM_CONFIG_PREFIX` changes the user's `npm -g` inside the project shell.** → Without it, `npm -g` from the profile's npm fails with EACCES on `/nix/store`. Scoped to the shadowenv-activated project.
- **Tests that fake tools need executable scripts.** → Gate them `#[cfg(unix)]`. The resolution and PATH-joining logic gets platform-neutral unit tests with temp files.

## Migration Plan

No user action. On the first `devy up` after upgrading:
- Existing nix projects may see `.venv` rebuilt once, if it was created by a system Python.
- They gain `.devy/npm-global`.

Rollback is reverting the change. A rebuilt venv keeps working.

## Open Questions

- **Should the fallback skip project-local PATH entries?** Today a bare name resolves against devy's full PATH. Switching the fallback to `which_outside_project` would align setup tools with filesystem-safety's "Executables resolved outside the project". That is a separate hardening change. This change keeps today's fallback so behaviour on brew, apt and WinGet is unchanged.
