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
- **Why the PM does it:** only the PM knows store paths. Keeping the copy generic avoids teaching nix.rs about Elasticsearch.
- **Alternative rejected:** following the `bin/` symlink from the module. That relies on profile layout details in module code and breaks when the profile is a merged environment.

### D2. Config path through the environment
- **Mechanism:** `ES_PATH_CONF` / `OPENSEARCH_PATH_CONF` = `<data>/config`, set in `LaunchSpec.env` by each module.
- **Why env:** both startup scripts and the keystore tool honor it.
- **Alternative rejected:** `-E path.conf`, which was removed in ES 6.

### D3. Platform-safe settings
- **Elasticsearch:** always gets `-E xpack.ml.enabled=false` under nix. ML's native controller fails on darwin-aarch64 and adds startup cost elsewhere; local development doesn't need it.
- **OpenSearch:** gets `-E plugins.security.disabled=true` only when `<root>/plugins/opensearch-security` exists, because an unknown setting aborts startup. Decided by the PM-resolved root, the module adds it through a `conditional_args: Vec<(package_path, arg)>` on the spec, which the PM evaluates. This keeps the knowledge in the module and the store access in the PM.

## Risks / Trade-offs

- **A seeded config from an older major version breaks after a nixpkgs bump** → documented in the README: delete `.devy/data/<service>/config` to reseed. A follow-up could stamp the package version next to the seed.
- **The copied config is large** (≈ a few hundred KB) → negligible.
- **The OpenSearch security plugin may already be absent from nixpkgs' build** → the conditional arg is then a no-op.

## Migration Plan

None. The first nix start after upgrading seeds the config. The servers previously failed to start, so there's no state to migrate.

## Open Questions

- Whether nixpkgs' Elasticsearch 7.17 needs further `-E` settings on Linux (e.g. `bootstrap.system_call_filter`). Answer it during manual verification (task 3.1); it would only add an argument and doesn't change the approach.
