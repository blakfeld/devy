# service-ports Specification

## Purpose
Defines how devy chooses, validates, persists, and exposes the TCP port for each service dependency, so projects get stable non-conflicting ports and connection environment variables.

## Requirements

### Requirement: Port precedence during up
For every dependency whose module declares a port key, devy SHALL resolve the port in this order:
1. An explicit value in `devy.yml`.
2. When the active backend can apply a port to that service, the `assigned_port` recorded for the dependency's canonical name in the existing `devy.lock`.
3. When the active backend can apply a port to that service, a free port chosen by the operating system (`devy up` only).
4. Otherwise, the module's default port.

#### Scenario: Explicit port wins
- **WHEN** `devy.yml` declares `redis` with `port: 6380` and `devy.lock` records `assigned_port: 51000`
- **THEN** devy uses port 6380

#### Scenario: Locked port reused
- **WHEN** the backend is nix, `devy.yml` declares `redis` without a port, and `devy.lock` records `assigned_port: 51000` for `redis`
- **THEN** devy uses port 51000

#### Scenario: Random port assigned
- **WHEN** the backend is nix, `devy.yml` declares `redis` without a port, and there is no lock entry for it
- **THEN** `devy up` binds `127.0.0.1:0`, uses the port the OS assigns, and fails with `Failed to find available port for redis` if that is impossible

#### Scenario: Backend cannot apply the port
- **WHEN** the backend is brew, `devy.yml` declares `redis` without a port, and `devy.lock` records `assigned_port: 51000` for `redis`
- **THEN** devy uses redis's default port 6379, and `REDIS_PORT` and `REDIS_URL` reference 6379

### Requirement: Ports survive update
Port resolution SHALL read the existing `devy.lock` even when `devy up --update` ignores the lock for version pinning.

#### Scenario: Update keeps ports
- **WHEN** the user runs `devy up --update` and `devy.lock` records `assigned_port: 51000` for `redis`
- **THEN** devy still uses port 51000 for `redis`

### Requirement: Port key per module
Each service module SHALL declare which config key holds its port; the default key is `port`, and modules MAY use a different key (mailhog uses `smtp_port`).

#### Scenario: Mailhog port key
- **WHEN** `devy.yml` declares `mailhog` with `smtp_port: 2525`
- **THEN** devy uses 2525 as mailhog's port

### Requirement: Port range validation
A configured or assigned port MUST be between 1 and 65535; otherwise devy SHALL fail with `'<dep>': port value <N> is out of range (must be 1–65535)`.

#### Scenario: Zero port
- **WHEN** `devy.yml` declares `redis` with `port: 0`
- **THEN** `devy up` and `devy check` fail with the out-of-range error

### Requirement: Port persistence in the lock file
`devy up` SHALL record the resolved port as `assigned_port` in the dependency's `devy.lock` entry for every module that has a port key and whose port the active backend can apply, including ports set explicitly in `devy.yml`. For services whose port the backend cannot apply, `devy up` MUST NOT record an `assigned_port`.

#### Scenario: Port recorded
- **WHEN** `devy up` assigns port 51000 to `redis` under the nix backend
- **THEN** `devy.lock` contains `assigned_port: 51000` under `redis`

#### Scenario: Port not recorded where it cannot be applied
- **WHEN** `devy up` runs with the brew backend and `devy.yml` declares `redis` with `port: 6380`
- **THEN** the `redis` entry in `devy.lock` has no `assigned_port`

### Requirement: Host and port environment variables
For every service dependency, `devy up` SHALL export `<NAME>_HOST=127.0.0.1` and, when a port is known, `<NAME>_PORT=<port>`, where `<NAME>` is the canonical name upper-cased with `-` replaced by `_`.

#### Scenario: Postgres alias
- **WHEN** `devy.yml` declares `postgres` and the resolved port is 5433
- **THEN** the environment contains `POSTGRESQL_HOST=127.0.0.1` and `POSTGRESQL_PORT=5433`

### Requirement: Applying ports to database services
On brew and apt, for postgresql, mysql, and mariadb, when the resolved port differs from the default (or `cli_args` is set for mysql/mariadb), devy SHALL write a devy-managed config file into the package manager's service config directory. Under nix, these services SHALL receive the port through their launch arguments instead (see service-management).

#### Scenario: Postgres on brew
- **WHEN** `postgresql` resolves to port 5433 with the brew backend
- **THEN** devy writes `$(brew --prefix postgresql)/etc/devy.conf` containing `port = 5433`

#### Scenario: MySQL on apt
- **WHEN** `mysql` resolves to port 3307 with the apt backend
- **THEN** devy writes `/etc/mysql/conf.d/my.cnf` with a `[mysqld]` section containing `port = 3307`

#### Scenario: Backend without config directory
- **WHEN** `postgresql` is declared with an explicit non-default port with the winget backend
- **THEN** devy warns that it cannot apply the port to postgresql with winget

#### Scenario: Postgres under nix
- **WHEN** `postgresql` resolves to port 51000 with the nix backend
- **THEN** devy writes no config file into a system directory and launches postgres listening on 51000

### Requirement: MySQL cli_args sanitization
devy SHALL accept a whitespace-separated `cli_args` token for MySQL or MariaDB only if all of these hold:
- it has the form `--key=value`
- its key (with `_` treated as `-`) is on devy's allowlist of server tuning options
- its value contains no newline, carriage return or NUL

