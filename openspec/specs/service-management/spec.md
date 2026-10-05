# service-management Specification

## Purpose
Defines how devy lists, starts, stops, and restarts background services declared in `devy.yml`, how it waits for readiness and shutdown, and how each package-manager backend controls the service on the host platform.

## Requirements

### Requirement: Service name resolution
The `start`, `stop`, and `restart` commands SHALL resolve the given name against the configured dependencies by exact name or by canonical name, so that an alias and its canonical name refer to the same dependency.

#### Scenario: Alias used on the command line
- **WHEN** `devy.yml` declares `postgresql` and the user runs `devy start postgres`
- **THEN** devy starts the `postgresql` dependency

#### Scenario: Unknown name
- **WHEN** the user runs `devy start foo` and no dependency matches `foo`
- **THEN** devy fails with `'foo' not found in devy.yml dependencies`

#### Scenario: Dependency is not a service
- **WHEN** the user runs `devy start jq` and `jq` is declared but is not a service module
- **THEN** devy fails with `'jq' is not a service`

### Requirement: Backend service name
devy SHALL pass the module's service name to the package-manager backend when starting, stopping or checking a service. postgresql SHALL always use `postgresql` and mariadb SHALL always use `mariadb`. mongodb SHALL use `mongodb-community` on macOS and `mongod` on Linux. Every other module SHALL use the dependency name exactly as written in `devy.yml`, aliases included. The nix backend SHALL combine this name with the project slug, as described in its macOS and Linux requirements.

#### Scenario: Alias reaches the backend
- **WHEN** `devy.yml` declares `elastic` and brew is the package manager
- **THEN** devy runs `brew services start elastic` (even though the installed formula is `elasticsearch-full`)

#### Scenario: Alias under nix
- **WHEN** `devy.yml` declares `meili` in project `app` at `/src/app`, and nix is the package manager on macOS
- **THEN** the launchd label is `sh.devy.app-<hash>.meili`, where `<hash>` is the first 8 hex characters of the hash of `/src/app`

### Requirement: Listing services
`devy services` SHALL print each declared service with a running or stopped indicator, suffixing docker-managed services with `(docker)`, and SHALL print `No services defined.` and exit 0 when no service dependencies exist.

#### Scenario: Mixed running state
- **WHEN** `devy.yml` declares `redis` (running) and `postgresql` (stopped)
- **THEN** devy prints a "Services" header, a filled `●` next to `redis`, and a dimmed `○` next to `postgresql`

#### Scenario: No services
- **WHEN** `devy.yml` declares only non-service dependencies
- **THEN** devy prints `No services defined.` and exits 0

#### Scenario: Docker-managed service labeled
- **WHEN** `redis` is docker-managed and running
- **THEN** devy prints `● redis (docker)`

### Requirement: Starting a single service
`devy start <name>` SHALL resolve the service's port as `devy up` does (using `devy.lock`), start the service when it is not running, and then wait for its health check on that port; a health-check timeout MUST be reported as a warning rather than a failure, and that warning MUST tell the user to run `devy logs <name>`.

#### Scenario: Already running
- **WHEN** the named service is already running
- **THEN** devy prints `○ <name> is already running` and exits 0 without starting it again

#### Scenario: Health check times out
- **WHEN** the service starts but never passes its health check within the configured attempts
- **THEN** devy warns that the service started but the health check timed out, asks the user to verify manually, and suggests `devy logs <name>`
- **AND** devy prints `✓ <name> started` and exits 0

#### Scenario: Locked port honored
- **WHEN** `devy.lock` records `assigned_port: 51000` for `redis` under the nix backend and the user runs `devy start redis`
- **THEN** redis listens on 51000 and the health check probes 51000

### Requirement: Stopping a single service
`devy stop <name>` SHALL stop a running service and wait until it is no longer running; it MUST NOT fail when the service is already stopped.

#### Scenario: Already stopped
- **WHEN** the named service is not running
- **THEN** devy prints `○ <name> is already stopped` and exits 0

