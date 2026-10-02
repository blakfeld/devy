## MODIFIED Requirements

### Requirement: Project-local Nix profile
The Nix backend SHALL install packages into the project-local profile `<project_root>/.devy/nix-profile` using `nix profile install --profile <profile> nixpkgs#<attr>`. It SHALL fall back to `nix-env --profile <profile> -iA nixpkgs.<attr>` when the `nix profile` command is unavailable. `<attr>` SHALL be the module's versioned nixpkgs attribute when the dependency has a version that maps to one, and its unversioned attribute otherwise.

#### Scenario: Install jq
- **WHEN** `devy up` installs `jq` with the nix backend
- **THEN** `jq` appears at `.devy/nix-profile/bin/jq` and nothing is installed into the user's global profile

#### Scenario: Versioned install
- **WHEN** `devy up` installs `node` with `version: "22"` using the nix backend
- **THEN** devy runs `nix profile install --profile <profile> nixpkgs#nodejs_22`

### Requirement: Nix installed check is project-scoped
The Nix backend SHALL treat a package as installed only if it appears in the project-local profile, matching the entry's attribute path against the exact attribute devy would install (versioned or not). When the profile does not exist yet, it MUST report nothing as installed. When the version was pinned from `devy.lock`, the unversioned attribute already installed at exactly that version SHALL also count as installed, so a later `devy up` doesn't reinstall it under its versioned name.

#### Scenario: Globally installed package
- **WHEN** `jq` is installed in the user's global Nix profile but `.devy/nix-profile` does not exist
- **THEN** devy reports `jq` as not installed and installs it into the project profile

#### Scenario: Versioned attribute recognized
- **WHEN** `nodejs_22` is in the project profile and `node` is declared with `version: "22"`
- **THEN** devy reports `node` as installed and does not reinstall it on the next `devy up`

#### Scenario: Mapped attribute recognized
- **WHEN** `mysql84` (package name `mysql`) is in the project profile and `mysql` is declared
- **THEN** devy reports `mysql` as installed

#### Scenario: Version changed
- **WHEN** `nodejs_22` is in the project profile and `node` is now declared with `version: "24"`
- **THEN** devy reports `node` as not installed and installs `nodejs_24`

## ADDED Requirements

### Requirement: Unhonored nix versions are reported
When the nix backend cannot map a dependency's `version` from `devy.yml` to a versioned nixpkgs attribute, `devy up` SHALL install the unversioned attribute and warn `<dep>: version <v> is not supported by the nix backend — installing the nixpkgs default`. A version that came only from `devy.lock` pinning MUST NOT produce this warning.

#### Scenario: Explicit unmapped version
- **WHEN** `jq` is declared with `version: "1.6"` under nix
- **THEN** `devy up` installs `nixpkgs#jq` and prints the version-not-supported warning

#### Scenario: Lock-pinned version
- **WHEN** `jq` has no `version` in `devy.yml` and `devy.lock` records `resolved_version: 1.7.1`
- **THEN** `devy up` prints no version warning for `jq`

### Requirement: MariaDB wins nix profile conflicts with MySQL
`mysql` and `mariadb` ship many of the same file names (`mysql`, `mysqldump`, `mysqld` and others). The nix backend SHALL install `mariadb` with profile priority 4, so that in the project profile those names resolve to MariaDB whichever package is installed first. The `mysql` service SHALL still run MySQL's own `mysqld` from the `mysql` package, not from the merged profile.

#### Scenario: Both installed
- **WHEN** `devy.yml` lists `mysql` and `mariadb` and `devy up` runs under nix
- **THEN** devy runs `nix profile install nixpkgs#mariadb --priority 4`, both packages are installed, `.devy/nix-profile/bin/mysqld` is MariaDB's, and the `mysql` service reports a MySQL server version
