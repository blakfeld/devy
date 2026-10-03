# Spec Delta

## MODIFIED Requirements

### Requirement: Built-in subcommands
The CLI SHALL provide the built-in subcommands `up`, `init`, `services`, `start`, `stop`, `restart`, `down`, `status`, `check`, `hook`, `pr`, `export` and `mcp`, plus a hidden `_commands` subcommand, and `--help` SHALL list the visible ones.

#### Scenario: Help lists core subcommands
- **WHEN** the user runs `devy --help`
- **THEN** the process exits 0
- **AND** the output lists `up`, `down`, `check`, `init`, `hook`, `status` and `mcp`

#### Scenario: Hidden subcommand is not advertised
- **WHEN** the user runs `devy --help`
- **THEN** `_commands` is not listed

#### Scenario: No version flag
- **WHEN** the user runs `devy --version`
- **THEN** devy reports a usage error and exits 2, because no `--version` flag is defined

#### Scenario: No subcommand
- **WHEN** the user runs `devy` with no arguments
- **THEN** devy prints usage and exits 2

#### Scenario: MCP flag
- **WHEN** the user runs `devy mcp --help`
- **THEN** the output describes the `--allow-mutations` flag