#### Scenario: Service does not stop
- **WHEN** the service is still running after the configured shutdown attempts
- **THEN** devy fails with a message that the service did not stop and suggests stopping it manually

### Requirement: Restarting a single service
`devy restart <name>` SHALL stop the service if running (waiting for it to stop) and then always start it using the same port resolution and health-check warning behavior as `start`.

#### Scenario: Restart a stopped service
- **WHEN** the named service is not running
- **THEN** devy prints `○ <name> was already stopped` and then starts the service

### Requirement: Bringing the environment down
`devy down` SHALL run the `before_down` hook, stop every running service in declaration order waiting for each to stop, and then run the `after_down` hook; it MUST NOT modify `devy.lock` or the shadowenv file. Docker-managed services SHALL be stopped with the container CLI and their containers and volumes kept, unless `devy down --volumes` is given, in which case devy SHALL also remove each docker-managed service's container and named volume and print `✓ <dep> container and volume removed`. `--volumes` SHALL have no effect on package-managed services.

#### Scenario: Mixed services
- **WHEN** `devy.yml` declares `redis` (running) and `postgresql` (stopped)
- **THEN** devy prints `○ postgresql already stopped`, stops `redis`, prints `✓ redis stopped`, and prints `✓ all services stopped`

#### Scenario: Nothing running
- **WHEN** all declared services are already stopped
- **THEN** devy prints `○ nothing to stop`

#### Scenario: No services declared
- **WHEN** `devy.yml` declares no service dependencies
- **THEN** devy prints `○ no services defined` and exits 0

#### Scenario: Stop failure skips after_down
- **WHEN** stopping a service fails
- **THEN** devy exits with an error and does not run the `after_down` hook

#### Scenario: Plain down keeps data
- **WHEN** docker-managed `postgresql` is running and the user runs `devy down`
- **THEN** devy stops the container and its volume `devy-<project>-postgresql` still exists

#### Scenario: Down with volumes
- **WHEN** docker-managed `postgresql` exists and the user runs `devy down --volumes`
- **THEN** devy stops and removes the container and removes the volume `devy-<project>-postgresql`

### Requirement: Readiness and shutdown polling
Waiting for a service SHALL poll its health check (or running state when stopping) up to a per-module attempt count with a fixed interval (default 10 attempts at 500 ms), failing with the attempt count when exhausted. Health waits SHALL print `Still waiting for <name> (<n>/<max>)` on every 10th attempt before the last, so a wait with the default 10 attempts prints no progress. Shutdown waits SHALL report no progress and SHALL fail with `<name> did not stop after <N> attempts — try stopping it manually or run devy logs <name>`.

#### Scenario: Service becomes healthy
- **WHEN** the health check succeeds on the third attempt
- **THEN** waiting returns successfully without further attempts

#### Scenario: Never healthy
- **WHEN** the health check fails on every attempt
- **THEN** waiting fails with `<name> did not become healthy after <N> attempts`

#### Scenario: Never stops
- **WHEN** a service is still running after every shutdown attempt
- **THEN** waiting fails with `<name> did not stop after <N> attempts — try stopping it manually or run devy logs <name>`

### Requirement: Homebrew service backend
When the package manager is brew, devy SHALL control services with `brew services start|stop <name>` and SHALL treat a service as running when `brew services info --json <name>` reports `running: true`; a failed info command MUST be treated as not running. If the info command succeeds but returns an empty JSON array, devy SHALL fail with `` `brew services info` returned an empty array — service may not be managed by brew `` instead of reporting the service as stopped.

#### Scenario: Start via brew
- **WHEN** devy starts `redis` with the brew backend
- **THEN** it runs `brew services start redis`

### Requirement: Apt (systemd) service backend
When the package manager is apt, devy SHALL control services with `sudo systemctl start|stop <name>` and SHALL treat a service as running when `systemctl is-active <name>` reports `active`.

