## MODIFIED Requirements

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

### Requirement: Port persistence in the lock file
`devy up` SHALL record the resolved port as `assigned_port` in the dependency's `devy.lock` entry for every module that has a port key and whose port the active backend can apply, including ports set explicitly in `devy.yml`. For services whose port the backend cannot apply, `devy up` MUST NOT record an `assigned_port`.

#### Scenario: Port recorded
- **WHEN** `devy up` assigns port 51000 to `redis` under the nix backend
- **THEN** `devy.lock` contains `assigned_port: 51000` under `redis`

#### Scenario: Port not recorded where it cannot be applied
- **WHEN** `devy up` runs with the brew backend and `devy.yml` declares `redis` with `port: 6380`
- **THEN** the `redis` entry in `devy.lock` has no `assigned_port`

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

## REMOVED Requirements

### Requirement: Port conflict detection
**Reason**: It described `devy check` and `devy up` computing effective ports differently, so the two commands could disagree (e.g. on `mysql` + `mariadb`). Both now use the same port resolution.
**Migration**: See "Port conflicts on resolved ports", which applies one rule to every command.

## ADDED Requirements

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
