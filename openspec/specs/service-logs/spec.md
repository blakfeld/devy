# service-logs Specification

## Purpose
Lets users read and follow the logs of any service declared in `devy.yml` with one command, regardless of which backend (nix, brew, apt, winget or docker) runs the service.

## Requirements

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
- **nix on macOS:** the launchd agent's log file, `devy-<project>-<name>.log` in devy's private per-user directory under `$TMPDIR` (or `$XDG_RUNTIME_DIR`), as described in service-management.
- **nix on Linux:** the user journal for unit `devy-<project>-<name>.service`, via `journalctl --user`.
- **brew:** the log file paths that `brew services info --json <name>` reports. When it reports both standard output and standard error files, devy SHALL show both.
- **apt:** the system journal for the service's unit, via `journalctl -u <name>`. devy MUST NOT run `sudo` to read logs.
- **docker-managed services:** `<container_cli> logs` for the container `devy-<project>-<service>`. When that container belongs to another project or another machine (see docker-services), devy SHALL refuse with the same error `devy start` gives instead of showing its logs, and SHALL NOT send them with `--explain`.

`<name>` is the backend service name and `<project>` is the project slug, both defined in service-management. Under nix, devy SHALL read the legacy source (`<name>.log` in that same directory, or unit `devy-<name>.service`) instead only when this project owns a legacy-named unit for the service: the per-project source does not exist, the legacy source exists, and the legacy unit's data or working directory is inside this project's root. Otherwise devy MUST NOT show the legacy source.

#### Scenario: Nix on macOS
- **WHEN** the package manager is nix on macOS and the user runs `devy logs redis` in project `app`
- **THEN** devy shows the tail of `devy-app-<hash>-redis.log` in devy's per-user directory

#### Scenario: Nix on Linux
- **WHEN** the package manager is nix on Linux and the user runs `devy logs redis` in project `app`
- **THEN** devy shows journal entries for the user unit `devy-app-<hash>-redis.service`

#### Scenario: Logs from a not-yet-migrated service
- **WHEN** the package manager is nix on macOS, a legacy `sh.devy.redis` unit's data directory is under this project's root, `redis.log` exists in devy's per-user directory, and `devy-<project>-redis.log` does not
- **THEN** `devy logs redis` shows the tail of `redis.log`

#### Scenario: Another project's legacy log is not shown
- **WHEN** the package manager is nix on macOS, the legacy `sh.devy.redis` unit's data directory is under `/src/other`, `redis.log` exists in devy's per-user directory, `devy-<project>-redis.log` does not, and the user runs `devy logs redis` in `/src/app`
- **THEN** devy does not show `redis.log`

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

### Requirement: Log files are read safely
For nix on macOS, devy SHALL read log files only from its private per-user directory, and only when that directory is a real directory owned by the current user with mode 0700. Otherwise devy SHALL fail, reporting `refusing to read service logs: <dir> is not owned by the current user — remove it so devy can create its own`, `refusing to read service logs: <dir> has mode <mode>, expected 700 — remove it so devy can create its own`, or `refusing to read service logs: <dir> is not a directory owned by the current user — remove it so devy can create its own`. `devy logs` MUST NOT create that directory. While it is missing, there are no logs.

On Unix, every log file devy opens, including brew's log files and each reopen while following, SHALL be a regular file that is not a symlink, owned by the current user or root, in a directory owned by the current user or root that is not world-writable unless it has the sticky bit. Otherwise devy SHALL refuse it with one of:
- `<path> is a symlink; devy does not follow it`
- `<path> is not a regular file`
- `<path> is owned by another user; devy will not read it`
- `<dir> is not a directory only you or root can write to; devy will not read logs from it`

For a single service the refusal SHALL fail the command, with the message included in the error. When showing all services, devy SHALL print the refusal for that service, show the others, and then fail with `Could not read the logs of <names>`. While following, devy SHALL print `not following: <reason>` (with the service prefix) and stop following that file. When following ends after such a refusal, devy SHALL fail with `Stopped following a log file devy refused to read`.

#### Scenario: Log directory is not private
- **WHEN** the package manager is nix on macOS, devy's per-user log directory has mode 777, and the user runs `devy logs redis`
- **THEN** devy fails, reporting `refusing to read service logs: <dir> has mode 777, expected 700 — remove it so devy can create its own` and prints no log lines

#### Scenario: Symlinked log file
- **WHEN** the service's log file is a symlink to another file, and the user runs `devy logs redis`
- **THEN** devy fails, reporting `<path> is a symlink; devy does not follow it`, and does not print the target's contents

#### Scenario: One refused log among several services
- **WHEN** brew is the package manager, `redis` and `nginx` are declared, redis's log file is a symlink, and the user runs `devy logs`
- **THEN** devy shows nginx's lines, reports that it refused redis's log, and fails with `Could not read the logs of redis`

#### Scenario: Docker service that cannot be checked among several services
- **WHEN** `redis` is docker-managed and `nginx` is not, the container runtime is unreachable (or redis's container belongs to another project or machine), and the user runs `devy logs`
- **THEN** devy shows nginx's lines, reports why redis's logs can't be read, and fails with `Could not read the logs of redis`
- **AND** with `--follow`, devy prints `redis | not following: <reason>`, keeps following nginx, and when following ends by itself it fails with an error naming redis; stopping with Ctrl-C exits 0

#### Scenario: Log directory missing
- **WHEN** the package manager is nix on macOS, devy's per-user log directory does not exist, and the user runs `devy logs redis`
- **THEN** devy prints `· No logs yet for redis (expected at <path>)`, exits 0, and does not create the directory
