# Proposal

## Why

Under nix, devy starts Elasticsearch and OpenSearch from the project profile, but their config directory is still the package's own `config/` inside the read-only `/nix/store`. The nixpkgs packages copy `config/` into `$out` and set no config path. On startup both servers write into that directory: they create a keystore, and Elasticsearch writes generated settings. So they fail even though devy passes the right host, port, and data and log paths.

## What Changes

- **A writable config directory per project.** On first start under nix, devy copies the package's `config/` into `.devy/data/<service>/config/`, makes it writable, and points the server at it (`ES_PATH_CONF` / `OPENSEARCH_PATH_CONF`).
- **Edits persist.** Later starts reuse that directory, so changes users make, like JVM heap in `jvm.options`, are kept.
- **Elasticsearch runs with machine learning disabled** (`-E xpack.ml.enabled=false`). Its native ML controller isn't available on every platform nixpkgs supports (e.g. Apple silicon), and it isn't needed for local development.
- **OpenSearch starts without TLS** when the package bundles the security plugin, so the existing plain-HTTP health check works.
- **Elasticsearch runs with `ES_HOME` set** to its package directory, which nixpkgs' start script requires.
- **Elasticsearch installs although nixpkgs marks it insecure.** nixpkgs flags `elasticsearch-7.17.27` as insecure (7.x is end-of-life), so `nix profile install` refuses it. Modules can now declare their package insecure, like unfree. The nix backend then allows insecure packages for that one install, with a warning, and `devy export` permits exactly those packages. Elasticsearch is the only such module.
- **Unfree:** Elasticsearch's package is also unfree, which `allow-unfree-nix-packages` already handles. OpenSearch is free and secure.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `service-modules`: Elasticsearch and OpenSearch under nix run with a writable, project-local config directory, plus the startup settings above.
- `package-managers`: nix installs of packages that modules declare insecure.
- `nix-export`: exports permit the insecure packages they list.

## Impact

- **Code:**
  - `src/modules/mod.rs`: the search launch spec, plus a launch-spec field for seeding a directory from the package.
  - `src/package_manager/nix.rs`: resolving the package root and copying `config/` once.
  - `elasticsearch.rs`, `opensearch.rs`.
  - `src/config.rs` (`Dependency`), `src/commands/export.rs`: the insecure allowance.
  - `README.md`.
- **Files:** `.devy/data/elasticsearch/config/` and `.devy/data/opensearch/config/`.
- **Dependencies:** none.
