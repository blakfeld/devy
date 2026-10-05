# Spec Delta

## MODIFIED Requirements

### Requirement: Backend service name
devy SHALL pass the module's service name to the package-manager backend when starting, stopping or checking a service. postgresql SHALL always use `postgresql` and mariadb SHALL always use `mariadb`. mongodb SHALL use `mongodb-community` on macOS and `mongod` on Linux. Every other module SHALL use the dependency name exactly as written in `devy.yml`, aliases included. The nix backend SHALL combine this name with the project slug, as described in its macOS and Linux requirements.

#### Scenario: Alias reaches the backend
- **WHEN** `devy.yml` declares `elastic` and brew is the package manager
- **THEN** devy runs `brew services start elastic` (even though the installed formula is `elasticsearch-full`)

#### Scenario: Alias under nix
- **WHEN** `devy.yml` declares `meili` in project `app` at `/src/app`, and nix is the package manager on macOS
- **THEN** the launchd label is `sh.devy.app-<hash>.meili`, where `<hash>` is the first 8 hex characters of the hash of `/src/app`

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

## ADDED Requirements

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
