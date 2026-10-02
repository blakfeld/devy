## ADDED Requirements

### Requirement: Locked versions are kept under nix
Under the nix backend, when `devy up` applies a dependency's version from `devy.lock`, it SHALL write that same locked version back to the lock, even if the installed package reports a different patch version. Nix installs attributes, not exact versions, and teammates on different nixpkgs revisions would otherwise rewrite each other's locks. The locked version SHALL change only when `devy up --update` runs, when `devy.yml` sets an explicit version, or when the dependency has no lock entry yet.

#### Scenario: Teammate with newer nixpkgs
- **WHEN** `devy.lock` records node `24.20.0`, the user's profile has `nodejs` 24.21.0, and the user runs `devy up` under nix
- **THEN** `devy.lock` still records `24.20.0` and is not rewritten

#### Scenario: Update refreshes the version
- **WHEN** the same user runs `devy up --update`
- **THEN** `devy.lock` records `24.21.0`

#### Scenario: Other backends unchanged
- **WHEN** the backend is brew and the installed version differs from the locked one
- **THEN** devy records the installed version, as before
