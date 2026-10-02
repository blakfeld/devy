# project-config Specification

## Purpose
Defines how devy finds and parses the project's `devy.yml`, the schema of that file, and how `devy init` creates a starter config.

## Requirements
### Requirement: Config discovery
devy SHALL find `devy.yml` by starting in the current directory and walking up through parent directories. It SHALL return the first `devy.yml` it finds, and SHALL stop searching after checking a directory that contains `.git` or that is the user's home directory. The directory containing the found `devy.yml` SHALL be the project root.

#### Scenario: Found in a parent directory
- **WHEN** the user runs a devy command in `repo/src/app` and `repo/devy.yml` exists, with no `.git` between them
- **THEN** devy uses `repo/devy.yml` and treats `repo` as the project root

#### Scenario: Config at the git root
- **WHEN** `devy.yml` and `.git` are both in the same directory
- **THEN** that `devy.yml` is found

#### Scenario: Search stops at the git root
- **WHEN** the repository root contains `.git` but no `devy.yml`, and a parent directory outside the repository contains `devy.yml`
- **THEN** devy fails with `devy.yml not found — are you inside a devy project?`

#### Scenario: Search stops at the home directory
- **WHEN** no `devy.yml` exists between the current directory and `$HOME`
- **THEN** devy does not look above `$HOME` and fails with the not-found error

### Requirement: Strict top-level schema
`devy.yml` SHALL accept only the top-level keys `name`, `dependencies`, `environment`, `commands`, `hooks` and `package_manager`. Any other top-level key SHALL be a parse error. Every key SHALL be optional. `name` SHALL default to `project`, and the collections SHALL default to empty.

#### Scenario: Misspelled top-level key
- **WHEN** `devy.yml` contains `dependecies:`
- **THEN** loading fails with `Failed to parse <path>` and a non-zero exit

#### Scenario: Malformed YAML
- **WHEN** `devy.yml` is not valid YAML
- **THEN** loading fails with `Failed to parse <path>`

#### Scenario: Minimal config
- **WHEN** `devy.yml` contains only `dependencies: []`
- **THEN** the config loads with the project name `project` and no environment, commands or hooks

### Requirement: Dependency entry forms
Each item in `dependencies` SHALL be one of: a bare string naming the dependency; a single-key map from the name to a configuration map; or a single-key map with a null value. A configuration map SHALL accept `version`, `tap`, `after_install` and `shell`, and SHALL keep every other key as a module-specific option (for example `port` or `cli_args`). A map item with more than one key SHALL be rejected.

#### Scenario: String form
- **WHEN** a dependency is written as `- redis`
- **THEN** it is a dependency named `redis` with no version and no options

#### Scenario: Map form with options
- **WHEN** a dependency is written as `- mysql: { version: "8.1", port: 3307 }`
- **THEN** it is a dependency named `mysql` with version `8.1` and the module option `port: 3307`

#### Scenario: Null configuration
- **WHEN** a dependency is written as `- mysql:` with no value
- **THEN** it is treated like the string form `- mysql`

#### Scenario: Multiple keys in one item
- **WHEN** a single list item is `{ redis: ~, mysql: ~ }`
- **THEN** devy fails with `dependency entry has multiple keys (redis, mysql); each dependency must be its own list item`, listing the keys in no guaranteed order

### Requirement: Environment map
`environment` SHALL be a map of string variable names to string values that devy exports into the project's shell environment.

#### Scenario: Environment variables declared
- **WHEN** `devy.yml` sets `environment: { LOG_LEVEL: debug }`
- **THEN** `LOG_LEVEL=debug` is part of the project environment that `devy up` writes

### Requirement: Command definitions
Each entry in `commands` SHALL be either a string command line or a map with a required `cmd` and optional `cwd` and `shell`. When `shell` is omitted it SHALL default to `sh`, or to `cmd` on Windows. Unknown keys inside a command map SHALL be silently ignored, not rejected. The same applies to configured hook entries.

#### Scenario: String command
- **WHEN** `commands.dev` is `"npm run dev"`
- **THEN** `devy dev` runs `npm run dev` with the default shell

#### Scenario: Configured command
- **WHEN** `commands.migrate` is `{ cmd: "rails db:migrate", cwd: ./api, shell: bash }`
- **THEN** `devy migrate` runs the command with `bash` in `./api`

#### Scenario: Misspelled key in a command map
- **WHEN** `commands.dev` is `{ cmd: "npm run dev", sehll: bash }`
- **THEN** `devy.yml` loads without error, the `sehll` key is ignored, and the command runs with the default shell

### Requirement: Hook definitions
`hooks` SHALL accept only the keys `before_up`, `after_up`, `before_down` and `after_down`. Each value SHALL be a single command (string or configured map) or a list mixing both forms. Any other key under `hooks` SHALL be a parse error.

#### Scenario: Misspelled hook key
- **WHEN** `hooks` contains `before_Up`
- **THEN** loading `devy.yml` fails with a parse error

#### Scenario: List of hook commands
- **WHEN** `hooks.before_down` is a list of a string and a `{cmd, shell}` map
- **THEN** both entries are accepted as hook commands, in order

### Requirement: Package manager setting
`package_manager` SHALL accept exactly `auto`, `nix`, `brew` or `apt` in lowercase, and SHALL default to `auto`. Any other value, including `winget`, SHALL be a parse error. How `auto` picks a backend, and the errors for a backend chosen on the wrong OS, are defined in the package-managers spec.

#### Scenario: Invalid package manager
- **WHEN** `package_manager: pacman` is set
- **THEN** loading `devy.yml` fails with a parse error

### Requirement: Init command
`devy init` SHALL write `devy.yml` in the current directory containing `name: my-project` and `dependencies: []`, and print `✓ wrote devy.yml`. If `devy.yml` already exists there, it SHALL fail with `devy.yml already exists. Use --force to overwrite.`, unless `--force` is given, in which case it SHALL overwrite the file. `init` SHALL NOT search parent directories.

#### Scenario: Fresh init
- **WHEN** the user runs `devy init` in a directory without `devy.yml`
- **THEN** `devy.yml` is created with a `dependencies` key and the command exits 0

#### Scenario: Existing file without force
- **WHEN** `devy.yml` already exists and the user runs `devy init`
- **THEN** the command exits non-zero, stderr mentions that the file already exists and `--force`, and the file is unchanged

#### Scenario: Existing file with force
- **WHEN** `devy.yml` already exists and the user runs `devy init --force`
- **THEN** the file is replaced with the starter content