#### Scenario: Status via systemctl
- **WHEN** `systemctl is-active redis` prints `active`
- **THEN** devy reports `redis` as running

### Requirement: WinGet service backend
When the package manager is winget, devy SHALL control services with `net start|stop <name>` and SHALL treat a service as running when `sc query <name>` output contains `RUNNING`.

#### Scenario: Status via sc
- **WHEN** `sc query redis` output contains `RUNNING`
- **THEN** devy reports `redis` as running

### Requirement: Nix service backend on macOS
When the package manager is nix on macOS, devy SHALL run the service binary from `.devy/nix-profile/bin` under a launchd agent labeled `sh.devy.<project>.<name>`, writing `~/Library/LaunchAgents/sh.devy.<project>.<name>.plist` and loading and starting it with `launchctl`. `<project>` is the project slug used for docker-managed container names: the project `name` lower-cased with characters outside `[a-z0-9-]` replaced by `-`, followed by `-` and the first 8 hex characters of a hash of the project root path. The plist MUST include the service's launch arguments in `ProgramArguments`, its launch environment plus `DEVY_PROJECT_ROOT=<project root>` in `EnvironmentVariables`, KeepAlive and RunAtLoad false, and logs at `devy-<project>-<name>.log` in devy's private per-user directory, `devy-<uid>` under `$XDG_RUNTIME_DIR` when it is set and otherwise under `$TMPDIR`. The plist SHALL be rewritten on every start so it reflects the current port and configuration.

#### Scenario: Start redis under nix on macOS
- **WHEN** devy starts `redis` resolved to port 51000 with the nix backend on macOS in project `app` at `/src/app`
- **THEN** it writes the `sh.devy.app-<hash>.redis` plist whose `ProgramArguments` run `.devy/nix-profile/bin/redis-server` with `--port 51000`, `--bind 127.0.0.1`, and a data directory under `.devy/data/redis`, and whose `EnvironmentVariables` include `DEVY_PROJECT_ROOT=/src/app`
- **AND** it runs `launchctl load` and `launchctl start sh.devy.app-<hash>.redis`

#### Scenario: postgresql label under nix on macOS
- **WHEN** devy starts `postgres` with the nix backend on macOS
- **THEN** the label is `sh.devy.<project>.postgresql`, and the plist runs `.devy/nix-profile/bin/postgres` with `-D`, `-p`, `-k` and `listen_addresses` arguments for `.devy/data/postgresql`

#### Scenario: Stop under launchd
- **WHEN** devy stops a nix-managed service on macOS
- **THEN** it runs `launchctl stop` on `sh.devy.<project>.<name>` (ignoring failure) and unloads the plist if it exists

#### Scenario: Two checkouts run redis
- **WHEN** the main checkout `/src/app` and a worktree `/src/app-feat`, both with project name `app`, each run nix-managed `redis` on macOS
- **THEN** two distinct agents, `sh.devy.app-<hashA>.redis` and `sh.devy.app-<hashB>.redis`, run at the same time, and starting or stopping one does not affect the other

#### Scenario: Log file location under nix on macOS
- **WHEN** devy starts `redis` with the nix backend on macOS in project `app` at `/src/app`, `$XDG_RUNTIME_DIR` is not set, and the user's uid is 501
- **THEN** the plist sends the service's output to `$TMPDIR/devy-501/devy-app-<hash>-redis.log`

### Requirement: Nix service backend on Linux
When the package manager is nix on Linux, devy SHALL run the service binary from `.devy/nix-profile/bin` as a systemd user unit `devy-<project>-<name>.service` written to `~/.config/systemd/user/`, with `Restart=on-failure`, and control it with `systemctl --user`. `<project>` is the same project slug used on macOS. The unit's `ExecStart` MUST include the service's launch arguments. Each launch environment variable, plus `DEVY_PROJECT_ROOT=<project root>`, MUST appear as an `Environment=` line. The unit SHALL be rewritten and `daemon-reload` run on every start.

