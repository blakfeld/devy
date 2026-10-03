# Spec Delta

## MODIFIED Requirements

### Requirement: Built-in subcommands
The CLI SHALL provide the built-in subcommands `up`, `init`, `services`, `start`, `stop`, `restart`, `down`, `status`, `check`, `doctor`, `hook`, `pr`, `export`, `logs`, `ask` and `allow`, plus the hidden `_commands` and `_services` subcommands used by shell completion, and `--help` SHALL list the visible ones.

#### Scenario: Help lists core subcommands
- **WHEN** the user runs `devy --help`
- **THEN** the process exits 0
- **AND** the output lists `up`, `down`, `check`, `doctor`, `init`, `hook`, `status`, `logs`, `ask` and `allow`

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

### Requirement: Output conventions
The CLI SHALL print progress to stdout using consistent markers: a blank line plus bold text for section headers, `→` for steps in progress, `✓` for successes, a dimmed `○` for skipped work, and `·` for informational lines. Warnings SHALL go to stderr prefixed with a yellow `!`. `devy check` writes its failure summary to stderr as a red `✗` followed by `<n> issue(s) found`.

Text that comes from outside devy SHALL have C0 control characters (other than newline and tab in multi-line content), DEL and C1 control characters removed before it is printed, whether or not output is a terminal. The one exception is service log output written to a terminal, which SHALL keep SGR color sequences (`ESC [ <digits and ;> m`) while all other escape sequences are removed. Such text includes configuration and lock values, file contents, service logs, child-process error text and model replies.

#### Scenario: Warning goes to stderr
- **WHEN** a command emits a warning
- **THEN** the warning appears on stderr as `  ! <message>`
- **AND** nothing for that warning is written to stdout

#### Scenario: Escape sequence in a dependency name
- **WHEN** `devy.yml` sets `name: "app\e]52;c;ZWNobyBoaQ==\a"`
- **THEN** the `devy up` header contains no ESC or BEL characters
