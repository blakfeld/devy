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
- **brew:** `<prefix>/opt/<brew_formula_name(pkg)>/bin` when it is a directory and not project-local (an `opt` link into the project, or a prefix above the project). `<prefix>` is the parent of the `bin` directory that holds `brew_bin()` (not canonicalized, since on Intel `/usr/local/bin/brew` links into `/usr/local/Homebrew`); when `brew` is a link, the links are followed one hop at a time (at most 40) and the first prefix with `opt` along the chain is used, so a `~/bin/brew` link to `/opt/homebrew/bin/brew` or to Intel's `/usr/local/bin/brew` finds the right prefix even if `~/opt` exists; failing that, the real path's prefix when it has `opt` and differs from the direct one. Intel's `/usr/local/Homebrew` has no `opt`, so `/usr/local` is kept. Every candidate prefix (a hop, the real path's, or the direct one) is used only when the prefix directory and its `opt`, both followed through links, are directories owned by a trusted owner and not world-writable, and, when `opt` is a link, the link itself has a trusted owner. So a hop through a shared directory such as `/tmp` can't let another local user plant `opt/<formula>/bin`, or an `opt` link to swap later. Trusted owners are the current user, root and the owner of the real `brew` executable devy runs, which devy already trusts (on a shared Mac, the account that installed Homebrew). A prefix found through a hop is canonicalized, so PATH and JAVA_HOME have no `..`. Group-writable is accepted only when the group is `wheel` (gid 0) or `admin` (gid 80, which Intel Homebrew uses for `/usr/local`): every macOS account is in `staff`, so a `staff`-writable directory would let other local users plant formulae. On other platforms only the directory check applies. With no qualifying candidate there is no prefix and no `opt` entries. `brew_bin_with` already refuses a project-local `HOMEBREW_PREFIX`. The raw prefix is found once per `Homebrew` value (`OnceLock`), since `resolve` asks per dependency, and `fs_safe::is_project_local` is checked against it on every call.
- **nix:** the profile bin, only when `fs_safe::verified_nix_profile` passes. It duplicates the backend entry and is removed by dedupe (D4); it exists so Java can use one code path. `JAVA_HOME` under nix therefore goes through the project-tree `.devy/nix-profile` symlink (`.devy/nix-profile/lib/openjdk`), which is re-verified only on the next `resolve` (`up`, `exec`, `check`, `status`). That is the same exposure as the existing profile-bin PATH entry: between runs, the activated shell trusts whatever the link points to.
- **apt / winget:** `None`.
- **Alternative: `brew --prefix -- <formula>`** per dependency. Correct but spawns `brew` (Ruby startup) once per dependency on every `devy exec`. The opt layout is stable Homebrew API (`brew --prefix <formula>` itself returns `<prefix>/opt/<formula>`), so a filesystem check is enough.
- **Alternative: only keg-only formulae** (from `brew info --json=v2`). Needs a `brew` call per formula, and prepending linked formulae is harmless; it also makes a pin win over a different linked version.

### D2. Modules expose the package they install
Add `Module::backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency>`, default `None`. Every module whose `install` calls `pm.install_package(...)` implements it with the exact value it installs, and its `install` / `is_installed` are rewritten to call it, so the two cannot drift. Script- or toolchain-installed modules (rust, deno, bun, gcloud on nix/apt, ruby via rbenv) keep `None`. `GenericModule` returns `pm_dep(dep, &dep.name)`.
- **Alternative: let brew record what it installed during `up`.** Not available to `exec`/`check`, which never install.

