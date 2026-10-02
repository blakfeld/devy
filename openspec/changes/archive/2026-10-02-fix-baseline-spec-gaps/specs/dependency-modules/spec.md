## MODIFIED Requirements

### Requirement: TypeScript module
The typescript module SHALL install Node as the node module does, then install the `typescript` npm package globally along with any packages listed in its `global_packages` extra key, which is part of its allowed extra keys. On every `devy up` it MUST run the same lockfile-based project install as the node module, stamped in `.devy_ts_local_stamp`. Its nixpkgs attribute follows the node module's, including versioned attributes.

#### Scenario: Fresh install
- **WHEN** `typescript` is not installed
- **THEN** devy installs Node and runs `npm install -g typescript`

#### Scenario: Global packages
- **WHEN** `typescript` is declared with `global_packages: [eslint, prettier]` and is not installed
- **THEN** devy runs `npm install -g typescript eslint prettier`
- **AND** `devy check` does not report `global_packages` as unrecognized

#### Scenario: Node already installed
- **WHEN** Node is already installed through the package manager but the `typescript` npm package is not
- **THEN** devy reports typescript as already installed and does not run `npm install -g typescript`

## ADDED Requirements

### Requirement: Versioned nix attributes
Modules with versioned nixpkgs packages SHALL map a dependency `version` to a versioned attribute, using the major (and, where nixpkgs names include it, minor) component of the version. This means full versions applied from `devy.lock` map to the same attribute as their short form. Each module SHALL map only an allowlist of versions that current nixpkgs carries:

| Module | Supported versions | Example | Without a version |
|---|---|---|---|
| node, typescript | 22, 24 | `22` or `22.11.0` → `nodejs_22` | `nodejs` |
| python | 3.11–3.14 | `3.12` or `3.12.4` → `python312` | `python3` |
| postgresql | 14–18 | `16` → `postgresql_16` | `postgresql` |
| mysql | 8.4 | `8.4` → `mysql84` | `mysql84` |
| java | 8, 11, 17, 21, 25 | `21` → `jdk21` | `jdk21` |
| dotnet | 6–10 | `8` → `dotnet-sdk_8` | `dotnet-sdk_8` |
| go | 1.26 | `1.26` → `go_1_26` | `go` |

Modules without a mapping, and versions outside the supported set, SHALL fall back to the unversioned attribute.

#### Scenario: Full locked version maps to major
- **WHEN** `node` has `version` `22.11.0` applied from `devy.lock` under nix
- **THEN** devy installs `nodejs_22`

#### Scenario: Version nixpkgs no longer carries
- **WHEN** `node` is declared with `version: "20"` under nix
- **THEN** devy installs `nodejs` and warns that the version is not supported by the nix backend

#### Scenario: Python minor version
- **WHEN** `python` is declared with `version: "3.12"` under nix
- **THEN** devy installs `python312`
