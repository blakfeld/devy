# shell-environment Specification

## Purpose
Defines how devy persists project environment variables and PATH entries into a shadowenv file, trusts it, and reads it back, so the environment activates automatically in the user's shell.

## Requirements
### Requirement: Shadowenv file location and header
devy SHALL write the project environment to `<project_root>/.shadowenv.d/500_devy.lisp`, creating the directory if needed, and the file MUST begin with `(provide "devy" "1.0.0")`.

#### Scenario: First write
- **WHEN** `devy up` has environment variables to write and `.shadowenv.d/` does not exist
- **THEN** devy creates `.shadowenv.d/500_devy.lisp` whose first line is `(provide "devy" "1.0.0")`

### Requirement: PATH prepends ordering
devy SHALL write one `(env/prepend-to-pathlist "PATH" "<dir>")` form per PATH entry, in reverse order, so that the first entry in devy's list ends up leftmost in `PATH` when activated. In `devy up`, the package manager's PATH entries SHALL come first in that list, before entries contributed by modules. The nix backend always contributes `<project_root>/.devy/nix-profile/bin`, and brew, apt and winget contribute none.

#### Scenario: Two PATH entries
- **WHEN** the PATH entries are `[<project_root>/.devy/nix-profile/bin, <project_root>/.venv/bin]`
- **THEN** the file prepends `<project_root>/.venv/bin` first and `<project_root>/.devy/nix-profile/bin` second, so the nix profile takes precedence

### Requirement: Environment variables sorted and escaped
devy SHALL write each variable as `(env/set "KEY" "VALUE")` sorted by key, escaping `\` and `"` with a backslash, encoding newline and carriage return as `\n` and `\r`, and removing NUL characters in keys, values and PATH entries.

#### Scenario: Value with quotes
- **WHEN** a variable `GREETING` has value `say "hi"`
- **THEN** the file contains `(env/set "GREETING" "say \"hi\"")`

#### Scenario: Sorted output
- **WHEN** variables `ZED` and `ALPHA` are written
- **THEN** the `ALPHA` line appears before the `ZED` line

### Requirement: User environment overrides module environment
When `devy up` merges environment variables, values from the `environment:` section of `devy.yml` SHALL take precedence over variables contributed by dependency modules.

#### Scenario: DATABASE_URL override
- **WHEN** the postgresql module contributes `DATABASE_URL` and `devy.yml` sets `DATABASE_URL: postgres://custom`
- **THEN** the shadowenv file contains `DATABASE_URL` set to `postgres://custom`

### Requirement: Shadowenv installation and trust
When there is environment content to write and `shadowenv` is not on `PATH`, `devy up` SHALL install `shadowenv` through the active package manager; after writing the file, devy SHALL run `shadowenv trust` in the project root, locating the binary on `PATH` or in the PATH entries just written, and MUST fail if trusting fails. The error SHALL be `Failed to configure environment variables: shadowenv trust failed` when `shadowenv trust` exits non-zero, or `Failed to configure environment variables: Failed to run shadowenv trust` when the binary cannot be started. If installing shadowenv fails, `devy up` SHALL fail with `Failed to install shadowenv`.

#### Scenario: Shadowenv installed via nix
- **WHEN** shadowenv is not on PATH and the backend is nix
- **THEN** devy installs `shadowenv` into the project nix profile and runs `<project_root>/.devy/nix-profile/bin/shadowenv trust`

### Requirement: Clearing a stale environment
When there are no variables or PATH entries but a devy shadowenv file already exists, `devy up` SHALL rewrite it with only the `provide` line, run `shadowenv trust`, and print `✓ Environment configuration cleared`. When neither content nor a file exists, devy MUST NOT create the file. Because the nix backend always contributes a PATH entry, these cases occur only with brew, apt or winget.

When clearing, devy SHALL NOT install shadowenv if it is missing. It still runs `shadowenv trust`, so `devy up` fails with `Failed to configure environment variables: Failed to run shadowenv trust` on a machine without shadowenv (current bug).

#### Scenario: Last env-contributing dependency removed
- **WHEN** the backend is brew, apt or winget, a previous `devy up` wrote variables, and `devy.yml` no longer produces any
- **THEN** `500_devy.lisp` contains only `(provide "devy" "1.0.0")`

#### Scenario: Nix always writes the file
- **WHEN** the backend is nix and there are no variables
- **THEN** devy writes the file with the nix profile PATH entry and prints `✓ Environment configured (0 variables)`

#### Scenario: Nothing to write
- **WHEN** the backend contributes no PATH entries (brew, apt or winget), `devy.yml` has no environment, no dependency contributes variables or PATH entries, and no file exists
- **THEN** devy does not create `.shadowenv.d/500_devy.lisp`

### Requirement: Activation hint
After writing a non-empty environment, `devy up` SHALL print `✓ Environment configured (<N> variable[s])`, where N counts only variables and not PATH entries, and an activation hint `eval "$(shadowenv hook <shell>)"`, where `<shell>` is the basename of `$SHELL` when it is sh, zsh, bash, fish, or powershell, and otherwise zsh (powershell on Windows).

#### Scenario: Bash user
- **WHEN** `$SHELL` is `/bin/bash`
- **THEN** devy prints `eval "$(shadowenv hook bash)"`

### Requirement: Reading the environment back
devy SHALL be able to parse the written file back into the set of variables (unescaping values) and PATH entries (in original order), returning nothing when the file does not exist; `devy check` and `devy status` use this to compare expected and written state.

#### Scenario: Round trip
- **WHEN** devy writes variables and PATH entries and then reads the file back
- **THEN** the variables and PATH entries equal those originally written