The allowlist SHALL contain:
- **buffers and caches:** `innodb-buffer-pool-size`, `innodb-log-file-size`, `innodb-redo-log-capacity`, `innodb-flush-log-at-trx-commit`, `innodb-file-per-table`, `key-buffer-size`, `table-open-cache`, `thread-cache-size`, `tmp-table-size`, `max-heap-table-size`, `sort-buffer-size`, `join-buffer-size`
- **limits:** `max-connections`, `max-allowed-packet`, `wait-timeout`, `interactive-timeout`
- **character sets and SQL behavior:** `character-set-server`, `collation-server`, `sql-mode`, `default-time-zone`, `lower-case-table-names`, `explicit-defaults-for-timestamp`, `default-authentication-plugin`
- **logging:** `log-bin-trust-function-creators`, `slow-query-log`, `long-query-time`, `general-log`
- **other:** `transaction-isolation`, `event-scheduler`, `performance-schema`, `skip-name-resolve`

Every other token SHALL be skipped with the warning `skipped cli_args token <token>: not an allowed server option`. Network, authentication, file-path, plugin and startup-script options (for example `bind-address`, `skip-grant-tables`, `init-file`, `plugin-load`, `plugin-dir`, `secure-file-priv`, `datadir`, `socket`, `user`, `general-log-file`) are therefore never passed. Accepted tokens SHALL be converted into `key = value` lines when devy writes a config file, or passed before devy's own forced arguments when launching directly. Under brew, devy SHALL write its settings to a devy-owned include file inside the formula's configuration directory, and SHALL NOT overwrite an existing `my.cnf` it did not create.

#### Scenario: Valid and invalid args
- **WHEN** `cli_args` is `--innodb-buffer-pool-size=256M bogus`
- **THEN** the config contains `innodb-buffer-pool-size = 256M` and devy warns that `bogus` was skipped

#### Scenario: Dangerous option skipped
- **WHEN** `cli_args` is `--bind-address=0.0.0.0 --skip-grant-tables=1`
- **THEN** both tokens are skipped with warnings and the server listens only on `127.0.0.1`

### Requirement: Port conflicts on resolved ports
devy SHALL fail when two service dependencies resolve to the same effective port, reporting `port conflict: '<a>' and '<b>' both use port <N>`. The effective port is the port produced by port resolution, and ports not yet assigned are excluded.

#### Scenario: Explicit conflict
- **WHEN** `devy.yml` declares `redis` with `port: 7000` and `memcached` with `port: 7000`
- **THEN** devy fails with a port conflict naming both dependencies and port 7000

#### Scenario: Default-port conflict where ports cannot be applied
- **WHEN** the backend is brew and `devy.yml` declares `elasticsearch` and `opensearch` without ports (both default to 9200)
- **THEN** devy fails with `port conflict: 'elasticsearch' and 'opensearch' both use port 9200`

### Requirement: Consistent port resolution across commands
`devy start`, `devy restart`, `devy check`, and `devy status` SHALL resolve service ports with the same precedence as `devy up`, using `devy.lock`, and MUST NOT assign new random ports or write `devy.lock`. When a port would be randomly assigned by `devy up` but is not yet in the lock, `devy check` SHALL treat it as not yet assigned and exclude it from conflict detection. `devy start` and `devy restart` SHALL instead fail with `'<name>' has no port in devy.lock yet — run `devy up` first`.

#### Scenario: Start uses the locked port
- **WHEN** the backend is nix, `devy.lock` records `assigned_port: 51000` for `redis`, and the user runs `devy start redis`
- **THEN** redis is launched on port 51000 and the health check probes port 51000

#### Scenario: Start before up
- **WHEN** the backend is nix, `devy.lock` has no entry for `redis`, and the user runs `devy start redis`
- **THEN** devy fails, telling the user to run `devy up` first, and starts nothing

#### Scenario: Check agrees with up on unassigned ports
- **WHEN** the backend is nix, `devy.yml` declares `mysql` and `mariadb` with no ports, and `devy.lock` has no entries for them
- **THEN** `devy check` does not report a port conflict between them

### Requirement: Port applicability per backend
devy SHALL treat a service's port as applicable when the active backend can make the running service listen on it:
- Under nix on macOS and Linux, every built-in service.
- Under brew and apt, `postgresql`, `mysql`, and `mariadb`, via their service config directory.
- Under winget, none.

When an explicit non-default port is configured for a service whose port is not applicable, `devy up` and `devy check` MUST warn that devy cannot apply the port and that the service must be configured to listen on it manually.

#### Scenario: Explicit port not applicable
- **WHEN** the backend is brew and `devy.yml` declares `redis` with `port: 6380`
- **THEN** devy uses 6380 in `REDIS_PORT` and `REDIS_URL` and warns that it cannot make redis listen on 6380 with brew

### Requirement: Ports for docker-managed services
A docker-managed service's port SHALL always be applicable, so it follows the full port precedence: explicit, locked, then newly assigned. The resolved port SHALL be published on the host as `127.0.0.1:<resolved port>`, mapped to the module's container port. The container port SHALL be fixed per module and MUST NOT change with the resolved port. Exported `<NAME>_PORT` and module URLs SHALL use the host port.

#### Scenario: Random port under docker with brew packages
- **WHEN** `package_manager: brew`, `service_manager: docker`, and `redis` has no port and no lock entry
- **THEN** `devy up` assigns a free port such as 51000, publishes `127.0.0.1:51000:6379`, and exports `REDIS_URL=redis://127.0.0.1:51000`

#### Scenario: Two databases on default ports
- **WHEN** `mysql` and `mariadb` are both docker-managed with no explicit ports
- **THEN** `devy check` reports no port conflict and `devy up` gives them distinct host ports
