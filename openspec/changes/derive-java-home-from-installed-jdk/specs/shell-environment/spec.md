# Spec Delta

## MODIFIED Requirements

### Requirement: PATH prepends ordering
devy SHALL write one `(env/prepend-to-pathlist "PATH" "<dir>")` form per PATH entry, in reverse order, so that the first entry in devy's list ends up leftmost in `PATH` when activated. In `devy up`, the package manager's PATH entries SHALL come first in that list, before entries contributed by modules. The nix backend always contributes `<project_root>/.devy/nix-profile/bin`; brew contributes `<brew prefix>/opt/<formula>/bin` for each installed formula whose directory exists (see package-managers, "Homebrew formula bin directories on PATH"); apt and winget contribute none. When the same directory appears more than once, devy SHALL keep only its first occurrence. `devy check` and `devy status` SHALL compare the written file against this same list.

#### Scenario: Two PATH entries
- **WHEN** the PATH entries are `[<project_root>/.devy/nix-profile/bin, <project_root>/.venv/bin]`
- **THEN** the file prepends `<project_root>/.venv/bin` first and `<project_root>/.devy/nix-profile/bin` second, so the nix profile takes precedence

#### Scenario: Duplicate entry
- **WHEN** the same directory is contributed twice, for example by the Homebrew backend and by a module
- **THEN** the file contains a single prepend form for that directory, at the position of its first occurrence

### Requirement: Clearing a stale environment
When there are no variables or PATH entries but a devy shadowenv file already exists, `devy up` SHALL rewrite it with only the `provide` line, run `shadowenv trust`, and print `✓ Environment configuration cleared`. When neither content nor a file exists, devy MUST NOT create the file. Because the nix backend always contributes a PATH entry, these cases occur only with brew, apt or winget, and with brew only when no installed formula contributes an `opt/<formula>/bin` directory.

When clearing, devy SHALL NOT install shadowenv if it is missing. It still runs `shadowenv trust`, so `devy up` fails with `Failed to configure environment variables: Failed to run shadowenv trust` on a machine without shadowenv (current bug).

#### Scenario: Last env-contributing dependency removed
- **WHEN** the backend is brew, apt or winget, a previous `devy up` wrote variables, and `devy.yml` no longer produces any variables or PATH entries
- **THEN** `500_devy.lisp` contains only `(provide "devy" "1.0.0")`

#### Scenario: Nix always writes the file
- **WHEN** the backend is nix and there are no variables
- **THEN** devy writes the file with the nix profile PATH entry and prints `✓ Environment configured (0 variables)`

#### Scenario: Nothing to write
- **WHEN** the backend contributes no PATH entries (apt or winget, or brew with no formula `opt/<formula>/bin` directory), `devy.yml` has no environment, no dependency contributes variables or PATH entries, and no file exists
- **THEN** devy does not create `.shadowenv.d/500_devy.lisp`

#### Scenario: Brew formula writes the file
- **WHEN** the backend is brew, `devy.yml` declares only `jq` with no environment, and `<brew prefix>/opt/jq/bin` exists
- **THEN** devy writes `500_devy.lisp` with a prepend form for `<brew prefix>/opt/jq/bin` and prints `✓ Environment configured (0 variables)`
