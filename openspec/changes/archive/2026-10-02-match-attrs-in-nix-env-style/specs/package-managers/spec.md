## ADDED Requirements

### Requirement: Legacy nix-env style tracks installed attributes
When the Nix backend uses the legacy `nix-env` style, devy SHALL record each nixpkgs attribute it installs, together with the exact derivation name that install produced, in `<project_root>/.devy/nix-env-attrs.json`.
- **Installed:** an attribute is installed when the file records it and the project profile still contains a package with that derivation name. Its version SHALL be that package's version.
- **No record:** for an attribute with no record, which happens in profiles created before this file existed, devy SHALL fall back to matching a profile package whose name equals the attribute.
- **Missing or unreadable file:** devy SHALL treat it as empty and MUST NOT fail.

#### Scenario: Renamed attribute recognized
- **WHEN** devy installed `mysql84` with `nix-env` (package name `mysql`) and `devy up` runs again
- **THEN** devy reports `mysql` as installed and does not run `nix-env -iA` again

#### Scenario: Versioned attribute does not satisfy the unversioned one
- **WHEN** devy installed `nodejs_22` with `nix-env`, and `node` is now declared without a version
- **THEN** devy reports `node` as not installed and installs `nodejs`

#### Scenario: Profile from before the manifest
- **WHEN** a `nix-env` project profile contains `jq` and `.devy/nix-env-attrs.json` does not exist
- **THEN** devy reports `jq` as installed by its package name

#### Scenario: Package removed outside devy
- **WHEN** the manifest records `redis` but the user removed it with `nix-env -e redis`
- **THEN** devy reports `redis` as not installed and reinstalls it
