## MODIFIED Requirements

### Requirement: Built-in subcommands
The CLI SHALL provide the built-in subcommands `up`, `init`, `services`, `start`, `stop`, `restart`, `down`, `status`, `check`, `doctor`, `hook`, `pr`, `export`, `logs` and `ask`, plus the hidden `_commands` and `_services` subcommands used by shell completion, and `--help` SHALL list the visible ones.

#### Scenario: Help lists core subcommands
- **WHEN** the user runs `devy --help`
- **THEN** the process exits 0
- **AND** the output lists `up`, `down`, `check`, `doctor`, `init`, `hook`, `status`, `logs` and `ask`

#### Scenario: Hidden subcommand is not advertised
- **WHEN** the user runs `devy --help`
- **THEN** neither `_commands` nor `_services` is listed

#### Scenario: Service names for completion
- **WHEN** `devy.yml` declares `redis`, `node` and `postgres`, and the user runs `devy _services`
- **THEN** devy prints `redis` and `postgres`, one per line in declaration order, and exits 0
- **AND** when no `devy.yml` can be loaded it prints nothing and exits 0

#### Scenario: No version flag
- **WHEN** the user runs `devy --version`
- **THEN** devy reports a usage error and exits 2, because no `--version` flag is defined

#### Scenario: No subcommand
- **WHEN** the user runs `devy` with no arguments
- **THEN** devy prints usage and exits 2

#### Scenario: Built-in logs shadows project command
- **WHEN** `devy.yml` defines a project command named `logs` and the user runs `devy logs`
- **THEN** the built-in `logs` subcommand runs
