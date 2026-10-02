# Tasks

## 1. Versions from v3 profiles

- [x] 1.1 Add a pure `version_from_store_paths(paths)` that strips hashes, selects the main output (the name that prefixes the others) and splits at the first `-<digit>`. Verify unit tests cover `redis-8.6.3`, `apache-kafka-2.13-4.3.1`, `python3-3.14.7`, `mongodb-ce-8.2.12`, `awscli2-2.x`, mysql with `-man` listed first, a single output, and a name without a version (→ None).
- [x] 1.2 Make `profile_find_pkg` report "installed" separately from an optional version: an explicit `version` field first, then D1. Verify the existing profile fixtures still pass, and that a v3 fixture reports `8.6.3` instead of `unknown`.
- [x] 1.3 Make `NixPackageManager::resolved_version` return `None` rather than `Some("unknown")`. Verify by unit test, and check that `devy up` output shows `redis@8.6.3` on a v3 profile.

## 2. Resolved version through the install attribute

- [x] 2.1 Add `modules::pkg_resolved_version` (install attribute first, then unversioned, under nix; `dep.name` elsewhere), and use it in node, typescript, python, postgres, mysql, java, dotnet, mongodb (`mongodb-ce`) and `PackageModule`. Verify unit tests with the mock PM:
  - under nix, node queries `nodejs_22` / `nodejs`;
  - under brew, node still queries `node`.
- [x] 2.2 Change the `pkg_installed` lock-pinned fallback to attribute granularity: the installed unversioned version maps to the same versioned attribute. Verify unit tests:
  - locked 24.21.0 with `nodejs` at 24.20.0 is installed;
  - locked 22.1.0 with `nodejs` at 24.20.0 is not installed;
  - an explicit version still needs the versioned attribute.

## 3. Lock stability

- [x] 3.1 In `write_lock`, under nix, keep `dep.version` for lock-pinned deps. Verify unit tests:
  - with the nix mock, a pinned 24.20.0 against installed 24.21.0 keeps 24.20.0 and leaves the lock unwritten;
  - with the brew mock, the installed version is recorded;
  - `--update` (no pin) records the installed version.
- [x] 3.2 Add an `up_impl` test that runs `up` twice under the nix mock with a version-reporting profile. Verify the second run neither installs nor rewrites the lock.
- [x] 3.3 Document under "Versions under Nix" in the README that the lock pins the attribute and records the first resolved version, and that `--update` refreshes it. Verify the README states this.

## 4. Integration checks

- [x] 4.1 Manually, under nix, run `devy up` twice with `redis` and `node`. Verify `devy.lock` records real versions (not `unknown`), the second run installs nothing, and the lock is unchanged.
- [x] 4.2 Run `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` and `openspec validate record-nix-profile-versions --strict`. Verify all pass.
