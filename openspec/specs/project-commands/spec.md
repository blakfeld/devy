# project-commands Specification

## Purpose
Running the project-defined commands from `devy.yml` (`devy <name> [args…]`) and lifecycle hooks through a restricted set of shells, plus the hidden `_commands` listing that shell completions use.

## Requirements

### Requirement: Unknown subcommands dispatch to project commands
The system SHALL treat any subcommand that is not a built-in as the name of a command in the `commands:` map of the nearest `devy.yml`. Built-in subcommands MUST take precedence over a user command with the same name.

#### Scenario: Running a defined command
- **WHEN** `devy.yml` defines `commands: { dev: "npm run dev" }` and the user runs `devy dev`
- **THEN** devy prints `→ Running 'dev'` and runs `npm run dev` through the command's shell

#### Scenario: Built-in name shadows a user command
- **WHEN** `devy.yml` defines a command named `status` and the user runs `devy status`
- **THEN** the built-in `status` command runs, not the user command

### Requirement: Unknown command errors list available commands
The system SHALL fail with exit status 1 when the requested command is not defined. The error MUST list the defined command names in sorted order, or state that no commands are defined.

#### Scenario: Command missing with others defined
- **WHEN** `devy.yml` defines commands `test` and `build` and the user runs `devy deploy`
- **THEN** devy fails with `Unknown command 'deploy'. Available: build, test`

#### Scenario: No commands defined
- **WHEN** `devy.yml` has no `commands:` and the user runs `devy deploy`
- **THEN** devy fails with `Unknown command 'deploy'. No commands are defined.`

### Requirement: Command forms and defaults
A command SHALL be either a string or a map with a required `cmd` and optional `cwd` and `shell`. Other keys in the map are silently ignored, so a misspelled key such as `sehll` leaves the default shell in effect. `shell` MUST default to `sh`, or `cmd` on Windows. When `cwd` is set, the command runs in that directory, and a relative `cwd` resolves against the process's current directory. Without `cwd`, the command runs in the current directory.

#### Scenario: Configured command with cwd and shell
- **WHEN** a command is `{ cmd: "bundle exec rails db:migrate", cwd: ./api, shell: bash }`
- **THEN** devy runs `bash -c "bundle exec rails db:migrate"` with `./api` as its working directory

#### Scenario: cmd shell uses /c
- **WHEN** a command's shell is `cmd`
- **THEN** devy invokes `cmd /c <command>`

### Requirement: Shell allowlist
The system MUST only run commands, hooks and `after_install` scripts through one of the bare shell names `sh`, `bash`, `zsh`, `fish`, `cmd` or `powershell`. Any shell value containing `/` or `\` MUST be rejected as a path, even if it points to an allowed shell.

#### Scenario: Disallowed shell
- **WHEN** a command sets `shell: python`
- **THEN** devy fails with `'<name>': invalid shell` and a message that `python` is not in the allowed list, without spawning anything

#### Scenario: Path to an allowed shell
- **WHEN** a command sets `shell: /bin/bash`
- **THEN** devy rejects it, saying the shell must be a bare name, not a path

### Requirement: Extra arguments are shell-quoted and appended
The system SHALL append arguments given after the command name to the command string, each wrapped in POSIX single quotes with embedded `'` escaped as `'\''`. For the `cmd` and `powershell` shells, extra arguments MUST be ignored with the warning `extra args are not supported with shell '<shell>' — args ignored`.

#### Scenario: Arguments with spaces and quotes
- **WHEN** `test: "cargo test"` is run as `devy test my filter "it's"`
- **THEN** the executed command is `cargo test 'my' 'filter' 'it'\''s'`

#### Scenario: Arguments with cmd shell
- **WHEN** a command uses `shell: cmd` and is run with extra arguments
- **THEN** devy warns that extra args are ignored and runs the command unchanged

### Requirement: Command failure reporting
The system SHALL exit with status 1 when the command cannot be spawned or exits non-zero. A spawn failure MUST report `Failed to spawn '<name>' via <shell>`. A non-zero exit MUST report `'<name>' command "<cmd>" failed with exit status <code> — check the output above for details`, using -1 when the child was terminated by a signal. `<cmd>` is the final command string, including appended arguments, shown as an escaped double-quoted literal (Rust debug formatting, so embedded `"` and `\` are backslash-escaped). For hooks, `<name>` is the hook tag, which is already quoted, so these messages show doubled quotes, for example `''before_up'' command "false" failed…` or `''before_up' (1/2)' command …`; the same applies to the spawn-failure and invalid-shell errors. The child's exit code is not passed through.

#### Scenario: Command exits non-zero
- **WHEN** a command `lint: "exit 3"` is run
- **THEN** devy prints an error naming `'lint'` and exit status 3, and devy itself exits 1

### Requirement: Hook execution
The system SHALL run a hook (`before_up`, `after_up`, `before_down`, `after_down`) as a single command or an ordered list of commands, using the same command forms, shell rules and working-directory rules as project commands. Each command MUST print `→ Running hook <tag>`, then `✓ Hook <tag> succeeded`. The tag is `'<label>'` for a single command and `'<label>' (i/N)` for a list. The first failing command MUST stop the hook and fail the surrounding devy command.

#### Scenario: List hook stops on first failure
- **WHEN** `before_up` is `["false", "echo second"]`
- **THEN** the first command fails, `echo second` is not run, and `devy up` fails with `''before_up' (1/2)' command "false" failed with exit status 1 — check the output above for details`

#### Scenario: Single successful hook
- **WHEN** `after_up` is `"true"`
- **THEN** devy prints `Running hook 'after_up'` and `Hook 'after_up' succeeded`

### Requirement: Hidden command listing for completions
The system SHALL provide a hidden `_commands` subcommand that prints each name in `commands:` once per line, sorted. It MUST exit 0 with no output when `devy.yml` is missing or invalid, so shell completions never print errors.

#### Scenario: Listing commands
- **WHEN** `devy.yml` defines commands `dev` and `build` and the user runs `devy _commands`
- **THEN** stdout is `build` then `dev`, one per line, and the exit status is 0

#### Scenario: No devy.yml
- **WHEN** `devy _commands` runs in a directory with no `devy.yml`
- **THEN** stdout is empty and the exit status is 0
