# Proposal

## Why

Four modules install nixpkgs packages that are marked unfree: `mongodb` (`mongodb-ce`), `elasticsearch`, `vault` and `terraform`. devy runs `nix profile install nixpkgs#<attr>` without allowing unfree packages. Flake evaluation is pure, so even a user's `NIXPKGS_ALLOW_UNFREE=1` is ignored. As a result, `devy up` fails for all four under the default nix backend. The new nix service launch for mongodb, elasticsearch and vault is unreachable in practice.

## What Changes

- **Modules declare whether their nix package is unfree.** This covers mongodb, elasticsearch, vault and terraform.
- **devy allows unfree only for those packages.** It installs them with unfree packages allowed for that one install (`NIXPKGS_ALLOW_UNFREE=1` plus `--impure`, or the legacy `nix-env` equivalent) and prints a one-line notice naming the package. No configuration is needed: `- mongodb` just works. Every other install stays pure and free-only.
- **`devy export` allows exactly those unfree packages.** It emits an `allowUnfreePredicate` listing them, so the generated `flake.nix` / `shell.nix` evaluates. Without unfree packages the output is unchanged.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `package-managers`: the nix backend installs a module's unfree package with unfree allowed for that install only, and says so.
- `nix-export`: the export allows the unfree packages it lists, so the generated shell evaluates.

## Impact

- **Code:**
  - `src/modules/mod.rs`: a new `Module` hook declaring an unfree nix package.
  - `mongodb.rs`, `elasticsearch.rs`, `vault.rs`, and the `PackageModule` static for terraform.
  - `src/package_manager/nix.rs`: the install command and environment.
  - `src/commands/export.rs`: the nixpkgs import with an unfree predicate.
  - `README.md`.
- **Behavior:** installs that used to fail now succeed and print a notice. No change for free packages.
- **Dependencies:** none.
