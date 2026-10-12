# cli Specification

## Purpose
Defines the `devy` command-line surface: the built-in subcommands, how user-defined commands are dispatched, the output conventions, and how errors map to exit codes.

## Requirements

### Requirement: Built-in subcommands
The CLI SHALL provide the built-in subcommands `up`, `init`, `services`, `start`, `stop`, `restart`, `down`, `status`, `check`, `doctor`, `hook`, `pr`, `export`, `logs`, `ask`, `exec`, `agent-setup` and `prune`, plus the hidden `_commands` and `_services` subcommands used by shell completion, and `--help` SHALL list the visible ones.

#### Scenario: Help lists core subcommands
- **WHEN** the user runs `devy --help`
- **THEN** the process exits 0
- **AND** the output lists `up`, `down`, `check`, `doctor`, `init`, `hook`, `status`, `logs`, `ask`, `exec`, `agent-setup` and `prune`
- **AND** the output does not list `allow`

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

#### Scenario: Built-in exec shadows project command
- **WHEN** `devy.yml` defines a project command named `exec` and the user runs `devy exec env`
- **THEN** the built-in `exec` subcommand runs `env` with the project environment

#### Scenario: Prune runs without a devy.yml
- **WHEN** the user runs `devy prune` in a directory with no `devy.yml` above it
- **THEN** devy runs the prune command rather than failing to find a config

### Requirement: Unknown subcommands dispatch to project commands
The CLI SHALL treat any subcommand that is not built in as the name of a project command from `devy.yml`, passing any remaining arguments through to it, and built-in names SHALL take precedence over project commands of the same name.

#### Scenario: Project command dispatch
- **WHEN** `devy.yml` defines `commands.dev` and the user runs `devy dev --port 3000`
- **THEN** devy runs the `dev` project command with the extra arguments `--port 3000`

#### Scenario: Built-in shadows project command
- **WHEN** `devy.yml` defines a project command named `status` and the user runs `devy status`
- **THEN** the built-in `status` subcommand runs

#### Scenario: Misspelled built-in
- **WHEN** the user runs `devy stauts` and no project command has that name
- **THEN** devy looks it up as a project command and fails with `Unknown command 'stauts'…`, exiting 1, rather than offering a clap suggestion

### Requirement: Dry-run delegates to check
`devy up --dry-run` SHALL behave exactly like `devy check`, and when combined with `--update` or `--bootstrap` it SHALL warn that the flag has no effect and ignore it.

#### Scenario: Dry run ignores update flag
- **WHEN** the user runs `devy up --dry-run --update`
- **THEN** devy warns `--update has no effect with --dry-run; ignoring`
- **AND** runs the `check` behavior without installing anything or writing `devy.lock`

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

### Requirement: Error reporting and exit codes
On failure the CLI SHALL print `error: <message chain>` to stderr, with context and causes joined by `: `, and exit with status 1. Successful runs SHALL exit 0, and command-line usage errors SHALL exit 2. When `devy check` finds issues (unrecognized keys, invalid shells, missing installs or environment drift), it SHALL print its own summary and exit 1 without the additional `error:` line. Configuration errors that `check` hits, such as port conflicts, multi-key dependency entries or failed config validation, SHALL still print `error: …` and exit 1.

#### Scenario: Generic failure
- **WHEN** a command fails, for example because no `devy.yml` is found
- **THEN** stderr contains `error: devy.yml not found — are you inside a devy project?`
- **AND** the process exits 1

#### Scenario: Silent failure from check
- **WHEN** `devy check` finds issues
- **THEN** it prints its own issue summary and exits 1 without printing an `error:` line

#### Scenario: Configuration error from check
- **WHEN** `devy check` detects a port conflict
- **THEN** stderr contains `error: …` describing the conflict and the process exits 1

#### Scenario: Usage error
- **WHEN** the user passes an unknown flag to a built-in subcommand
- **THEN** the process exits 2

### Requirement: Child process failures map to exit 1
When a project command, hook or `after_install` command exits non-zero, devy SHALL fail with an error describing the command and its exit status and SHALL exit 1, rather than passing through the child's exit code. `devy exec` is the exception: it SHALL pass the program's exit code through without an `error:` line, as defined by the environment-exec capability.

#### Scenario: Project command exits with 3
- **WHEN** a project command exits with status 3
- **THEN** devy reports the failure, including the exit status
- **AND** devy itself exits 1

#### Scenario: Exec passes the exit code through
- **WHEN** `devy exec sh -c 'exit 3'` runs
- **THEN** devy exits 3 without printing `error:`
