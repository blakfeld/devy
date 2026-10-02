## MODIFIED Requirements

### Requirement: Nix installed check is project-scoped
The Nix backend SHALL treat a package as installed only if it appears in the project-local profile, matching the entry's attribute path against the exact attribute devy would install (versioned or not). When the profile does not exist yet, it MUST report nothing as installed. When the version was pinned from `devy.lock`, nix installs attributes rather than exact versions, so the check works at the attribute's granularity. An installed unversioned attribute SHALL also count as installed when its version maps to the same versioned attribute as the locked version. For a module without versioned attributes, the installed package satisfies any locked version. A later `devy up` therefore never reinstalls a package under its versioned name.

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

#### Scenario: Locked patch version from a newer nixpkgs
- **WHEN** `nodejs` 24.20.0 is in the project profile and `devy.lock` records node `24.21.0`, written by a teammate whose nixpkgs is newer
- **THEN** devy reports `node` as installed and installs nothing

## ADDED Requirements

### Requirement: Nix reports installed versions
The Nix backend SHALL report a package's installed version from its profile entry. When the entry records no version, as with Nix ≥ 2.20, the backend SHALL derive the version from the name of the entry's main output store path. It splits at the first `-` that is followed by a digit, as Nix itself does: `redis-8.6.3` gives `8.6.3`, and `apache-kafka-2.13-4.3.1` gives `2.13-4.3.1`. The main output is the store path whose name is a prefix of the entry's other outputs' names (so `mysql-8.4.11` rather than `mysql-8.4.11-man`). When no version can be derived, the reported version SHALL be absent. It MUST NOT be the string `unknown`.

#### Scenario: Version from a v3 profile entry
- **WHEN** the project profile contains `redis` with store path `/nix/store/<hash>-redis-8.6.3` and no version field
- **THEN** devy reports redis as installed at `8.6.3`, and `devy up` prints `redis@8.6.3 already installed`

#### Scenario: Multiple outputs
- **WHEN** the profile entry for `mysql84` lists `<hash>-mysql-8.4.11-man` before `<hash>-mysql-8.4.11`
- **THEN** devy reports version `8.4.11`
