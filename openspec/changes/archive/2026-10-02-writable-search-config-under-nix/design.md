# Design

## Context

- **Current launch.** `search_server_launch` passes `-E http.host/http.port/path.data/path.logs`. Config still resolves to `$ES_HOME/config`.
- **nixpkgs packaging** (from the packages' install phases):
  - both do `cp -R bin config lib modules plugins … $out` and `wrapProgram` with only `JAVA_HOME` and `PATH`;
  - no config path is set;
  - `bin/<server>` invokes `$out/bin/<server>-keystore`, which creates `config/<server>.keystore` when it's missing.
- **What the launch spec can see.** `LaunchSpec.exec_package` already lets the nix PM resolve a package's own store `bin/` from the profile. The module itself can't see store paths.

## Goals / Non-Goals

**Goals:**
- Both servers start under nix with no manual steps.
- Users can tune the copied config.

**Non-Goals:**
- **Upgrading a seeded config when the package's major version changes.** It's documented; users delete the directory to reseed.
- **TLS or auth for local dev.**
- **brew/apt behavior.**

## Decisions

### D1. Seed once from the package, owned by the PM
- **Spec field:** add `seed_dirs: Vec<SeedDir>` to `LaunchSpec`, where `SeedDir { from_package: "config", to: <data>/config }`. Both modules also set `exec_package` to their nix attribute.
- **PM step:** after resolving the package's store `bin/`, the nix PM takes its parent as the package root. Then, before init and launch, for each seed whose `to` doesn't exist, it copies `<root>/<from_package>` recursively and adds owner-write to everything copied (store files are 0444).
- **Missing source:** if the package root can't be resolved, fail with `Failed to prepare <dep> config: could not locate the package's config directory`.
- **Legacy `nix-env` profiles:** they record no per-package store paths, so the root can't be resolved, and the error says so.
- **Why the PM does it:** only the PM knows store paths. Keeping the copy generic avoids teaching nix.rs about Elasticsearch.
- **Rewrites on first copy:** a seed can list plain text replacements (`file`, `from`, `to`), applied to the staged copy before it's moved into place, and never on later starts. A listed file that doesn't exist is skipped.
  - Why: nixpkgs' env scripts `cd` to the package root, so the relative `logs/…` and `data` paths in the stock `jvm.options` (GC log, error file, heap dump) resolve into the read-only store and the JVM fails to start. Found in manual verification with OpenSearch 3.5.0.
  - The search modules rewrite those paths to absolute paths under `<data>/logs` and `<data>/data`, and create `<data>/logs` when building the launch spec, because the JVM opens its GC log before the server creates `path.logs`.
- **Alternative rejected:** following the `bin/` symlink from the module. That relies on profile layout details in module code and breaks when the profile is a merged environment.

### D2. Config path through the environment
- **Mechanism:** `ES_PATH_CONF` / `OPENSEARCH_PATH_CONF` = `<data>/config`, set in `LaunchSpec.env` by each module.
- **Why env:** both startup scripts and the keystore tool honor it.
- **Alternative rejected:** `-E path.conf`, which was removed in ES 6.

### D3. Platform-safe settings
- **Elasticsearch:** always gets `-E xpack.ml.enabled=false` under nix. ML's native controller fails on darwin-aarch64 and adds startup cost elsewhere; local development doesn't need it.
- **OpenSearch:** gets `-E plugins.security.disabled=true` only when `<root>/plugins/opensearch-security` exists, because an unknown setting aborts startup. Decided by the PM-resolved root, the module adds it through a `conditional_args: Vec<(package_path, arg)>` on the spec, which the PM evaluates. This keeps the knowledge in the module and the store access in the PM.

### D4. Package paths in the environment
- **Mechanism:** `package_env: Vec<(name, package_path)>` on the spec. The PM sets each variable to `<root>/<package_path>`, or to `<root>` when the path is empty. It fails like a seed does when the root can't be resolved.
- **Elasticsearch:** sets `ES_HOME` to the package root. nixpkgs' 7.17 `elasticsearch-env` exits with `You must set the ES_HOME var` instead of working it out from the script's location; NixOS's module sets it. Found in manual verification.
- **Alternative rejected:** an `ES_HOME` under the data dir that symlinks the package's `lib`, `modules` and `plugins`, as NixOS does. It's more code for the same result, because the config is already relocated with `ES_PATH_CONF`.

### D5. Insecure packages, handled like unfree
- **Mechanism:** `Module::nix_insecure()`, default false and true only for Elasticsearch, sets `Dependency.allow_insecure` through `pkg_dep`. The nix install then adds `NIXPKGS_ALLOW_INSECURE=1` on that one child process, plus `--impure` for flake references. That's the same path unfree uses, and they can be combined.
- **Notice:** a warning, not the unfree info line, because the user should know they're running a package with known vulnerabilities: `<dep>: nixpkgs#<attr> is marked insecure by nixpkgs — allowing insecure packages for this install`.
- **Export:** `config.allowInsecurePredicate` matching by `getName`, next to `allowUnfreePredicate`. Without it the exported shell refuses to evaluate.
- **Why not `permittedInsecurePackages`:** it needs the exact `name-version`, which changes with every nixpkgs bump.
- **Risk accepted:** the service only binds `127.0.0.1` and is for local development. A newer Elasticsearch attribute in nixpkgs would remove the need.


- **A seeded config from an older major version breaks after a nixpkgs bump** → documented in the README: delete `.devy/data/<service>/config` to reseed. A follow-up could stamp the package version next to the seed.
- **The copied config is large** (≈ a few hundred KB) → negligible.
- **A user who reseeds by hand-copying `config/` misses the rewrites** → the README tells them to delete the directory and let devy reseed it.
- **The OpenSearch security plugin may already be absent from nixpkgs' build** → the conditional arg is then a no-op.

## Migration Plan

None. The first nix start after upgrading seeds the config. The servers previously failed to start, so there's no state to migrate.

## Open Questions

- Whether nixpkgs' Elasticsearch 7.17 needs further `-E` settings on Linux (e.g. `bootstrap.system_call_filter`). On macOS arm64 it needed only `ES_HOME` (D4). Linux is unverified.
