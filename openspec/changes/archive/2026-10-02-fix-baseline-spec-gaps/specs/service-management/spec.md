## MODIFIED Requirements

### Requirement: Starting a single service
`devy start <name>` SHALL resolve the service's port as `devy up` does (using `devy.lock`), start the service when it is not running, and then wait for its health check on that port; a health-check timeout MUST be reported as a warning rather than a failure.

#### Scenario: Already running
- **WHEN** the named service is already running
- **THEN** devy prints `○ <name> is already running` and exits 0 without starting it again

#### Scenario: Health check times out
- **WHEN** the service starts but never passes its health check within the configured attempts
- **THEN** devy warns that the service started but the health check timed out and asks the user to verify manually
- **AND** devy prints `✓ <name> started` and exits 0

#### Scenario: Locked port honored
- **WHEN** `devy.lock` records `assigned_port: 51000` for `redis` under the nix backend and the user runs `devy start redis`
- **THEN** redis listens on 51000 and the health check probes 51000

### Requirement: Restarting a single service
`devy restart <name>` SHALL stop the service if running (waiting for it to stop) and then always start it using the same port resolution and health-check warning behavior as `start`.

#### Scenario: Restart a stopped service
- **WHEN** the named service is not running
- **THEN** devy prints `○ <name> was already stopped` and then starts the service

### Requirement: Nix service backend on macOS
When the package manager is nix on macOS, devy SHALL run the service binary from `.devy/nix-profile/bin` under a launchd agent labeled `sh.devy.<name>`, writing `~/Library/LaunchAgents/sh.devy.<name>.plist` and loading and starting it with `launchctl`. The plist MUST include the service's launch arguments in `ProgramArguments`, its launch environment in `EnvironmentVariables`, KeepAlive and RunAtLoad false, and logs at `$TMPDIR/devy-<name>.log`. The plist SHALL be rewritten on every start so it reflects the current port and configuration.

#### Scenario: Start redis under nix on macOS
- **WHEN** devy starts `redis` resolved to port 51000 with the nix backend on macOS
- **THEN** it writes the `sh.devy.redis` plist whose `ProgramArguments` run `.devy/nix-profile/bin/redis-server` with `--port 51000`, `--bind 127.0.0.1`, and a data directory under `.devy/data/redis`, then runs `launchctl load` and `launchctl start sh.devy.redis`

#### Scenario: postgresql label under nix on macOS
- **WHEN** devy starts `postgres` with the nix backend on macOS
- **THEN** the label is `sh.devy.postgresql`, and the plist runs `.devy/nix-profile/bin/postgres` with `-D`, `-p`, `-k` and `listen_addresses` arguments for `.devy/data/postgresql`

#### Scenario: Stop under launchd
- **WHEN** devy stops a nix-managed service on macOS
- **THEN** it runs `launchctl stop` (ignoring failure) and unloads the plist if it exists

### Requirement: Nix service backend on Linux
When the package manager is nix on Linux, devy SHALL run the service binary from `.devy/nix-profile/bin` as a systemd user unit `devy-<name>.service` written to `~/.config/systemd/user/`, with `Restart=on-failure`, and control it with `systemctl --user`. The unit's `ExecStart` MUST include the service's launch arguments, each launch environment variable MUST appear as an `Environment=` line, and the unit SHALL be rewritten and `daemon-reload` run on every start.

#### Scenario: Start redis under nix on Linux
- **WHEN** devy starts `redis` resolved to port 51000 with the nix backend on Linux
- **THEN** it writes `devy-redis.service` whose `ExecStart` runs `.devy/nix-profile/bin/redis-server` with `--port 51000`, runs `systemctl --user daemon-reload`, and starts the unit

### Requirement: Unsupported nix services
When a dependency is a service only through the generic module (no built-in nix launch definition), starting it with the nix backend MUST fail with a message telling the user to start it manually using the binary in `.devy/nix-profile/bin/`. Every built-in service module MUST provide a nix launch definition.

#### Scenario: MongoDB under nix
- **WHEN** devy starts `mongodb` with the nix backend
- **THEN** devy launches `mongod` from the project profile instead of failing

#### Scenario: Kafka under nix
- **WHEN** devy starts `kafka` without `kraft: true` using the nix backend
- **THEN** devy does not start zookeeper, and launches Kafka in KRaft mode from the project profile instead of failing

## ADDED Requirements

### Requirement: Project-local service data under nix
Services run by the nix backend SHALL keep their data, sockets, and generated config files under `<project_root>/.devy/data/<canonical-name>/`, creating the directory on first start, and MUST NOT write to system directories.

#### Scenario: Data directory created
- **WHEN** `devy up` starts `postgresql` with the nix backend for the first time
- **THEN** `.devy/data/postgresql/` exists and holds the database cluster

### Requirement: One-time service initialization under nix
When a nix-run service requires initialization before its first start, devy SHALL run that initialization once, before launching it, only when the data directory has not yet been initialized. If initialization fails, devy SHALL fail with `Failed to initialize <dep>` and MUST NOT launch the service.

#### Scenario: Postgres cluster initialized once
- **WHEN** `devy up` starts `postgresql` under nix and `.devy/data/postgresql/PG_VERSION` does not exist
- **THEN** devy runs `initdb` for that directory before starting postgres
- **AND** on the next `devy up`, devy does not run `initdb` again
