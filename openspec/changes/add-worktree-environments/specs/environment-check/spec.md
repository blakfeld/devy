# Spec Delta

## MODIFIED Requirements

### Requirement: Status report
`devy status` SHALL print the header `devy status · <name>`. When the project is in a linked git worktree, it SHALL print `worktree of <main checkout project root>` on the next line, or `worktree (no main checkout)` for a worktree of a bare repository. It SHALL then print:
- a dependency table showing installed state and service running state
- an environment table showing each configured variable's written value, `(not set)` if it is missing from the environment file, or `(not configured)` if no environment file exists
- when modules contribute PATH entries, a PATH table that marks each entry `✓` if written, `(not set)` if missing, or `(not configured)` if no environment file exists. As in `devy check`, the package manager's own PATH entry is not listed.

Missing or stopped items SHALL NOT make `devy status` fail. It SHALL exit 0 unless the configuration, package manager or a state query fails, and it SHALL NOT write any files.

#### Scenario: Status before up
- **WHEN** `devy status` runs before any `devy up`
- **THEN** configured variables show `(not configured)` and the command exits 0

#### Scenario: Status after up
- **WHEN** `devy up` has written `LOG_LEVEL=debug`
- **THEN** `devy status` shows `LOG_LEVEL` with the value `debug`

#### Scenario: Status in a worktree
- **WHEN** the user runs `devy status` in a linked worktree of `/src/app`
- **THEN** the line after the header reads `worktree of /src/app`

#### Scenario: Status in the main checkout
- **WHEN** the user runs `devy status` in a checkout whose `.git` is a directory
- **THEN** no worktree line is printed
