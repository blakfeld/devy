# Proposal

## Why

devy uses the legacy `nix-env` style whenever `nix profile` is unavailable. That includes stock Nix installs that haven't enabled the `nix-command` experimental feature, so it's a common path. In that style the installed check still compares each entry's package name to the attribute devy installs, so several packages are never recognized and `devy up` reinstalls them on every run:
- versioned attributes: `nodejs_22`, `python312`, `postgresql_16`;
- renamed attributes: `mysql84` (pname `mysql`), `jdk21` (pname `openjdk`), `rabbitmq-server`, `mongodb-ce`.

Unversioned matches are also ambiguous: `nodejs_22` satisfies a check for `nodejs`.

## What Changes

- **devy records what it installs.** It writes a small manifest in `.devy/` that maps each nixpkgs attribute it installed with `nix-env` to the exact derivation name that install produced.
- **The installed check consults the manifest.** An attribute counts as installed when the manifest records it and the profile still contains that derivation, so versioned and renamed attributes are recognized and `nodejs` no longer matches `nodejs_22`.
- **Old profiles keep working.** For packages installed before the manifest existed, the check falls back to today's package-name match. The manifest fills in on the next install.
- **Versions come from the manifest's entry** (via the profile element it names), not from the first element with the same package name.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `package-managers`: the nix installed check matches the exact attribute in the legacy `nix-env` style, the same way it already does in the `nix profile` style.

## Impact

- **Code:** `src/package_manager/nix.rs` (the `NixStyle::Env` install, installed check and version query, plus manifest read/write).
- **Files:** a new `.devy/nix-env-attrs.json`, written only by the legacy style. It lives under `.devy/`, which projects already ignore.
- **Dependencies:** none.
