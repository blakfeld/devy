# environment-exec Specification

## Purpose
Lets users and agents run a single program in the project's environment (`devy.yml` variables, service host/port variables and PATH entries) with `devy exec`, without activating a shell.

## Requirements
### Requirement: Exec invocation
devy SHALL provide `devy exec [--] <program> [args…]`. It SHALL run `<program>` with the given arguments in the current working directory, within a devy project found the same way other commands find `devy.yml`. Arguments SHALL be passed to the program as-is, as separate arguments, without being interpreted by a shell. Flags after `<program>` SHALL belong to the program, not to devy. Running `devy exec` with no program SHALL be a usage error that exits 2.

#### Scenario: Arguments are not shell-interpreted
- **WHEN** the user runs `devy exec -- printf '%s\n' '$HOME; rm -rf x'`
- **THEN** the program prints `$HOME; rm -rf x` literally, and nothing is expanded or executed

#### Scenario: Program flags are not parsed by devy
- **WHEN** the user runs `devy exec cargo test --help`
- **THEN** `cargo test --help` runs and devy does not print its own help

#### Scenario: No program
- **WHEN** the user runs `devy exec` with no further arguments
- **THEN** devy reports a usage error and exits 2

#### Scenario: Outside a project
- **WHEN** the user runs `devy exec env` with no `devy.yml` in the current directory or its parents
- **THEN** stderr contains `error: devy.yml not found` and the process exits 1

### Requirement: Project environment for exec
The program SHALL inherit devy's own environment, overlaid with the project environment that `devy up` writes to the environment file:
- module environment variables
- `<SERVICE>_HOST` and `<SERVICE>_PORT` for each service
- `devy.yml` `environment`, which overrides module values for the same name

PATH SHALL be the module PATH entries, including the package manager's own entry, in the order `devy up` writes them, followed by the inherited PATH. A `PATH` set in devy.yml `environment` SHALL replace this value, as it does in the environment file. devy SHALL compute these values from `devy.yml` and `devy.lock` on each run, whether or not the environment file exists or is current. Ports SHALL be resolved read-only, so `devy exec` SHALL NOT assign ports or write `devy.lock`, the environment file or any other file. A service whose port is unassigned SHALL get `<SERVICE>_HOST` but no `<SERVICE>_PORT`. The program SHALL be looked up on the computed PATH.

#### Scenario: Locked port exported
- **WHEN** `devy.lock` assigns redis port 52113 and the user runs `devy exec env`
- **THEN** the output contains `REDIS_HOST=127.0.0.1` and `REDIS_PORT=52113`

#### Scenario: Config environment wins
- **WHEN** a module sets `DATABASE_URL` and `devy.yml` `environment` also sets `DATABASE_URL`
- **THEN** the program sees the `devy.yml` value

#### Scenario: Project-local binary found
- **WHEN** the project uses the nix profile and `jq` is installed only in `.devy/nix-profile/bin`, and the user runs `devy exec jq --version` from a shell without shadowenv
- **THEN** the project's `jq` runs

#### Scenario: Works before the environment file exists
- **WHEN** `devy.yml` sets `LOG_LEVEL=debug` and `.shadowenv.d` does not exist
- **THEN** `devy exec env` prints `LOG_LEVEL=debug` and no file is created

### Requirement: Exec stdio and exit status
The program SHALL inherit devy's stdin, stdout and stderr. devy SHALL print nothing to stdout itself. When the program exits, devy SHALL exit with the program's exit code and SHALL NOT print an `error:` line. When the program is terminated by a signal, devy SHALL exit 1. When the program cannot be started, for example because it is not found on the computed PATH or the file found there is not executable, devy SHALL print `error: …` naming the program and exit 1. devy's own warnings MAY appear on stderr.

#### Scenario: Exit code passes through
- **WHEN** the user runs `devy exec sh -c 'exit 3'`
- **THEN** devy exits 3 and stderr contains no `error:` line

#### Scenario: Program not found
- **WHEN** the user runs `devy exec no-such-program`
- **THEN** stderr contains `error:` and `no-such-program`, and the process exits 1

#### Scenario: Output is the program's only
- **WHEN** the user runs `devy exec echo hi`
- **THEN** stdout is exactly `hi` followed by a newline
