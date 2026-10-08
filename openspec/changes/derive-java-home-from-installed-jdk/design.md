# Design

## Context

See proposal.md for the bug. The relevant shape of the code today:

- `JavaModule::env_vars` and `JavaModule::path_prepends` (`src/modules/java.rs`) both call `detect_java_home()`, which takes no dependency and no package manager. It asks `/usr/libexec/java_home` on macOS, `/usr/lib/jvm/default-java` or `which java` on Linux, and `$JAVA_HOME` on Windows. Verified locally: `brew info --json=v2 openjdk node@22 postgresql@16` reports `keg_only: true` for all three (`shadowed_by_macos`, `versioned_formula`), and `/usr/libexec/java_home` exits 1 ("Unable to locate a Java Runtime") on a Mac without a registered JDK.
- `Module::path_prepends(dep, project_root)` has no access to the package manager. `Module::backend_env_vars(dep, pm, project_root, mode)` does, and `project_env::resolve` already calls it.
- `PackageManager::path_prepends(project_root)` is per backend, not per package; only nix overrides it (`.devy/nix-profile/bin`).
- The formula devy installs is computed per module (`pkg_name`/`pm_dep` → `brew_formula_name`, which appends `@<version>` only for a devy.yml pin, never a lock value). There is no trait method that returns "the package this module installs".
- `devy check` (`src/commands/check.rs`) builds its PATH list from module `path_prepends` only, not from `project_env::resolve`.
- `project_env::resolve` runs on every `devy exec`, so it must stay cheap.

## Goals / Non-Goals

**Goals:**
- `JAVA_HOME` and the Java PATH entry always describe the JDK devy installed for the dependency under brew and nix, honoring the pinned version.
- Every Homebrew-installed formula, keg-only or not, is reachable from the activated environment.
- One PATH list shared by `up`, `exec`, `check` and `status`.

**Non-Goals:**
- Making devy's own process use these entries during `post_setup` (mvn, gradle, npm…). That is `use-installed-toolchain-in-post-setup`, which consumes the list this change produces.
- Fixing winget's JDK location (`Microsoft.OpenJDK.<major>` installs under `C:\Program Files\Microsoft\jdk-*`) or apt's version handling (`default-jdk=21`). Both keep today's detection.
- `sbin` directories or `MANPATH`, `PKG_CONFIG_PATH`, `LDFLAGS` for keg-only formulae.

## Decisions

### D1. A per-package bin directory on the package manager
Add `PackageManager::package_bin_dir(&self, pkg: &Dependency) -> Option<PathBuf>` (default `None`), where `pkg` is the backend package dependency a module installs (the same value it passes to `install_package`).
- **brew:** `<prefix>/opt/<brew_formula_name(pkg)>/bin` when it is a directory. `<prefix>` is the parent of the `bin` directory that holds `brew_bin()` (not canonicalized, since on Intel `/usr/local/bin/brew` links into `/usr/local/Homebrew`). `brew_bin_with` already refuses a project-local `HOMEBREW_PREFIX`; the prefix is checked again with `fs_safe::is_project_local`.
- **nix:** the profile bin, only when `fs_safe::verified_nix_profile` passes. It duplicates the backend entry and is removed by dedupe (D4); it exists so Java can use one code path.
- **apt / winget:** `None`.
- **Alternative: `brew --prefix -- <formula>`** per dependency. Correct but spawns `brew` (Ruby startup) once per dependency on every `devy exec`. The opt layout is stable Homebrew API (`brew --prefix <formula>` itself returns `<prefix>/opt/<formula>`), so a filesystem check is enough.
- **Alternative: only keg-only formulae** (from `brew info --json=v2`). Needs a `brew` call per formula, and prepending linked formulae is harmless; it also makes a pin win over a different linked version.

### D2. Modules expose the package they install
Add `Module::backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency>`, default `None`. Every module whose `install` calls `pm.install_package(...)` implements it with the exact value it installs, and its `install` / `is_installed` are rewritten to call it, so the two cannot drift. Script- or toolchain-installed modules (rust, deno, bun, gcloud on nix/apt, ruby via rbenv) keep `None`. `GenericModule` returns `pm_dep(dep, &dep.name)`.
- **Alternative: let brew record what it installed during `up`.** Not available to `exec`/`check`, which never install.

### D3. Java derives its home from the installed binary
Replace `env_vars`/`path_prepends` in `JavaModule` with `backend_env_vars` (JAVA_HOME) and a new `Module::backend_path_prepends(dep, pm, project_root) -> Vec<String>` (default: empty), both calling `java_home(pm, dep)`:
- brew / nix: `pm.package_bin_dir(&backend_package)` → `<bin>/java` → `canonicalize` → `ancestors().nth(2)`; require `<home>/bin/java` to exist; for nix also require the home to start with `/nix/store/`. Otherwise `None`, with no fallback (a wrong JDK is worse than none: it silently builds with the wrong version).
- apt / winget: the existing `detect_java_home()` branches, unchanged.
- Canonicalizing the `java` link handles every layout without hard-coding: Homebrew macOS (`libexec/openjdk.jdk/Contents/Home`), Linuxbrew (`libexec`), nixpkgs Linux (`lib/openjdk`) and nixpkgs Darwin (Zulu's `Contents/Home`).
- **Alternative: hard-code `<opt>/libexec/openjdk.jdk/Contents/Home`.** Wrong on Linuxbrew and for nix.
- `PortMode` is irrelevant to Java; `backend_env_vars` writes nothing either way.

### D4. `project_env::resolve` builds the one PATH list, with dedupe
Order: `pm.path_prepends(root)`, then for each dependency `pm.package_bin_dir(module.backend_package(pm, dep))`, then each module's `path_prepends` and `backend_path_prepends`; then drop later duplicates. `check.rs` switches to `project_env::resolve(..., PortMode::ReadOnly)` for its PATH list, so it stops reporting a fresh `devy up` as out of date.

### D5. Ordering of brew entries vs. module entries
Per-package entries sit before module entries, matching the existing "package manager first" rule. Java's `$JAVA_HOME/bin` therefore follows `opt/openjdk/bin`, which holds the same binaries.

## Risks / Trade-offs

- [Every brew project now writes `.shadowenv.d/500_devy.lisp`] → Called out as BREAKING in the proposal; shadowenv is already required for env vars, and the file is regenerated on each `up`.
- [Long PATH with many brew dependencies] → One entry per formula, deduped; acceptable versus missing tools.
- [`backend_package` refactor touches ~30 modules] → Mechanical; each module's existing install argv tests guard against changing what gets installed.
- [`opt/<formula>` absent until first `up`] → `check` before `up` simply shows no entry and no `JAVA_HOME`, which is accurate.
- [A malicious `HOMEBREW_PREFIX` from the activated environment] → Already refused when project-local; the derived JAVA_HOME is a canonical path outside the project.

## Migration Plan

No data migration. Users get the new entries on their next `devy up`; `devy check` reports the environment as stale until then. Rollback is reverting the change; the next `up` rewrites the file.

## Open Questions

- Should `sbin` (for formulae such as `nginx` or `rabbitmq`, which ship admin tools there) also be prepended? Deferred; adding it later only adds entries.
- winget: derive `JAVA_HOME` from the `Microsoft.OpenJDK.<major>` install location instead of `$JAVA_HOME`. Deferred to a separate change.
