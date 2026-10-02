## ADDED Requirements

### Requirement: Unfree packages allowed in exports
When the export lists packages whose modules declare them unfree, the generated file SHALL import nixpkgs with an `allowUnfreePredicate` that permits exactly those packages by name, so the shell evaluates without global configuration. In `flake.nix` this means importing nixpkgs per system, instead of using `legacyPackages`. In `shell.nix` it means the default `pkgs` argument. When no listed package is unfree, the output MUST be unchanged.

#### Scenario: Flake with MongoDB
- **WHEN** `dependencies` contains `mongodb` and the user runs `devy export`
- **THEN** `flake.nix` imports nixpkgs with an `allowUnfreePredicate` that allows `mongodb-ce` and no other unfree package, and `nix flake check` can evaluate the dev shell

#### Scenario: shell.nix with Vault
- **WHEN** `dependencies` contains `vault` and the user runs `devy export --format shell`
- **THEN** `shell.nix` begins with a `pkgs ? import <nixpkgs> { config.allowUnfreePredicate = … }` argument that allows `vault`

#### Scenario: No unfree packages
- **WHEN** `dependencies` contains only `redis` and `node`
- **THEN** the export is identical to the output without this requirement
