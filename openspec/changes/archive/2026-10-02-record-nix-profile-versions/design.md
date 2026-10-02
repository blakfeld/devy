# Design

## Context

- **Profile entries.** `profile_find_entry` matches profile entries by attribute path tail. `profile_find_pkg` returns `entry.version`, or the string `unknown` when the entry has none. On Nix 2.34 (profile JSON v3), entries carry `attrPath`, `storePaths` and `priority`, but no `pname` or `version`. `storePaths` lists every output in no particular order (`-man` can come first).
- **Lock writes.** `write_lock` asks `module.resolved_version(pm, dep)`. The default implementation calls `pm.resolved_version(dep)` with the `devy.yml` name (`node`), so under nix it finds nothing for any module whose attribute differs from its name.
- **Lock pinning.** `apply_lock_from_source` pins versions only from same-source entries and sets `version_from_lock`. `pkg_installed` accepts the unversioned attribute when its version *equals* the locked one.

## Goals / Non-Goals

**Goals:**
- Nix lock entries hold real versions.
- Locks don't flap between machines.
- No reinstalls or profile conflicts from lock pinning.

**Non-Goals:**
- **Pinning exact patch versions under nix.** That would need nixpkgs revision pinning, which was a non-goal of the previous change.
- **Changing what brew, apt or winget record.**
- **The legacy `nix-env` style's version reporting.** Its entries already carry `version`; see the match-attrs-in-nix-env-style change.

## Decisions

### D1. Derive the version from the main output's store path name
- **Main output:** strip the 32-character hash and `-` from each store path. The main output is the name that is a prefix of every other output's name, or the only name when there's just one.
- **Split:** apply Nix's `parseDrvName` rule: the version starts after the first `-` that is followed by a digit.
- **When it's used:** only when the entry has no `version` field; an explicit `version` still wins.
- **Alternative rejected:** `nix path-info --json` doesn't report versions. `nix eval` per package costs one evaluation per dependency on every `devy up`.

### D2. Absent instead of `unknown`
`profile_find_pkg` keeps returning `Some(..)` to mean "installed", so the installed check is unaffected. The version part becomes an `Option`. This means splitting "installed?" from "which version?" internally: `profile_find_entry(..).is_some()` for the former, and the derived version for the latter. `resolved_version` returns `None` instead of `Some("unknown")`.

### D3. Resolved version through the install attribute
- **Helper:** add `modules::pkg_resolved_version(module, pm, dep, name)`, a mirror of `pkg_installed`. Under nix it queries `pkg_dep(..)`'s attribute first, then the unversioned `name`. Other backends keep today's behavior (query `dep.name`), so brew/apt/winget lock contents don't change.
- **Where it's used:** modules already using `pkg_installed` override `resolved_version` to call it.
- **Alternative rejected:** making the default `Module::resolved_version` smarter. The default doesn't know the package name, and modules without a nix mapping already work through the name.

### D4. Pinning at attribute granularity
- **Change to `pkg_installed`'s fallback:** when the version came from the lock and the target attribute isn't installed, read the unversioned attribute's version (D1). It counts as installed when `module.nix_versioned_attr(installed)` equals the target attribute.
- **Modules without versioned attributes:** these never reach the fallback, because the target equals the unversioned attribute.
- **Why not equality:** equality made a teammate's newer patch version force an install of the versioned attribute, which conflicts in the profile with the unversioned one (same `bin/node`).

### D5. Keep lock-pinned versions on write
- **Rule:** in `write_lock`, under nix, when `dep.version_from_lock` is set, record `dep.version` (the locked value) instead of the re-resolved one.
- **Other backends:** keep recording the resolved version, which is what they install exactly.
- **`--update`:** clears pins via `apply_lock` being skipped, so fresh versions are written.
- **Alternative rejected:** comparing old and new versions by attribute at write time. That's more logic for the same outcome, and it would still rewrite the lock for modules without versioned attributes.

## Risks / Trade-offs

- **Store names that don't follow the convention** (e.g. a name with a `-<digit>` early, like `python3.12-foo-1.0`?) → the split matches Nix's own `parseDrvName`, so what devy reports agrees with `nix-env`. Unit tests cover kafka's `2.13-4.3.1`.
- **The lock now understates what's installed** (it says 24.20.0 when 24.21.0 is installed) → documented: under nix the lock pins the attribute and records the first resolved version. `devy up --update` refreshes it.
- **One-time lock churn** when upgrading → noted in the proposal as BREAKING (lock contents only).

## Migration Plan

No user action. The first `devy up` after upgrading rewrites `unknown`/null nix versions to real ones. Pinned versions from older locks that say `unknown` map to no attribute, so they behave as unpinned until rewritten.
