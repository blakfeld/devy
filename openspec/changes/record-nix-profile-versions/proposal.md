# Proposal

## Why

Under the nix backend, `devy.lock` never holds a usable version, so lock pinning does nothing. There are two separate causes:
- **Nix ≥ 2.20 profiles carry no version.** `nix profile list --json` (v3) entries have no `pname` or `version`, so devy records `resolved_version: unknown` and prints `redis@unknown`.
- **devy looks up the wrong name.** It asks for the version under the `devy.yml` name, not the attribute it installed, so node (`nodejs`), python (`python3`), java (`jdk21`) and others record nothing under nix at any Nix version.

Once real versions are recorded, a naive rule would make the lock flap between teammates on different nixpkgs revisions, because the same attribute resolves to different patch versions. It could also re-trigger profile file conflicts. So this change defines what a lock version means under nix.

## What Changes

- **Real versions from v3 profile entries.** When a profile entry has no version, devy derives it from the entry's main store path name, using Nix's own name/version split. `unknown` is no longer recorded; when no version can be derived, `resolved_version` is null.
- **The version comes from the installed attribute.** Under nix, a dependency's resolved version is read from the attribute devy installs for it (versioned or not), so node, python, java, dotnet, go and mysql record versions.
- **Pinning works at the attribute's granularity.** Nix installs attributes, not exact versions. A lock-pinned version is satisfied by an installed package whose version maps to the same versioned attribute, or, for modules without versioned attributes, by the package being installed at all.
- **Lock-pinned versions are kept.** Under nix, a version pinned from the lock stays in the lock unchanged; it's rewritten only by `devy up --update` or when `devy.yml` changes the dependency. This stops teammates' locks from flapping between patch versions.
- **BREAKING (lock contents):** the first `devy up` after upgrading rewrites nix lock entries from `unknown` (or null) to real versions. That's a one-time diff.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `package-managers`: the nix installed check accepts lock-pinned versions at attribute granularity. A new requirement covers how nix reports versions.
- `dependency-modules`: resolved versions for the lock come from the installed nix attribute.
- `lock-file`: under nix, versions pinned from the lock stay unchanged.

## Impact

- **Code:**
  - `src/package_manager/nix.rs`: version derivation from store paths.
  - `src/modules/mod.rs`: `pkg_installed` fallback, plus a resolved-version helper that queries the install attribute.
  - The modules that use `pkg_dep` (node, typescript, python, postgres, mysql, java, dotnet, and `PackageModule`).
  - `src/commands/up.rs`: `write_lock` keeps lock-pinned versions under nix.
  - `README.md`.
- **Files:** `devy.lock` changes once per nix project.
- **Dependencies:** none.