#### Scenario: Start redis under nix on Linux
- **WHEN** devy starts `redis` resolved to port 51000 with the nix backend on Linux in project `app` at `/src/app`
- **THEN** it writes `devy-app-<hash>-redis.service` whose `ExecStart` runs `.devy/nix-profile/bin/redis-server` with `--port 51000` and which has `Environment=DEVY_PROJECT_ROOT=/src/app`
- **AND** it runs `systemctl --user daemon-reload` and starts the unit

### Requirement: Unsupported nix services
When a dependency is a service only through the generic module (no built-in nix launch definition), starting it with the nix backend MUST fail with a message telling the user to start it manually using the binary in `.devy/nix-profile/bin/`. Every built-in service module MUST provide a nix launch definition.

#### Scenario: MongoDB under nix
- **WHEN** devy starts `mongodb` with the nix backend
- **THEN** devy launches `mongod` from the project profile instead of failing

#### Scenario: Kafka under nix
- **WHEN** devy starts `kafka` without `kraft: true` using the nix backend
- **THEN** devy does not start zookeeper, and launches Kafka in KRaft mode from the project profile instead of failing

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

### Requirement: Legacy nix service units are migrated
Before starting or stopping a nix-managed service, whether through `devy up`, `start`, `stop`, `restart` or `down`, devy SHALL look for units of this service that use an outdated name:
- the legacy name, `sh.devy.<name>` on macOS or `devy-<name>.service` on Linux, when its data directory argument or working directory is inside this project's root
- a per-project name with a different project slug, for example after the project `name` changed, when its recorded `DEVY_PROJECT_ROOT` equals this project's root

For each such unit, devy SHALL stop it, unload or disable it, delete its plist or unit file, and print `○ migrated <name> to a per-project service name`. When starting, devy SHALL migrate a unit only right before it writes the new unit, so a start that fails before that point leaves a running legacy unit in place. Read-only commands (`devy status`, `devy services`, `devy check`) MUST NOT migrate units. When this project's legacy-named unit is running, they SHALL report the service as running. `devy check` SHALL also print `○ <name> uses a legacy service name; it will be migrated on next start`, without counting it as an issue. The note applies only to legacy-named units, not to per-project units under an old project slug. devy MUST NOT stop or remove a legacy unit that belongs to a different project root. While another project's legacy unit holds the service's resolved port, the existing port-in-use and health-check failures SHALL apply unchanged.

#### Scenario: Upgrade with a running legacy service
- **WHEN** a previous devy version started `sh.devy.redis` for `/src/app`, and the user runs `devy up` in `/src/app` with the new version
- **THEN** devy stops and removes `sh.devy.redis`, prints the migration notice, and starts `sh.devy.app-<hash>.redis`

#### Scenario: Failed start keeps the legacy service
- **WHEN** a previous devy version's `sh.devy.redis` is running for `/src/app`, and `devy start redis` in `/src/app` fails before writing the new unit, for example because the resolved port is taken
- **THEN** `sh.devy.redis` is left running with its plist in place, and no migration notice is printed

#### Scenario: Status does not migrate
- **WHEN** a previous devy version's `sh.devy.redis` is running for `/src/app`, and the user runs `devy status` there
- **THEN** redis is reported as running, and `sh.devy.redis` is left loaded with its plist in place

#### Scenario: Project renamed
- **WHEN** `sh.devy.app-<hash>.redis` records `DEVY_PROJECT_ROOT=/src/app`, the project `name` in `/src/app/devy.yml` changes to `shop`, and the user runs `devy up`
- **THEN** devy removes `sh.devy.app-<hash>.redis` and starts `sh.devy.shop-<hash>.redis`

#### Scenario: Another project's legacy unit
- **WHEN** a legacy `sh.devy.redis` unit points at `/src/other/.devy/data/redis` and the user runs `devy up` in `/src/app`
- **THEN** devy leaves `sh.devy.redis` running and untouched
