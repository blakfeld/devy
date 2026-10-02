# nix-export Specification

## Purpose
`devy export` turns `devy.yml` into a standalone Nix development shell (`flake.nix` or `shell.nix`). Projects can then enter an equivalent environment with plain Nix tooling.

## Requirements

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

### Requirement: Overwrite warning
The system SHALL overwrite an existing export file after warning `<path> already exists — overwriting`.

#### Scenario: Existing flake
- **WHEN** `flake.nix` already exists and the user runs `devy export`
- **THEN** devy warns that it is overwriting, then replaces the file

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

### Requirement: Environment variables exported as attributes
The system SHALL emit each `environment:` entry as an attribute of the mkShell. Keys that are not valid bare Nix identifiers MUST be quoted. Values MUST be emitted as Nix double-quoted strings with `\`, `"` and `${` escaped. Quoted keys escape only `"`. Attribute order is unspecified and can differ between runs.

#### Scenario: Value containing interpolation syntax
- **WHEN** `environment` has `GREETING: 'hi ${USER}'`
- **THEN** the export contains `GREETING = "hi \${USER}";`

#### Scenario: Key needing quotes
- **WHEN** `environment` has a key `1BAD.KEY`
- **THEN** the attribute name is emitted quoted as `"1BAD.KEY"`

### Requirement: Flake structure and shell hook
The flake export SHALL set `description = "<name> development environment"`, take `nixpkgs` from `github:NixOS/nixpkgs/nixpkgs-unstable`, and define `devShells.<system>.default` for `x86_64-linux`, `aarch64-linux`, `x86_64-darwin` and `aarch64-darwin`. Both formats MUST include a `shellHook` that echoes `Entered <name> dev shell`, with `<name>` defaulting to `project`.

#### Scenario: Unnamed project
- **WHEN** `devy.yml` has no `name` and the user runs `devy export`
- **THEN** the flake description is `project development environment` and the shellHook echoes `Entered project dev shell`

### Requirement: Generated files end with stray closing braces
Both export formats SHALL currently emit extra closing braces at the end of the file, so neither output is valid Nix. `shell.nix` ends with `}}` (one stray `}`) after the `shellHook`. `flake.nix` ends with `};`, `}});`, `}};` and `}}`, three more `}` than the expression opens.

#### Scenario: shell.nix trailing braces
- **WHEN** the user runs `devy export --format shell`
- **THEN** the last line of `shell.nix` is `}}`

#### Scenario: flake.nix trailing braces
- **WHEN** the user runs `devy export`
- **THEN** `flake.nix` ends with the lines `          };`, `        }});`, `    }};` and `}}`
