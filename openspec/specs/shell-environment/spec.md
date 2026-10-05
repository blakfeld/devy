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
When there is environment content to write and `shadowenv` is not found outside the project (as defined in filesystem-safety), `devy up` SHALL install `shadowenv` through the active package manager. After writing the file, devy SHALL run `shadowenv trust` in the project root only when both of these hold, and MUST fail if trusting fails:
- the project is trusted under project-trust
- `.shadowenv.d/` is a real directory, not tracked by git, that contains no entry shadowenv could evaluate other than `500_devy.lisp`: every other entry is a regular file whose name does not end in `.lisp` (compared case-insensitively), such as the `.gitignore` and `.trust-<fingerprint>` files `shadowenv trust` writes itself and the `.error-<n>-<shell pid>` files shadowenv's hook writes while the directory is untrusted

devy SHALL locate the binary outside the project, or in the verified project nix profile. devy SHALL check the directory before writing `500_devy.lisp`. When `.shadowenv.d/` contains a directory, a symlink or another `*.lisp` file, devy SHALL fail with `.shadowenv.d contains files devy did not write (<names>); devy removed shadowenv's trust for this project; review and remove them, then run devy allow and devy up`. Before failing on such an entry, or whenever the managed-path check of filesystem-safety refuses a managed directory (for example a `.shadowenv.d/` tracked by git), whichever command ran it (`devy up`, `devy exec`, or a service started under nix by `devy start`/`restart`), devy SHALL remove shadowenv's trust files (`.shadowenv.d/.trust-*`, only when `.shadowenv.d/` is a real directory) so a signature from an earlier `devy up` stops loading the directory.

The first line of every `500_devy.lisp` devy writes SHALL be the Lisp comment `; devy-env <nonce>`, where `<nonce>` is 32 lowercase hex digits drawn at random for each write, and the file SHALL end with a newline. Before putting the file in place, devy SHALL write an exact copy of it to `<nonce>.lisp` in the per-user directory `$XDG_STATE_HOME/devy/shadowenv/` (default `~/.local/state/devy/shadowenv/`; `%LOCALAPPDATA%\devy\shadowenv\` on Windows), created with mode 0700 beside the trust store and refused, like it, inside the project; failing to write the copy SHALL fail the write. After replacing the file, devy SHALL remove the copy the replaced file's first line named, but only when that copy is byte for byte the replaced file, so a `500_devy.lisp` copied from another project (or naming its nonce) never deletes that project's copy. `devy allow --revoke` SHALL likewise remove the copy of the project's `500_devy.lisp` (only when it is that file's copy), so the shell hook stops accepting the file until the next `devy up`. The shell hook (shell-integration) and the trust check (project-trust) treat a `500_devy.lisp` as devy's only when it is a regular file identical to the copy its first line names: repository content cannot write that copy, nor learn a nonce generated on the user's machine, so a pulled or replaced `500_devy.lisp` never matches.

The errors SHALL be:
- `Failed to configure environment variables: shadowenv trust failed` when `shadowenv trust` exits non-zero
- `Failed to configure environment variables: Failed to run shadowenv trust` when the binary cannot be started
- `Failed to install shadowenv` when installing shadowenv fails; `devy up` SHALL fail with this error

#### Scenario: Shadowenv installed via nix
- **WHEN** shadowenv is not found outside the project and the backend is nix
- **THEN** devy installs `shadowenv` into the project nix profile, verifies the profile, and runs `<project_root>/.devy/nix-profile/bin/shadowenv trust`

#### Scenario: Committed lisp file
- **WHEN** the repo commits `.shadowenv.d/000_evil.lisp` and the user runs `devy up`
- **THEN** devy does not run `shadowenv trust`, writes no environment, and exits 1

#### Scenario: Environment file and its copy
- **WHEN** `devy up` writes `.shadowenv.d/500_devy.lisp`
- **THEN** its first line is `; devy-env <nonce>` and `~/.local/state/devy/shadowenv/<nonce>.lisp` holds the same bytes
- **AND** the next write uses a new nonce and removes the old copy

#### Scenario: Another project's copy is kept
- **WHEN** a project's `500_devy.lisp` names the nonce of another project's copy but differs from it, and `devy up` rewrites it
- **THEN** the other project's copy is left in place

#### Scenario: Managed-path refusal outside up
- **WHEN** an earlier `devy up` trusted `.shadowenv.d`, a pull makes `.devy` a symlink, and the user runs `devy exec make`
- **THEN** devy refuses, and `.shadowenv.d/.trust-*` is removed

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
