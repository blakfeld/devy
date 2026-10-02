# Tasks

## 1. Declare unfree packages

- [ ] 1.1 Add `Module::nix_unfree()` (default `false`), and override it for mongodb, elasticsearch and vault. Add a `nix_unfree` field to `PackageModule`, set only for terraform. Verify a unit test asserts exactly these four modules report unfree.
- [ ] 1.2 Add a never-serialized `allow_unfree` flag to `Dependency`, set by the helper that builds a module's install dependency from `nix_unfree()`. Verify unit tests show `mongodb`'s nix install dep has `allow_unfree = true` and `redis`'s does not.

## 2. Nix install

- [ ] 2.1 In `NixPackageManager::install_package`, when `allow_unfree` is set:
  - Profile style: pass `--impure` and set `NIXPKGS_ALLOW_UNFREE=1` on that child only.
  - Env style: set the variable on that child.
  - Print the unfree notice.

  Verify by unit-testing the argv/env builder (extract it as a pure function) for both styles, with and without the flag.
- [ ] 2.2 Verify manually under nix that `devy up` with `mongodb` and `vault` installs both (with notices) and that redis installs without `--impure`. Record the result in this task.
- [ ] 2.3 Document unfree handling in the README "Versions under Nix" / platform notes section, naming the four modules. Verify the README mentions `mongodb-ce`, `elasticsearch`, `vault` and `terraform`.

## 3. Export

- [ ] 3.1 Collect unfree attribute names in `collect_pkg_lines`. Emit the `allowUnfreePredicate` import in the flake and shell.nix generators only when that list is non-empty. Verify unit tests:
  - mongodb produces the predicate with exactly `mongodb-ce`;
  - redis + node produce byte-identical output to before;
  - both formats still pass the balanced-braces test, and `nix-instantiate --parse` when it's on PATH.
- [ ] 3.2 Verify manually that `nix flake show` (or `nix develop --command true`) on an exported flake with `mongodb` evaluates without `NIXPKGS_ALLOW_UNFREE`.

## 4. Integration checks

- [ ] 4.1 Run `cargo test`, `cargo clippy -- -D warnings` and `cargo fmt --check`. Verify all pass.
- [ ] 4.2 Run `openspec validate allow-unfree-nix-packages --strict`. Verify the change is valid.
