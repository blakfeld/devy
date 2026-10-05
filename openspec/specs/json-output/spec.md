# json-output Specification

## Purpose
Gives `devy status`, `devy services` and `devy check` a versioned, machine-readable `--json` mode so scripts and coding agents can read project state without parsing tables.

## Requirements
### Requirement: JSON flag and stream rules
`devy status`, `devy services` and `devy check` SHALL accept `--json`. With `--json`:
- the command SHALL write exactly one JSON object to stdout, followed by a newline, and nothing else
- headers, tables and progress markers SHALL NOT be printed
- warnings SHALL still go to stderr as defined by the CLI output conventions, except check warnings, which `devy check --json` reports only in its `warnings` field
- output SHALL contain no ANSI color codes
- every document SHALL include an integer field `version` with the value `1`

Adding fields SHALL NOT change `version`. Removing or renaming a field, or changing its type or meaning, SHALL increment it.

#### Scenario: Stdout is a single JSON document
- **WHEN** the user runs `devy status --json` in a project with dependencies, services and environment
- **THEN** stdout parses as one JSON object containing `"version": 1`
- **AND** stdout contains no header text and no ANSI escape sequences

#### Scenario: Warnings stay on stderr
- **WHEN** `devy services --json` runs and emits a warning
- **THEN** the warning appears on stderr and stdout still parses as one JSON object

### Requirement: JSON errors and exit codes
With `--json`, exit codes SHALL match the command without `--json`. A failure that prevents the report from being built, such as a missing `devy.yml`, an invalid configuration or a failed state query, SHALL print `error: …` to stderr, print nothing to stdout, and exit 1. `devy check --json` SHALL print its document whether or not issues were found, SHALL NOT print the `✗ N issue(s) found` summary, and SHALL exit 1 when the document reports any issues.

#### Scenario: No devy.yml
- **WHEN** the user runs `devy status --json` outside a devy project
- **THEN** stdout is empty, stderr contains `error: devy.yml not found`, and the process exits 1

#### Scenario: Check with issues
- **WHEN** `devy check --json` finds two issues
- **THEN** stdout is a JSON document with `"passed": false` and two entries in `issues`
- **AND** stderr does not contain `issues found`
- **AND** the process exits 1

#### Scenario: Check passes
- **WHEN** `devy check --json` finds no issues
- **THEN** the document has `"passed": true` and an empty `issues` array, and the process exits 0

### Requirement: Services document
`devy services --json` SHALL print `{"version", "services": [...]}`, with one entry per declared service in declaration order. Each entry SHALL contain:
- `name`: the name as written in `devy.yml`
- `backend`: `"package"` or `"docker"`
- `running`: a boolean
- `host`: `"127.0.0.1"`
- `port`: the effective port as an integer, or `null` when none is resolved
- `port_source`: one of `"explicit"`, `"lock"`, `"default"`, `"unassigned"` or `null`

Ports SHALL be resolved as other read-only commands resolve them, and the command SHALL NOT write `devy.lock`. Plain `devy services` SHALL resolve ports the same way, so its running state reflects the locked port and it fails on the same port errors as `devy status`. When no services are declared, `services` SHALL be an empty array and the command SHALL exit 0.

#### Scenario: Locked port reported
- **WHEN** `devy.yml` declares `redis`, `devy.lock` assigns it port 52113, and redis is running
- **THEN** the entry for `redis` has `"backend": "package"`, `"running": true`, `"port": 52113` and `"port_source": "lock"`

#### Scenario: Docker-managed service
- **WHEN** `postgres` is docker-managed and its container is stopped
- **THEN** its entry has `"backend": "docker"` and `"running": false`

#### Scenario: Port not yet assigned
- **WHEN** a service's backend applies ports but `devy up` has not yet assigned one
- **THEN** its entry has `"port": null` and `"port_source": "unassigned"`

