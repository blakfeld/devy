# Design

## Context

- **Style selection.** `detect_style` picks `NixStyle::Env` when `nix profile list` fails, for example when `nix-command` isn't enabled.
- **Install and lookup.** In that style, install runs `nix-env --profile <p> -iA nixpkgs.<attr>`, and lookups use `nix-env --profile <p> -q --json`. The resulting entries carry `name`, `pname` and `version`. They do not record the attribute they were installed from: `nix-env -q -P` shows attribute paths only for available packages, not installed ones.
- **Matching today.** `env_find_pkg` matches `pname == attr`. That never matches attributes whose pname differs, and it makes `nodejs` match a `nodejs_22` install.

## Goals / Non-Goals

**Goals:**
- Exact-attribute installed checks in the legacy style, matching the `nix profile` style.
- No reinstall loop.
- Works with existing profiles.

**Non-Goals:**
- **MariaDB/MySQL conflict priorities in the legacy style.** `nix-env -i` has no install-time priority, and resolving collisions there is separate work.
- **Changing style detection.**

## Decisions

### D1. A devy-owned manifest, keyed by attribute
- **What it records:** `.devy/nix-env-attrs.json` holds `{ "<attr>": "<derivation name>" }`, e.g. `{"mysql84": "mysql-8.4.11"}`.
- **Why a manifest:** it's the only reliable link between an installed derivation and the attribute that produced it.
- **Alternatives rejected:**
  - Matching by pname plus version prefix. It needs per-module knowledge of version granularity (python312 vs python313 share pname `python3` and major `3`), and it duplicates `nix_versioned_attr` in the PM.
  - Comparing out paths. It reinstalls after every channel update.

### D2. Learning the derivation name at install time
- **Mechanism:** before installing, run `nix-env -f '<nixpkgs>' -qaA <attr> --json` to get the derivation `name` the install will produce. After a successful install, record it with an atomic write (temp file then rename).
- **Cost:** one evaluation per *install*, not per check.
- **Alternative rejected:** diffing `nix-env -q` before and after the install. A no-op reinstall shows no diff.

### D3. Check order
- **Manifest first:** if the manifest records the attribute, it's installed only when an entry with exactly that `name` is present. A versioned attribute's record never satisfies a different attribute.
- **Fallback:** if there's no record, use today's `pname == attr` match. That keeps pre-manifest profiles working, and devy writes the record on its next install of that attribute.
- **Accepted edge:** the fallback can still make `nodejs` match a `nodejs_22` installed before the manifest existed. That's tolerable for migration, and it goes away once devy installs anything for that dependency.

### D4. Version
The version comes from the entry matched in D3, so the lock gets the version of the right package.

## Risks / Trade-offs

- **The manifest drifts from the profile after a manual `nix-env -u`** → the recorded name no longer matches, so devy reinstalls the attribute and re-records it. That's self-healing.
- **`<nixpkgs>` missing from `NIX_PATH` breaks the pre-install query** → the install itself uses `nixpkgs.<attr>` and fails the same way, so record only on success and surface the install's error.
- **Concurrent `devy up`** → already serialized by the `.devy-lock` guard.

## Migration Plan

None required. Existing profiles fall back to name matching and gain manifest entries as devy installs.
