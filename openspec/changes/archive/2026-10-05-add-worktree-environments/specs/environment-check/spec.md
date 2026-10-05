# Spec Delta

## MODIFIED Requirements

### Requirement: Status report
`devy status` SHALL print the header `devy status · <name>`. When the project is in a linked git worktree, it SHALL print `worktree of <main checkout project root>` on the next line, or `worktree (no main checkout)` for a worktree of a bare repository. It SHALL then print a dependency table showing installed state and service running state, and an environment table showing each configured variable's written value, `(not set)` if it is missing from the environment file, or `(not configured)` if no environment file exists. Every written value SHALL be shown on one line with control characters removed and then passed through the ai-assist value redaction: a value whose key matches the ai-assist redaction key rule SHALL be shown as `<redacted>`, and in any other value the credential-looking substrings that rule recognizes (URL userinfo passwords, PEM blocks, well-known token prefixes, secret-named assignments) SHALL be replaced by `<redacted>`. When `devy status --json` reports environment values, and the `cmd` of each project command, it SHALL apply the same redaction (the full ai-assist text redaction for commands), after first removing control and invisible characters (C0, DEL, C1, zero-width and bidirectional formatting characters) from the value, so such a character cannot split a credential past the redaction patterns; the removed characters are therefore not reported. When modules contribute PATH entries, it SHALL also print a PATH table that marks each entry `✓` if written, `(not set)` if missing, or `(not configured)` if no environment file exists. As in `devy check`, the package manager's own PATH entry is not listed. Missing or stopped items SHALL NOT make `devy status` fail. It SHALL exit 0 unless the configuration, package manager or a state query fails, and it SHALL NOT write any files.

#### Scenario: Status before up
- **WHEN** `devy status` runs before any `devy up`
- **THEN** configured variables show `(not configured)` and the command exits 0

#### Scenario: Status after up
- **WHEN** `devy up` has written `LOG_LEVEL=debug`
- **THEN** `devy status` shows `LOG_LEVEL` with the value `debug`

#### Scenario: Secret values masked
- **WHEN** `devy up` has written `API_TOKEN=abc`
- **THEN** `devy status` shows `API_TOKEN` as `<redacted>` and does not print `abc`

#### Scenario: Credential inside a non-secret value masked
- **WHEN** `devy up` has written `DATABASE_URL=postgres://app:hunter2@localhost/app`
- **THEN** `devy status` shows `postgres://app:<redacted>@localhost/app` and does not print `hunter2`

#### Scenario: Invisible character does not defeat JSON redaction
- **WHEN** `devy up` has written `DATABASE_URL=postgres://app:hun\u200bter2@localhost/app`
- **THEN** `devy status --json` reports `postgres://app:<redacted>@localhost/app` and contains neither `hunter2` nor the zero-width character

#### Scenario: Status in a worktree
- **WHEN** the user runs `devy status` in a linked worktree of `/src/app`
- **THEN** the line after the header reads `worktree of /src/app`

#### Scenario: Status in the main checkout
- **WHEN** the user runs `devy status` in a checkout whose `.git` is a directory
- **THEN** no worktree line is printed
