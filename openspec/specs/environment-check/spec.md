# environment-check Specification

## Purpose
Defines the read-only commands `devy check` and `devy status`. They compare the declared environment in `devy.yml` against the machine's actual state, without installing, starting or writing anything.

## Requirements

### Requirement: Check is read-only
`devy check` SHALL NOT install dependencies, start or stop services, run hooks, or write `devy.lock` or the environment file.

#### Scenario: Check with missing dependencies
- **WHEN** a declared dependency is not installed and the user runs `devy check`
- **THEN** devy reports it as `not installed` and does not install it

### Requirement: Check output header
`devy check` SHALL begin by printing the header `devy check · <name>`, where `<name>` is the `devy.yml` `name` or `project` when unset.

#### Scenario: Unnamed project
- **WHEN** `devy.yml` has no `name` and the user runs `devy check`
- **THEN** the output begins with `devy check · project`

### Requirement: Dry run delegates to check
`devy up --dry-run` SHALL run exactly `devy check`, with the same output and exit codes, including exit 1 when issues are found. When combined with `--update` or `--bootstrap`, it SHALL first warn `--update has no effect with --dry-run; ignoring` or `--bootstrap has no effect with --dry-run; ignoring`.

#### Scenario: Dry run with issues
- **WHEN** a dependency is not installed and the user runs `devy up --dry-run`
- **THEN** the output matches `devy check`, nothing is installed, and the exit code is 1

#### Scenario: Dry run with --update
- **WHEN** the user runs `devy up --dry-run --update`
- **THEN** devy warns `--update has no effect with --dry-run; ignoring` and then runs the check

### Requirement: Hard configuration errors
`devy check` SHALL fail immediately with an `error:` message and exit 1 in these cases:
- The configuration cannot be loaded.
- The package manager setting is invalid for the platform.
- A dependency fails package-manager config validation.
- Service ports, resolved as `devy up` would resolve them from `devy.yml` and `devy.lock`, are out of range or conflict. Ports not yet assigned are excluded.

#### Scenario: No config
- **WHEN** `devy check` runs outside any devy project
- **THEN** it exits non-zero and stderr mentions `devy.yml`

#### Scenario: Port conflict on default ports
- **WHEN** the backend is brew and `devy.yml` lists `elasticsearch` and `opensearch` with no explicit ports (both default to 9200)
- **THEN** `devy check` fails with `port conflict: 'elasticsearch' and 'opensearch' both use port 9200`

#### Scenario: No conflict for ports up would assign
- **WHEN** the backend is nix, `devy.yml` lists `mysql` and `mariadb` with no explicit ports, and `devy.lock` has no entries for them
- **THEN** `devy check` reports no port conflict

#### Scenario: Locked ports conflict
- **WHEN** `devy.lock` records the same `assigned_port` for `redis` and `memcached` under nix
- **THEN** `devy check` fails with a port conflict naming both

### Requirement: Issue detection
`devy check` SHALL count one issue for each of the following:
- An unrecognized option key on a module that declares its allowed keys. A warning SHALL list the known keys, or say the module accepts no extra keys.
- An invalid dependency `shell`.
- A dependency that is not installed.
- An installed service that is stopped.
- A configured environment variable or PATH entry that is missing from the written environment file.

A service that is not installed SHALL count as one issue, not two. Module configuration warnings SHALL be printed but SHALL NOT count as issues. These also include:
- an explicit port that the backend cannot apply
- a `devy.yml` `version` that the nix backend cannot honor

#### Scenario: Unrecognized key
- **WHEN** `redis` is configured with `prot: 6380`
- **THEN** `devy check` warns `redis: unrecognized config key `prot` — known keys: port` and counts one issue

#### Scenario: Environment not yet written
- **WHEN** `environment` declares two variables and `devy up` has never run
- **THEN** each variable is reported as `missing` and counted as an issue

#### Scenario: Uninstalled service
- **WHEN** a service dependency is not installed
- **THEN** exactly one issue is counted for it

#### Scenario: TypeScript global packages accepted
- **WHEN** `typescript` is configured with `global_packages: [eslint]`
- **THEN** `devy check` reports no unrecognized key for it

#### Scenario: Unhonored nix version is a warning
- **WHEN** the backend is nix and `jq` is declared with `version: "1.6"` and no versioned nix attribute exists for it
- **THEN** `devy check` warns that the version is ignored by nix and does not count an issue for it

#### Scenario: Changed environment value not detected
- **WHEN** `devy up` wrote `LOG_LEVEL=debug` and `devy.yml` now sets `LOG_LEVEL: info`
- **THEN** `devy check` reports `LOG_LEVEL` as `configured` and counts no issue for it

### Requirement: Check result
When no issues are found, `devy check` SHALL print `✓ all checks passed` and exit 0. Otherwise it SHALL print `✗  N issue(s) found` to stderr and exit 1, without an additional `error:` line. This silent exit applies only to counted issues. Hard configuration errors SHALL still print `error: …`.

#### Scenario: Empty project passes
- **WHEN** `devy.yml` has `dependencies: []` and no environment
- **THEN** `devy check` exits 0

#### Scenario: Issues found
- **WHEN** two issues are detected
- **THEN** stderr shows `2 issues found` and the exit code is 1

### Requirement: Status report
`devy status` SHALL print the header `devy status · <name>`. It SHALL then print a dependency table showing installed state and service running state, and an environment table showing each configured variable's written value, `(not set)` if it is missing from the environment file, or `(not configured)` if no environment file exists. Every written value SHALL be shown on one line with control characters removed and then passed through the ai-assist value redaction: a value whose key matches the ai-assist redaction key rule SHALL be shown as `<redacted>`, and in any other value the credential-looking substrings that rule recognizes (URL userinfo passwords, PEM blocks, well-known token prefixes, secret-named assignments) SHALL be replaced by `<redacted>`. When `devy status --json` reports environment values, and the `cmd` of each project command, it SHALL apply the same redaction (the full ai-assist text redaction for commands), after first removing control and invisible characters (C0, DEL, C1, zero-width and bidirectional formatting characters) from the value, so such a character cannot split a credential past the redaction patterns; the removed characters are therefore not reported. When modules contribute PATH entries, it SHALL also print a PATH table that marks each entry `✓` if written, `(not set)` if missing, or `(not configured)` if no environment file exists. As in `devy check`, the package manager's own PATH entry is not listed. Missing or stopped items SHALL NOT make `devy status` fail. It SHALL exit 0 unless the configuration, package manager or a state query fails, and it SHALL NOT write any files.

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
