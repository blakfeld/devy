# Tasks

## 1. Manifest

- [x] 1.1 Add manifest read/write helpers for `.devy/nix-env-attrs.json`: a missing or unreadable file is treated as empty, and writes are atomic. Verify unit tests for round-trip, a missing file, malformed JSON (treated as empty) and atomic replacement.
- [x] 1.2 Add a pure `env_find_attr(json, manifest, attr)` implementing D3:
  - a manifest name must match exactly;
  - otherwise fall back to pname;
  - return the matched entry's version.

  Verify unit tests with `nix-env -q --json` fixtures:
  - `mysql84` recorded as `mysql-8.4.11` → installed;
  - `nodejs_22` recorded, query for `nodejs` → not installed;
  - no manifest with `jq` present → installed;
  - recorded name absent from the profile → not installed.

## 2. Install path

- [x] 2.1 Before `nix-env -iA`, query the derivation name with `nix-env -qaA nixpkgs.<attr> --json` (the same source the install resolves), and record it after a successful install. Verify by unit-testing the command builder and the record step with a fake runner, and that a failed install records nothing.
- [x] 2.2 Use `env_find_attr` for `is_package_installed` and `resolved_version` in the Env style. Verify by unit test with a manifest and profile fixtures.

## 3. Integration checks

- [x] 3.1 Manually force the Env style (`nix` without `nix-command`, e.g. `NIX_CONFIG='experimental-features ='`) and run `devy up` twice with `mysql` and `node` (version 22). Verify the second run installs nothing. Record the result here.
  - **Result: not run; waived by the user.** The dev machine runs Determinate Nix 2.34, where `nix-command` is built in, so the Env style can't be forced through `NIX_CONFIG`. It also has no `nixpkgs` channel (`~/.nix-defexpr`). The `nix-env -qaA nixpkgs.<attr> --json` output shape is covered only by unit-test fixtures.
- [x] 3.2 Run `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` and `openspec validate match-attrs-in-nix-env-style --strict`. Verify all pass.
