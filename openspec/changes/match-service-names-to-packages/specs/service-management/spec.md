## MODIFIED Requirements

### Requirement: Backend service name
devy SHALL pass the module's service name to the package-manager backend when starting, stopping, checking or reading logs for a service. The same name SHALL be used for every one of those operations, and it SHALL identify the service of the package devy installed for the dependency.

- Under brew, the service name SHALL be the Homebrew formula devy installs for the dependency, with aliases resolved to their canonical module. This is the module's brew package name (for example `elasticsearch-full` for elasticsearch and `mongodb-community` for mongodb). It gets an `@<version>` suffix when `version` is a user-specified formula pin (`<major>` or `<major>.<minor>`) that didn't come from `devy.lock`.
- Under apt, the service name SHALL be the systemd unit the module's apt package provides: `rabbitmq-server` for rabbitmq, `mongod` for mongodb, and the canonical module name for every other module (for example `postgresql`, `mysql`, `mariadb`).
- Under winget, the service name SHALL be the canonical module name.
- Under nix, the service name SHALL be the dependency name as written in `devy.yml`, except that postgresql always uses `postgresql`, mariadb always uses `mariadb`, and mongodb uses `mongodb-community` on macOS and `mongod` on Linux. The nix backend SHALL combine this name with the project slug, as described in its macOS and Linux requirements.

#### Scenario: Alias reaches the backend
- **WHEN** `devy.yml` declares `elastic` and brew is the package manager
- **THEN** devy runs `brew services start elasticsearch-full`

#### Scenario: Alias canonicalized under brew
- **WHEN** `devy.yml` declares `meili` and brew is the package manager
- **THEN** devy runs `brew services start meilisearch`

#### Scenario: Version pin selects the versioned service
- **WHEN** `devy.yml` declares `postgresql` with `version: "16"` and brew is the package manager
- **THEN** devy installs `postgresql@16`, runs `brew services start postgresql@16`, and checks its status with `brew services info --json postgresql@16`

#### Scenario: Locked version does not select a formula
- **WHEN** `devy.yml` declares `mysql` without a `version`, `devy.lock` records version `8.4.2` for it, and brew is the package manager
- **THEN** devy runs `brew services start mysql`

#### Scenario: Apt unit differs from the dependency name
- **WHEN** `devy.yml` declares `rabbitmq` and apt is the package manager
- **THEN** devy runs `sudo systemctl start rabbitmq-server`, and reports it running when `systemctl is-active rabbitmq-server` prints `active`

#### Scenario: Apt unit differs from the package name
- **WHEN** `devy.yml` declares `mysql` and apt is the package manager
- **THEN** devy installs `mysql-server` and runs `sudo systemctl start mysql`

#### Scenario: Logs use the same name as start
- **WHEN** `devy.yml` declares `postgresql` with `version: "16"`, brew is the package manager, and the user runs `devy logs postgresql`
- **THEN** devy reads the log files that `brew services info --json postgresql@16` reports

#### Scenario: Alias under nix
- **WHEN** `devy.yml` declares `meili` in project `app` at `/src/app`, and nix is the package manager on macOS
- **THEN** the launchd label is `sh.devy.app-<hash>.meili`, where `<hash>` is the first 8 hex characters of the hash of `/src/app`
