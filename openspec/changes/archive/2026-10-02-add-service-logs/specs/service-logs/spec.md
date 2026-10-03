# Spec Delta

## Purpose

Lets users read and follow the logs of any service declared in `devy.yml` with one command, regardless of which backend (nix, brew, apt, winget or docker) runs the service.

## ADDED Requirements

### Requirement: Logs command
`devy logs [<name>]` SHALL print recent log output for service dependencies declared in `devy.yml` and exit 0. `<name>` SHALL resolve by exact or canonical name, the same way as `devy start`. When `<name>` is not declared, devy MUST fail with `'<name>' not found in devy.yml dependencies`. When it is declared but is not a service, devy MUST fail with `'<name>' is not a service`. Showing logs MUST NOT start, stop or modify any service, `devy.lock` or the shadowenv file.

#### Scenario: Logs for one service
- **WHEN** `devy.yml` declares `redis`, redis has written log output, and the user runs `devy logs redis`
- **THEN** devy prints the most recent lines of redis's log and exits 0

#### Scenario: Alias resolves to the same service
- **WHEN** `devy.yml` declares `postgresql` and the user runs `devy logs postgres`
- **THEN** devy shows the postgresql service's logs

#### Scenario: Not a service
- **WHEN** `devy.yml` declares `node` and the user runs `devy logs node`
- **THEN** devy prints `error: 'node' is not a service` and exits 1

#### Scenario: Undeclared name
- **WHEN** the user runs `devy logs nosuch`
- **THEN** devy prints `error: 'nosuch' not found in devy.yml dependencies` and exits 1

### Requirement: Line count
`devy logs` SHALL show at most the last 100 lines per service by default. `-n <N>` or `--lines <N>` SHALL change this to the last `N` lines. A value that is not a positive integer MUST be a usage error with exit status 2.

#### Scenario: Custom line count
- **WHEN** a service's log has 500 lines and the user runs `devy logs redis -n 20`
- **THEN** devy prints exactly the last 20 lines

#### Scenario: Short log
- **WHEN** a service's log has 5 lines and the user runs `devy logs redis`
- **THEN** devy prints all 5 lines

#### Scenario: Invalid line count
- **WHEN** the user runs `devy logs redis -n zero`
- **THEN** devy reports a usage error and exits 2

### Requirement: Following logs
With `-f` or `--follow`, devy SHALL print the requested trailing lines and then keep streaming new log output as it is written, until the user interrupts it (Ctrl-C). An interrupt during follow SHALL end the command with exit status 0. Following a service that is not running SHALL still be allowed, so the user can watch it start from another terminal.

#### Scenario: New lines are streamed
- **WHEN** the user runs `devy logs redis -f` and redis then writes a new log line
- **THEN** the new line appears in devy's output without re-running the command

#### Scenario: Interrupting follow
- **WHEN** the user presses Ctrl-C while `devy logs redis -f` is running
- **THEN** devy stops streaming and exits 0

### Requirement: All services
When no `<name>` is given, `devy logs` SHALL show logs for every declared service, in declaration order. Without `--follow`, each service's lines SHALL appear under a section header with the service name. With `--follow`, devy SHALL stream all services together and prefix every line with `<name> | `. A service with no log output SHALL be reported as an informational line and SHALL NOT fail the command. When no services are declared, devy SHALL print `No services defined.` and exit 0.

#### Scenario: Sections per service
- **WHEN** `devy.yml` declares `redis` and `postgres`, and the user runs `devy logs`
- **THEN** devy prints a `redis` header followed by redis's lines, then a `postgres` header followed by postgres's lines

#### Scenario: Interleaved follow
- **WHEN** the user runs `devy logs -f` with `redis` and `postgres` declared
- **THEN** each streamed line starts with `redis | ` or `postgres | `, matching its source

#### Scenario: No services
- **WHEN** `devy.yml` declares only non-service dependencies and the user runs `devy logs`
- **THEN** devy prints `No services defined.` and exits 0

### Requirement: Log sources per backend
devy SHALL read logs from the place where the service's backend sends them:
- **nix on macOS:** the launchd agent's log file, `$TMPDIR/devy-<name>.log`.
- **nix on Linux:** the user journal for unit `devy-<name>.service`, via `journalctl --user`.
- **brew:** the log file paths that `brew services info --json <name>` reports. When it reports both standard output and standard error files, devy SHALL show both.
- **apt:** the system journal for the service's unit, via `journalctl -u <name>`. devy MUST NOT run `sudo` to read logs.
- **docker-managed services:** `<container_cli> logs` for the container `devy-<project>-<service>`.

`<name>` is the backend service name defined in service-management.

#### Scenario: Nix on macOS
- **WHEN** the package manager is nix on macOS and the user runs `devy logs redis`
- **THEN** devy shows the tail of `$TMPDIR/devy-redis.log`

#### Scenario: Nix on Linux
- **WHEN** the package manager is nix on Linux and the user runs `devy logs redis`
- **THEN** devy shows journal entries for the user unit `devy-redis.service`

#### Scenario: Apt journal not readable
- **WHEN** the package manager is apt and the user is not allowed to read the system journal for the unit
- **THEN** devy reports that the journal could not be read and suggests running `sudo journalctl -u <name>` or adding the user to the `systemd-journal` group, without running sudo itself

#### Scenario: Docker-managed service
- **WHEN** `redis` is docker-managed in project `myapp` with `container_cli: podman`, and the user runs `devy logs redis -n 50`
- **THEN** devy shows the last 50 lines of `podman logs` for container `devy-myapp-redis`

### Requirement: Missing or unavailable logs
When a supported backend has no log output for a service yet (the log file does not exist, or the journal has no entries), devy SHALL print `· No logs yet for <name>` and SHALL NOT fail. For the nix-on-macOS backend, that message SHALL include the expected log path. When the package manager is winget, devy MUST fail with `Logs are not available for winget-managed services — check Windows Event Viewer or the service's own log directory`. When a required log tool (`journalctl`, `brew` or the container CLI) cannot be run, devy MUST fail with an error that names that tool.

#### Scenario: Service never started
- **WHEN** the package manager is nix on macOS and `$TMPDIR/devy-redis.log` does not exist
- **THEN** devy prints `· No logs yet for redis (expected at <path>)` and exits 0

#### Scenario: Winget
- **WHEN** the package manager is winget and the user runs `devy logs mysql`
- **THEN** devy prints `error: Logs are not available for winget-managed services — check Windows Event Viewer or the service's own log directory` and exits 1

### Requirement: Additional service log files
For a service whose built-in module writes its own log files under `.devy/data/<canonical-name>/` (nginx, Elasticsearch, OpenSearch, Kafka and RabbitMQ under nix), devy SHALL print, after the primary log output and only when not following, one `· also see <path>` line for each of those files or directories that exists. devy MUST NOT print their contents.

#### Scenario: Nginx error log hint
- **WHEN** the package manager is nix, `nginx` is declared, `.devy/data/nginx/error.log` exists, and the user runs `devy logs nginx`
- **THEN** after the primary log output devy prints `· also see <project>/.devy/data/nginx/error.log`
