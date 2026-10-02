## MODIFIED Requirements

### Requirement: Export format and destination
The system SHALL write `flake.nix` (the default, `--format flake`) or `shell.nix` (`--format shell`) into the project root, the directory containing `devy.yml`. The written file MUST be a syntactically valid Nix expression. Export MUST NOT require or detect a package manager. It MUST print `✓ Wrote <path>` on success and fail with `Failed to write <path>` if the write fails.

#### Scenario: Default flake export
- **WHEN** the user runs `devy export` in a devy project
- **THEN** `<project_root>/flake.nix` is written and `✓ Wrote <project_root>/flake.nix` is printed

#### Scenario: shell.nix export
- **WHEN** the user runs `devy export --format shell`
- **THEN** `<project_root>/shell.nix` is written with `{ pkgs ? import <nixpkgs> {} }:` and a `pkgs.mkShell` expression

#### Scenario: shell.nix is well-formed
- **WHEN** the user runs `devy export --format shell`
- **THEN** the braces in `shell.nix` are balanced, the file ends with the closing `}` of the `mkShell` attribute set, and `nix-instantiate --parse shell.nix` succeeds

### Requirement: Packages from dependency nix attributes
The system SHALL include one `pkgs.<attr>` entry per dependency. The attribute comes from the module for the dependency's canonical name, so aliases resolve. When the dependency's `devy.yml` version maps to a versioned nixpkgs attribute, that attribute is used. Dependencies whose module has no nixpkgs attribute, or an empty one, MUST be omitted.

#### Scenario: Aliased dependency
- **WHEN** `dependencies` contains `nodejs`
- **THEN** the export contains `pkgs.nodejs`

#### Scenario: Versioned dependency
- **WHEN** `dependencies` contains `node` with `version: "22"`
- **THEN** the export contains `pkgs.nodejs_22`

#### Scenario: No packages
- **WHEN** no dependency has a nix attribute
- **THEN** the export has no `packages` list

#### Scenario: Dependency without a built-in module
- **WHEN** `dependencies` contains `jq`
- **THEN** the export contains no entry for it and no warning is printed
