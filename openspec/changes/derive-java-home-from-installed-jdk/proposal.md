# Proposal

## Why

The java module sets `JAVA_HOME` and its PATH entry from whatever JDK the machine already advertises, ignoring both the backend devy installed through and the requested `version`. On macOS it asks `/usr/libexec/java_home`, which cannot see Homebrew's `openjdk`: that formula is keg-only (shadowed by macOS) and never registers with the system. On a Mac with no other JDK, devy therefore sets no `JAVA_HOME` and adds nothing to PATH, and `java` is the macOS stub ("Unable to locate a Java Runtime"). With another JDK installed, `JAVA_HOME` points at that one instead of `openjdk@21`. Under nix it uses `/usr/lib/jvm/default-java` or the `java` on PATH, never the JDK in the project's nix profile. The same keg-only gap hits every version-pinned Homebrew formula: `node@22`, `postgresql@16` and `mysql@8.4` are never linked into `$(brew --prefix)/bin`, and neither the modules nor the Homebrew backend put `$(brew --prefix)/opt/<formula>/bin` on PATH, so the pinned tools are not found once the environment is active.

## What Changes

- **Java follows the installed JDK.** Under brew and nix, `JAVA_HOME` is derived from the JDK devy installed for the dependency: the real home behind `<brew prefix>/opt/<formula>/bin/java` (for example `opt/openjdk@21` → `.../libexec/openjdk.jdk/Contents/Home`), or behind `.devy/nix-profile/bin/java` in the nix store. `$JAVA_HOME/bin` is prepended to PATH. When that JDK is not installed, nothing is contributed; devy never falls back to `/usr/libexec/java_home`, `/usr/lib/jvm/default-java` or `java` on PATH on those backends, so a different JDK is never presented as the requested one.
- apt and winget keep today's detection (`/usr/lib/jvm/default-java` or `java` on PATH; an existing `$JAVA_HOME`), since apt installs `default-jdk` and winget's JDK location is not covered here.
- **Homebrew puts formula `bin` directories on PATH.** For each dependency installed through Homebrew, devy prepends `<brew prefix>/opt/<formula>/bin` when it exists, where `<formula>` is the formula devy installs (including `name@version` pins). This makes keg-only and versioned formulae (`node@22`, `postgresql@16`, `mysql@8.4`, `openjdk`) reachable, and makes a pinned formula win over a different linked version. Duplicate PATH entries are removed, keeping the first.
- **BREAKING (environment content):** brew projects now get PATH entries in `.shadowenv.d/500_devy.lisp`, so a brew project that wrote no environment file before now writes one. Existing projects see new PATH entries and a possibly different `JAVA_HOME` after their next `devy up`.
- `devy check` and `devy status` compare against the same PATH entries `devy up` writes.
- devy's own process using these entries while running `post_setup` (mvn, gradle, npm…) is the sibling change `use-installed-toolchain-in-post-setup`; this change only supplies the correct entries and `JAVA_HOME`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `dependency-modules`: the Java requirement derives `JAVA_HOME` and its PATH entry from the JDK installed by the backend and the requested version, with no system-JDK fallback under brew and nix.
- `package-managers`: new requirement that the Homebrew backend contributes each installed formula's `opt/<formula>/bin` directory to PATH.
- `shell-environment`: PATH ordering and stale-environment rules no longer say brew contributes no PATH entries; duplicates are removed.

## Impact

- `src/modules/java.rs` (`detect_java_home`, `env_vars`, `path_prepends`), `src/modules/mod.rs` (Module trait: a backend-aware PATH hook and an accessor for the package a module installs), the modules that install through the package manager (to expose that package), `src/package_manager/mod.rs` and `brew.rs` / `nix.rs` (per-package bin directory), `src/project_env.rs` (collect and dedupe), `src/commands/check.rs` (use the same PATH list).
- No new dependencies. No new `brew` subprocess on the `devy exec` path: the opt directory is found on disk from the brew prefix devy already resolves.
- Tests in `src/modules/java.rs`, `src/package_manager/brew.rs`, `src/project_env.rs` and `tests/cli.rs`.
