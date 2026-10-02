## ADDED Requirements

### Requirement: Unfree nix packages
Modules whose nixpkgs package is unfree SHALL declare it: `mongodb` (`mongodb-ce`), `elasticsearch`, `vault` and `terraform`. When the nix backend installs a declared-unfree package, it SHALL allow unfree packages for that install only, and print `<dep>: nixpkgs#<attr> is unfree — allowing unfree packages for this install`. It MUST NOT allow unfree packages for any other install. It MUST NOT change the user's global Nix configuration.

#### Scenario: MongoDB installs under nix
- **WHEN** `devy up` installs `mongodb` with the nix backend
- **THEN** devy prints that `nixpkgs#mongodb-ce` is unfree, installs it into `.devy/nix-profile`, and the install succeeds without any configuration in `devy.yml`

#### Scenario: Free packages stay free-only
- **WHEN** `devy up` installs `redis` and `vault` with the nix backend
- **THEN** only the `vault` install allows unfree packages, and no unfree notice is printed for `redis`

#### Scenario: Unfree dependency of a free package
- **WHEN** a module that does not declare an unfree package resolves to a nixpkgs attribute that nixpkgs refuses as unfree
- **THEN** the install fails with nix's own unfree error, and devy does not retry with unfree allowed
