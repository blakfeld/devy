## ADDED Requirements

### Requirement: Insecure nix packages
Modules whose nixpkgs package nixpkgs marks insecure SHALL declare it: `elasticsearch`.
- **Install:** when the nix backend installs a declared-insecure package, it SHALL allow insecure packages for that install only. It SHALL print the warning `<dep>: nixpkgs#<attr> is marked insecure by nixpkgs — allowing insecure packages for this install`.
- **Scope:** it MUST NOT allow insecure packages for any other install, and MUST NOT change the user's global Nix configuration.
- **Unfree:** a package that is both unfree and insecure SHALL have both allowed for that install.

#### Scenario: Elasticsearch installs under nix
- **WHEN** `devy up` installs `elasticsearch` with the nix backend
- **THEN** devy prints the unfree notice and the insecure warning for `nixpkgs#elasticsearch`, and the install into `.devy/nix-profile` succeeds without any configuration in `devy.yml`

#### Scenario: Other packages stay secure-only
- **WHEN** `devy up` installs `redis` and `mongodb` with the nix backend
- **THEN** neither install allows insecure packages, and no insecure warning is printed