### D3. Java derives its home from the installed binary
Replace `env_vars`/`path_prepends` in `JavaModule` with `backend_env_vars` (JAVA_HOME) and a new `Module::backend_path_prepends(dep, pm, project_root) -> Vec<String>` (default: empty), both calling `java_home(pm, dep)`:
- brew / nix: `pm.package_bin_dir(&backend_package)` → `<bin>/java` → `canonicalize` → `ancestors().nth(2)` gives the real home. It is only used to learn where the home sits inside its package, then rebased onto a path that survives upgrades: under brew onto `<bin>/..` = `<prefix>/opt/<formula>` (the real home relative to the canonical keg, e.g. `/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home` rather than the versioned Cellar path); under nix onto `<root>/.devy/nix-profile` (the real home relative to its store object, e.g. `.devy/nix-profile/lib/openjdk`). The rebased home must contain `bin/java` and canonicalize to the real home, so it is the JDK just verified; otherwise nix uses the real store home and brew gives `None`. Under nix, a store object that is itself the home (empty relative path) always uses the real store home, never the whole merged profile. Under nix the real home must be inside the store and not the store itself (as `verified_nix_profile_in`; the store root is a parameter so tests can accept a fake one); when the profile lacks the rebased path, the real store home is used. Under brew a home that is project-local is refused. Otherwise `None`, with no fallback (a wrong JDK is worse than none: it silently builds with the wrong version).
- apt / winget: the existing `detect_java_home()` branches, unchanged.
- Canonicalizing the `java` link handles every layout without hard-coding: Homebrew macOS (`libexec/openjdk.jdk/Contents/Home`), Linuxbrew (`libexec`), nixpkgs Linux (`lib/openjdk`) and nixpkgs Darwin (Zulu's `Contents/Home`).
- **Alternative: hard-code `<opt>/libexec/openjdk.jdk/Contents/Home`.** Wrong on Linuxbrew and for nix.
- `PortMode` is irrelevant to Java; `backend_env_vars` writes nothing either way.
- `java_home` runs twice per Java dependency per `resolve` (once for `JAVA_HOME`, once for the PATH entry). It is a directory check and one `canonicalize` on the cached brew prefix, so it is not cached: caching it would need state in the stateless `Module` statics or a combined trait hook.

### D4. `project_env::resolve` builds the one PATH list, with dedupe
Order: `pm.path_prepends(root)`, then each module's `path_prepends` and `backend_path_prepends`, then for each dependency `pm.package_bin_dir(module.backend_package(pm, dep))` (see D5); then drop later duplicates. `ProjectEnv` records how many leading entries are the package manager's own (`backend_path_count`). `check.rs` switches to `project_env::resolve(..., PortMode::ReadOnly)` for its PATH list, with lock pins applied as `up` does (`apply_lock_from_source`), so it stops reporting a fresh `devy up` as out of date. `check` and the `status` text table compare the written file against that list without the backend-wide entries (`ProjectEnv::compared_path_prepends`), as `check` did before this change: otherwise a nix project with no dependencies would fail `devy check`, `devy doctor` and `devy up --dry-run` until its first `devy up`. Under nix each package's bin dir is the profile `bin`, which dedupe folds into the backend entry, so it is excluded too. `status --json` `path` still lists every entry with its `written` flag.

### D5. Ordering of brew entries vs. module entries
Per-package entries sit after module entries (user decision, 2026-10-08). Before them, brew's `opt/python/bin` shadowed the project's `.venv/bin`, so `pip install` hit Homebrew's externally managed Python (PEP 668). The backend's own entries (the nix profile `bin`) still come first. Java's `$JAVA_HOME/bin`, a module entry, therefore precedes `opt/openjdk/bin`, which holds the same binaries.

## Risks / Trade-offs

- [Every brew project now writes `.shadowenv.d/500_devy.lisp`] → Called out as BREAKING in the proposal; shadowenv is already required for env vars, and the file is regenerated on each `up`.
- [Long PATH with many brew dependencies] → One entry per formula, deduped; acceptable versus missing tools.
- [`backend_package` refactor touches ~30 modules] → Mechanical; each module's existing install argv tests guard against changing what gets installed.
- [`opt/<formula>` absent until first `up`] → `check` before `up` simply shows no entry and no `JAVA_HOME`, which is accurate.
- [A malicious `HOMEBREW_PREFIX` from the activated environment] → Already refused when project-local, as are a formula bin dir and a JAVA_HOME inside the project.
- [`HOMEBREW_PREFIX` set from devy.yml `environment`] → Deferred by user 2026-10-08: treat `HOMEBREW_PREFIX` like `PATH` in the executable-entry review listing (`src/config_diff.rs`); separate change.

## Migration Plan

No data migration. Users get the new entries on their next `devy up`; `devy check` reports the environment as stale until then. Rollback is reverting the change; the next `up` rewrites the file.

## Open Questions

- Should `sbin` (for formulae such as `nginx` or `rabbitmq`, which ship admin tools there) also be prepended? Deferred; adding it later only adds entries.
- winget: derive `JAVA_HOME` from the `Microsoft.OpenJDK.<major>` install location instead of `$JAVA_HOME`. Deferred to a separate change.
- `HOMEBREW_PREFIX` in devy.yml `environment`: Deferred by user 2026-10-08: treat `HOMEBREW_PREFIX` like `PATH` in the executable-entry review listing (`src/config_diff.rs`); separate change.
