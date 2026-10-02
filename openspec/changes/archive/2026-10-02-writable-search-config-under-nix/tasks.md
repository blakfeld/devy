# Tasks

## 1. Launch spec support

- [x] 1.1 Add `seed_dirs` and `conditional_args` to `LaunchSpec`, both defaulting to empty. Verify `cargo build`, and that existing launch tests still pass.
- [x] 1.2 In the nix PM, resolve the package root from `exec_package`. Copy each missing seed directory recursively with owner-write added, and append each conditional arg whose package path exists. Fail with `Failed to prepare <dep> config` when the root can't be resolved. Verify unit tests using a temp "store" directory:
  - a missing destination is copied and becomes writable;
  - an existing destination is left untouched;
  - a conditional arg is added only when its path exists;
  - an unresolvable root errors.
- [x] 1.3 Add optional text rewrites to seed directories, applied to the staged copy on first seed only, skipping files that don't exist. Verify unit tests: a rewrite is applied on the first copy, a missing file is skipped, and an existing destination is not rewritten.

- [x] 1.4 Add `package_env` to `LaunchSpec`. The nix PM sets each variable to the package path under the resolved root (the root itself for an empty path), and fails like a seed when the root can't be resolved. Verify unit tests: the root and a subpath resolve, and an unresolvable root errors.

## 2. Search modules

- [x] 2.1 Update `search_server_launch` and its two callers:
  - set `exec_package` to the module's nix attribute;
  - seed `config` → `<data>/config`;
  - set `ES_PATH_CONF` / `OPENSEARCH_PATH_CONF`;
  - add `-E xpack.ml.enabled=false` (elasticsearch);
  - add the conditional `plugins.security.disabled=true` (opensearch);
  - rewrite the relative `logs/` and `data` paths in `jvm.options` to absolute paths, and create `<data>/logs`;
  - set `ES_HOME` to the package root (elasticsearch).

  Verify by updating the launch-definition unit tests to assert each of these.
- [x] 2.2 Document in the README platform notes that search servers keep a writable config under `.devy/data/<service>/config` and how to reseed it. Verify the README says so.

## 3. Insecure packages

- [x] 3.1 Add `Module::nix_insecure` (true for `elasticsearch`) and `Dependency.allow_insecure`, set by `pkg_dep`. The nix install sets `NIXPKGS_ALLOW_INSECURE=1` and `--impure` for that install, and prints the insecure warning. Verify unit tests: install command with insecure only and with insecure plus unfree, `pkg_dep` sets the flag only for elasticsearch under nix, and only `elasticsearch` declares itself insecure.
- [x] 3.2 `devy export` adds `allowInsecurePredicate` for declared-insecure packages in both formats. Verify unit tests for flake and shell output with and without elasticsearch, and that the flake output for elasticsearch evaluates with `nix`.
- [x] 3.3 Document the insecure allowance in the README next to the unfree note. Verify the README says so.

## 4. Integration checks

- [x] 4.1 Manually, under nix on macOS, run `devy up` with `opensearch`, and with `elasticsearch` once `allow-unfree-nix-packages` has landed. Verify:
  - the health checks pass;
  - `<data>/config/*.keystore` exists;
  - nothing under `/nix/store` changed;
  - a `jvm.options` edit survives `devy restart`.

  Record any extra settings needed (see the design's open question).

  Results (2026-10-02, macOS arm64):
  - OpenSearch 3.5.0 passes every check. It also needed the `jvm.options` path rewrites (task 1.3). The keystore tool logs a harmless `opensearch-cli: No such file`, because nixpkgs omits that file.
  - Elasticsearch 7.17.27 passes every check once `ES_HOME` is set (task 1.4). To verify it, I installed it by hand into a scratch profile with `NIXPKGS_ALLOW_INSECURE=1`, because nixpkgs marks it insecure (handled by section 3).
  - Linux is unverified.
- [x] 4.2 Run `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` and `openspec validate writable-search-config-under-nix --strict`. Verify all pass.
- [x] 4.3 Manually, in a fresh project, run `devy up` with `elasticsearch` under nix. Verify the install succeeds with the insecure warning, and the health check passes.
