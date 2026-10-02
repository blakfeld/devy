# Design

## Context

- **Install command.** `NixPackageManager::install_package` runs `nix profile install --profile <p> nixpkgs#<attr>`. The legacy fallback is `nix-env --profile <p> -iA nixpkgs.<attr>`.
- **Why env vars don't work.** Flake references evaluate in pure mode, so `NIXPKGS_ALLOW_UNFREE` from the environment is ignored unless `--impure` is passed. `nix-env` evaluates impurely and honors the variable.
- **Unfree packages today.** Probed in nixpkgs (2026-10): `mongodb-ce`, `mongodb`, `elasticsearch`, `vault` and `terraform` have `meta.unfree = true`. Every other attribute devy installs is free.
- **Export.** `devy export` builds `nixpkgs.legacyPackages.${system}` (flake) or `import <nixpkgs> {}` (shell). Neither form allows unfree packages.

## Goals / Non-Goals

**Goals:**
- Installing a known-unfree module works with no configuration.
- Allowing unfree is scoped to exactly that install.

**Non-Goals:**
- **A user-facing opt-in or opt-out switch.** The user chose automatic behavior for known modules.
- **Unfree packages for the generic module.** If `jq`-style generic deps turn out to be unfree, nix's own error stands.
- **Detecting unfree status at runtime from nixpkgs metadata.** The allowlist is static.

## Decisions

### D1. Static declaration on the module
- **Mechanism:** add `fn nix_unfree(&self) -> bool` (default `false`) to `Module`. mongodb, elasticsearch and vault override it. `PackageModule` gains a `nix_unfree: bool` field, true only for terraform.
- **Why static:** it matches how attribute names are already declared per module.
- **Alternative rejected:** evaluating `meta.unfree` before each install costs an extra nix evaluation on every `devy up`, and the set is small and stable.

### D2. Carrying the flag to the package manager
- **Problem:** `PackageManager::install_package(&Dependency)` has no slot for the flag.
- **Mechanism:** add `allow_unfree: bool` to `Dependency`, alongside `version_from_lock`. It's never serialized, and `pm_dep` sets it from the module when it builds the install dependency.
- **Alternative rejected:** a separate `install_unfree` method on the trait would duplicate both install paths in nix.rs and the mocks.
- **Where it's set:** a small helper that also sets the name, used by every install path that goes through `pkg_dep`/`pm_dep`. The flag only matters to the nix PM; brew, apt and winget ignore it.

### D3. Install command when unfree is allowed
- **`Profile` style:** add `--impure` and set `NIXPKGS_ALLOW_UNFREE=1` on that one child process only. The environment of devy and of other installs is untouched.
- **`Env` style:** set `NIXPKGS_ALLOW_UNFREE=1` on the `nix-env` child.
- **Notice:** printed before the install step line, via `output::info`.
- **Alternative rejected:** `--option allow-unfree`. That isn't a Nix option; unfree is a nixpkgs config, not a Nix setting.

### D4. Export predicate
- **Collecting names:** `collect_pkg_lines` already resolves each dependency's attribute. It also collects the attributes whose module is `nix_unfree()`.
- **Flake (when that list is non-empty):** replace `nixpkgs.legacyPackages.${system}` with `import nixpkgs { inherit system; config.allowUnfreePredicate = pkg: builtins.elem (nixpkgs.lib.getName pkg) [ … ]; }`.
- **shell.nix:** use the same predicate inside the `import <nixpkgs> { … }` default argument.
- **Matching:** `lib.getName` returns the package's pname, while devy knows attribute names. For all four packages the pname equals the attribute (`mongodb-ce`, `elasticsearch`, `vault`, `terraform`), so the predicate lists attribute names. A unit-level fixture pins that assumption, and a module whose pname differs would need its pname recorded instead.

## Risks / Trade-offs

- **A free package becomes unfree upstream** (or the reverse) → install fails with nix's clear unfree error. The module list is easy to update. A unit test pins the current set.
- **`--impure` lets the environment affect evaluation** (e.g. `NIX_PATH`) → it applies only to the unfree installs, which already depend on nixpkgs-unstable.
- **A vault/terraform version line that's unfree under the BSL** may change → covered by the static list.

## Migration Plan

None. Installs that failed now succeed. Existing profiles are unaffected.