#### Scenario: No services
- **WHEN** `devy.yml` declares only non-service dependencies
- **THEN** the document is `{"version": 1, "services": []}` and the process exits 0

### Requirement: Status document
`devy status --json` SHALL print a document with these fields:
- `version`
- `project`: the `name` from `devy.yml`, or `null`
- `package_manager`: the detected package manager's name
- `dependencies`: one entry per dependency in declaration order, each with:
  - `name`
  - `installed` (boolean)
  - `version` (the version requested in `devy.yml`, or `null`)
  - `service` (boolean)
  - for services only: `backend`, `running`, `host`, `port` and `port_source`, with the same meaning as in the services document
- `environment`: an object mapping each variable configured in `devy.yml` `environment` to the value written in the environment file, or `null` when it is missing or no environment file exists
- `environment_written`: a boolean, true when the environment file exists
- `path`: the PATH entries `devy up` writes to the environment file, in order and with the package manager's own entry first, each with `entry` and `written` (boolean)
- `commands`: one entry per project command, sorted by name, each with `name`, `cmd` and `shell`

Like `devy status`, it SHALL exit 0 even when items are missing or stopped, and SHALL NOT write any files.

#### Scenario: Status before up
- **WHEN** `devy status --json` runs before any `devy up` and `devy.yml` sets `LOG_LEVEL`
- **THEN** `environment_written` is `false` and `environment.LOG_LEVEL` is `null`
- **AND** the process exits 0

#### Scenario: Commands listed
- **WHEN** `devy.yml` defines `test: cargo test` and `lint: cargo clippy`
- **THEN** `commands` is `[{"name": "lint", …}, {"name": "test", "cmd": "cargo test", "shell": "sh", …}]` on macOS and Linux

#### Scenario: Status is read-only
- **WHEN** `devy status --json` runs in a project without `devy.lock` or `.shadowenv.d`
- **THEN** neither is created

### Requirement: Check document
`devy check --json` SHALL perform the same checks as `devy check` and print `{"version", "passed", "issues", "warnings"}`, where `issues` and `warnings` are arrays of human-readable message strings. Each message is the text `devy check` would show for that finding, without color codes. `passed` SHALL be true exactly when `issues` is empty. Configuration errors that make `devy check` print `error: …`, such as port conflicts, SHALL keep that behavior under `--json`.

#### Scenario: Missing dependency
- **WHEN** `jq` is declared but not installed
- **THEN** `issues` contains a message naming `jq` and `passed` is `false`

#### Scenario: Warning does not fail
- **WHEN** check emits only a warning, for example an unhonored nix version
- **THEN** the warning appears in `warnings`, `passed` is `true` and the process exits 0

### Requirement: Secret redaction in JSON output
Environment values in JSON output SHALL be redacted with the same rules devy uses for AI requests:
- a variable whose name contains `KEY`, `SECRET`, `TOKEN`, `PASSWORD`, `PASSWD`, `CREDENTIAL` or `PRIVATE` (ignoring case) has its value replaced by `<redacted>`
- in any other value, URL passwords and well-known token prefixes are replaced

Redaction SHALL apply to `environment` values in the status document. Redaction is best-effort and SHALL NOT apply to human (non-JSON) output.

#### Scenario: Secret name redacted
- **WHEN** the environment file sets `STRIPE_SECRET_KEY=sk_live_abc`
- **THEN** `environment.STRIPE_SECRET_KEY` is `"<redacted>"` in `devy status --json`
- **AND** plain `devy status` still shows the written value

#### Scenario: URL password redacted
- **WHEN** `DATABASE_URL=postgres://app:hunter2@127.0.0.1:5432/app` is written
- **THEN** the JSON value keeps the user, host, port and path but not `hunter2`

#### Scenario: Ordinary value kept
- **WHEN** `LOG_LEVEL=debug` is written
- **THEN** `environment.LOG_LEVEL` is `"debug"`
