# Tasks

## 1. Launch spec support

- [ ] 1.1 Add `seed_dirs` and `conditional_args` to `LaunchSpec`, both defaulting to empty. Verify `cargo build`, and that existing launch tests still pass.
- [ ] 1.2 In the nix PM, resolve the package root from `exec_package`. Copy each missing seed directory recursively with owner-write added, and append each conditional arg whose package path exists. Fail with `Failed to prepare <dep> config` when the root can't be resolved. Verify unit tests using a temp "store" directory:
  - a missing destination is copied and becomes writable;
  - an existing destination is left untouched;
  - a conditional arg is added only when its path exists;
  - an unresolvable root errors.

## 2. Search modules

- [ ] 2.1 Update `search_server_launch` and its two callers:
  - set `exec_package` to the module's nix attribute;
  - seed `config` → `<data>/config`;
  - set `ES_PATH_CONF` / `OPENSEARCH_PATH_CONF`;
  - add `-E xpack.ml.enabled=false` (elasticsearch);
  - add the conditional `plugins.security.disabled=true` (opensearch).

  Verify by updating the launch-definition unit tests to assert each of these.
- [ ] 2.2 Document in the README platform notes that search servers keep a writable config under `.devy/data/<service>/config` and how to reseed it. Verify the README says so.

## 3. Integration checks

- [ ] 3.1 Manually, under nix on macOS, run `devy up` with `opensearch`, and with `elasticsearch` once `allow-unfree-nix-packages` has landed. Verify:
  - the health checks pass;
  - `<data>/config/*.keystore` exists;
  - nothing under `/nix/store` changed;
  - a `jvm.options` edit survives `devy restart`.

  Record any extra settings needed (see the design's open question).
- [ ] 3.2 Run `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` and `openspec validate writable-search-config-under-nix --strict`. Verify all pass.
