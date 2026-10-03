# Spec Delta

## MODIFIED Requirements

### Requirement: Built-in subcommands
The CLI SHALL provide these built-in subcommands, and `--help` SHALL list the visible ones:
- `up`, `init`, `services`, `start`, `stop`, `restart`, `down`, `status`, `check`, `hook`, `pr`, `export`, `snapshot` and `seed`
- a hidden `_commands` subcommand

`snapshot` SHALL take the actions `save`, `restore`, `list` and `delete`.

#### Scenario: Help lists core subcommands
- **WHEN** the user runs `devy --help`
- **THEN** the process exits 0
- **AND** the output lists `up`, `down`, `check`, `init`, `hook`, `status`, `snapshot` and `seed`

#### Scenario: Hidden subcommand is not advertised
- **WHEN** the user runs `devy --help`
- **THEN** `_commands` is not listed

#### Scenario: No version flag
- **WHEN** the user runs `devy --version`
- **THEN** devy reports a usage error and exits 2, because no `--version` flag is defined

#### Scenario: No subcommand
- **WHEN** the user runs `devy` with no arguments
- **THEN** devy prints usage and exits 2

#### Scenario: Snapshot without an action
- **WHEN** the user runs `devy snapshot`
- **THEN** devy prints usage for `snapshot` and exits 2
