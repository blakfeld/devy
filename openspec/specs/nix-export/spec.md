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
The system SHALL emit each `environment:` entry as an attribute of the mkShell, in key order, except a key named `packages` or `shellHook` (attributes the export writes itself), which SHALL be left out as a comment with a warning. `devy export` SHALL NOT check any trust record or comment entries out: like `devy up`, the exported shell applies the project's environment (`mkShell` treats some names, such as `preHook`, as code, and others, such as `BASH_ENV`, change how the shell runs), and reviewing `devy.yml` is the user's decision.

Keys that are not valid bare Nix identifiers MUST be quoted. Values MUST be emitted as Nix double-quoted strings with `\`, `"` and `${` escaped. Quoted keys use the same escapes.

#### Scenario: Value containing interpolation syntax
- **WHEN** `environment` has `GREETING: 'hi ${USER}'`
- **THEN** the export contains `GREETING = "hi \${USER}";`

#### Scenario: Key needing quotes
- **WHEN** `environment` has the key `rec`, a Nix keyword
- **THEN** the attribute name is emitted quoted as `"rec"`

#### Scenario: Untrusted project
- **WHEN** a freshly cloned project's `environment` has `preHook: "touch /tmp/p"` and `FOO: bar`
- **THEN** the export contains the attributes `preHook = "touch /tmp/p";` and `FOO = "bar";`, nothing is commented out, and no warning about allowing the project is printed

#### Scenario: Value that would end the comment
- **WHEN** an entry's value contains a newline followed by `shellHook = "touch /tmp/q";`
- **THEN** the quotes are escaped, so the whole value stays inside its Nix string and adds no `shellHook` attribute

#### Scenario: Trusted project
- **WHEN** `environment` has `BASH_ENV: /tmp/env.sh`
- **THEN** the export contains `BASH_ENV = "/tmp/env.sh";` and no warning is printed

### Requirement: Flake structure and shell hook
The flake export SHALL set `description = "<name> development environment"`, take `nixpkgs` from `github:NixOS/nixpkgs/nixpkgs-unstable`, and define `devShells.<system>.default` for `x86_64-linux`, `aarch64-linux`, `x86_64-darwin` and `aarch64-darwin`. Both formats MUST include a `shellHook` that echoes `Entered <name> dev shell`, with `<name>` defaulting to `project`.

`<name>` SHALL be escaped for every context it appears in:
- in a Nix `"…"` string, `\`, `"` and `${` are escaped
- in the `''…''` shellHook string, `''` and `${` are escaped
- inside the shellHook, the shell receives `<name>` as a single-quoted word, so `$`, backticks and `\` are not interpreted

Attribute names derived from configuration SHALL be escaped with the same Nix string rules.

#### Scenario: Unnamed project
- **WHEN** `devy.yml` has no `name` and the user runs `devy export`
- **THEN** the flake description is `project development environment` and the shellHook echoes `Entered project dev shell`

#### Scenario: Hostile project name
- **WHEN** `name` is `x"; ${builtins.abort "p"} $(touch /tmp/p) ''`
- **THEN** the exported file evaluates without error, entering the shell creates no `/tmp/p`, and the shellHook echoes the name literally

### Requirement: Generated files are valid Nix
Both export formats SHALL emit balanced braces, so each file parses as a Nix expression. `shell.nix` ends with a single `}` after the `shellHook`. `flake.nix` ends with the lines `          };`, `        });`, `    };` and `}`.

#### Scenario: shell.nix parses
- **WHEN** the user runs `devy export --format shell`
- **THEN** the last line of `shell.nix` is `}`, and `nix-instantiate --parse shell.nix` succeeds

#### Scenario: flake.nix parses
- **WHEN** the user runs `devy export`
- **THEN** `flake.nix` ends with the lines `          };`, `        });`, `    };` and `}`, and `nix-instantiate --parse flake.nix` succeeds

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

### Requirement: Insecure packages allowed in exports
When the export lists packages whose modules declare them insecure, the generated file SHALL also set an `allowInsecurePredicate` that permits exactly those packages by name, imported the same way as `allowUnfreePredicate`. When no listed package is insecure, the output MUST NOT contain an insecure predicate.

#### Scenario: Flake with Elasticsearch
- **WHEN** `dependencies` contains `elasticsearch` and the user runs `devy export`
- **THEN** `flake.nix` imports nixpkgs with an `allowUnfreePredicate` and an `allowInsecurePredicate` that each allow `elasticsearch` and no other package

#### Scenario: No insecure packages
- **WHEN** `dependencies` contains `mongodb` but not `elasticsearch`
- **THEN** the export contains no `allowInsecurePredicate`
