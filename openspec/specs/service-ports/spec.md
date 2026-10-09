# service-ports Specification

## Purpose
Defines how devy chooses, validates, persists, and exposes the TCP port for each service dependency, so projects get stable non-conflicting ports and connection environment variables.

## Requirements

### Requirement: Port precedence during up
For every dependency whose module declares a port key, devy SHALL resolve the port in this order:
1. An explicit value in `devy.yml`.
2. When the active backend can apply a port to that service, the recorded port. In a linked git worktree, the recorded port is the port for the dependency's canonical name in `.devy/worktree.yml`. Otherwise, it is the `assigned_port` recorded for the dependency's canonical name in the existing `devy.lock`. In a linked worktree, devy MUST NOT use `devy.lock`'s `assigned_port`.
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

#### Scenario: Worktree ignores the lock's port
- **WHEN** the backend is nix, `devy up` runs in a linked worktree, `devy.lock` records `assigned_port: 51000` for `redis`, and `.devy/worktree.yml` has no entry for `redis`
- **THEN** devy assigns a new free port other than 51000 to `redis` and exports it as `REDIS_PORT`

#### Scenario: Worktree reuses its own port
- **WHEN** the backend is nix, the project is a linked worktree, and `.devy/worktree.yml` records `redis: 52000`
- **THEN** devy uses port 52000

### Requirement: Ports survive update
Port resolution SHALL read the existing recorded ports, either `devy.lock` or, in a linked worktree, `.devy/worktree.yml`, even when `devy up --update` ignores the lock for version pinning.

#### Scenario: Update keeps ports
- **WHEN** the user runs `devy up --update` and `devy.lock` records `assigned_port: 51000` for `redis`
- **THEN** devy still uses port 51000 for `redis`

#### Scenario: Update keeps worktree ports
- **WHEN** the user runs `devy up --update` in a linked worktree whose `.devy/worktree.yml` records `redis: 52000`
- **THEN** devy still uses port 52000 for `redis`

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
Outside a linked git worktree, `devy up` SHALL record the resolved port as `assigned_port` in the dependency's `devy.lock` entry for every module that has a port key and whose port the active backend can apply, including ports set explicitly in `devy.yml`. For services whose port the backend cannot apply, `devy up` MUST NOT record an `assigned_port`.

In a linked worktree, `devy up` SHALL record each such resolved port in `.devy/worktree.yml` instead, except ports set explicitly in `devy.yml`. `devy up` MUST NOT create `.devy/worktree.yml` when there is nothing to record. A port that `devy up` newly assigns in a worktree MUST NOT be any of the `assigned_port` values in `devy.lock`. It SHALL write each dependency's `assigned_port` in `devy.lock` exactly as the existing `devy.lock` had it, and leave it absent where it was absent, so a worktree never changes the committed ports.

#### Scenario: Port recorded
- **WHEN** `devy up` assigns port 51000 to `redis` under the nix backend
- **THEN** `devy.lock` contains `assigned_port: 51000` under `redis`

#### Scenario: Port not recorded where it cannot be applied
- **WHEN** `devy up` runs with the brew backend and `devy.yml` declares `redis` with `port: 6380`
- **THEN** the `redis` entry in `devy.lock` has no `assigned_port`

#### Scenario: Worktree leaves lock ports alone
- **WHEN** `devy.lock` records `assigned_port: 51000` for `redis`, and `devy up` in a linked worktree assigns 52000
- **THEN** `.devy/worktree.yml` records `redis: 52000`, `devy.lock` still records `assigned_port: 51000`, and if nothing else changed, `devy.lock` is not rewritten

#### Scenario: Worktree never reuses a committed port
- **WHEN** `devy.lock` records `assigned_port: 51000` for `redis` and `assigned_port: 51001` for `postgres`, `.devy/worktree.yml` has no entries, and `devy up` runs in a linked worktree with the nix backend
- **THEN** neither port assigned to `redis` or `postgres` in the worktree is 51000 or 51001

#### Scenario: New service in a worktree
- **WHEN** a worktree's `devy.yml` adds `memcached`, which has no entry in `devy.lock`, and the user runs `devy up` there with the nix backend
- **THEN** the new `memcached` entry in `devy.lock` has no `assigned_port`, and its port is recorded only in `.devy/worktree.yml`

### Requirement: Host and port environment variables
For every service dependency, `devy up` SHALL export `<NAME>_HOST=127.0.0.1` and, when a port is known, `<NAME>_PORT=<port>`, where `<NAME>` is the canonical name upper-cased with `-` replaced by `_`.

#### Scenario: Postgres alias
- **WHEN** `devy.yml` declares `postgres` and the resolved port is 5433
- **THEN** the environment contains `POSTGRESQL_HOST=127.0.0.1` and `POSTGRESQL_PORT=5433`

### Requirement: Applying ports to database services
On brew and apt, `postgresql`, `mysql` and `mariadb` run as one machine-wide server, so devy SHALL apply only a port set explicitly in `devy.yml` to them, and only where devy's config file takes effect:
- **apt** (`postgresql`, `mysql`, `mariadb`): when the explicit port differs from the default (or `cli_args` is set for mysql/mariadb), devy SHALL write a devy-managed file into the service config directory: `/etc/postgresql/<highest version>/main/conf.d/devy.conf` for postgresql, `/etc/mysql/conf.d/devy.cnf` for mysql and mariadb. Both files SHALL start with the line `# devy-managed`. devy SHALL NOT write or remove `/etc/mysql/conf.d/my.cnf`. MySQL and MariaDB read `conf.d` in file-name order and the last value wins, so a `my.cnf` there overrides `devy.cnf`, and the default port 3306 when devy has no settings to write (older devy versions wrote a random port to it). Whenever one exists, whatever the port and `cli_args`, every `devy up` SHALL warn that it is read after `devy.cnf` and overrides devy's settings and the default port 3306, and that a `my.cnf` left by an older devy should be deleted, and `devy check` SHALL report the same. devy SHALL check only that the file exists, and SHALL NOT read or show its content. When the write fails (for example the directory is not writable without root), `devy up` MUST NOT fail; it SHALL warn on every run while the file does not hold devy's settings, naming the file and only the lines to put in it.
- **brew** (`mysql`, `mariadb`): devy SHALL write its settings to `$(brew --prefix)/etc/my.cnf.d/devy.cnf`. While devy's settings differ from the defaults and `$(brew --prefix)/etc/my.cnf` does not include `my.cnf.d`, every `devy up` SHALL warn that the settings do not take effect and name the `!includedir` line to add, and `devy check` SHALL report the same problem.
- **brew** (`postgresql`): devy cannot apply a port. It SHALL write no config file for it, and SHALL warn that it cannot make postgresql listen on the port with brew and that `port` must be set in the server's data directory `postgresql.conf`.

When devy writes or removes one of these files, it SHALL tell the user to restart the service if it is already running, since brew and apt servers read their config only at start.

When the port is the default and no `cli_args` are set, devy SHALL remove its own `devy.conf` (apt postgresql) or `devy.cnf` (apt mysql/mariadb) if the file's first line is `# devy-managed`, and SHALL leave any other file in place. devy SHALL also remove a devy-managed `$(brew --prefix postgresql)/etc/devy.conf` left by earlier versions. A removal that fails SHALL be a warning. Under brew mysql/mariadb, `devy.cnf` SHALL be kept at the default port, since it carries the loopback bind.

Under nix, these services SHALL receive the port through their launch arguments instead (see service-management).

#### Scenario: MySQL on apt
- **WHEN** `mysql` is declared with `port: 3307` with the apt backend and `/etc/mysql/conf.d` is writable
- **THEN** devy writes `/etc/mysql/conf.d/devy.cnf` starting with `# devy-managed` and with a `[mysqld]` section containing `port = 3307`

#### Scenario: apt config directory not writable
- **WHEN** `postgresql` is declared with `port: 5433` with the apt backend and devy runs as a user who cannot write `/etc/postgresql/16/main/conf.d`
- **THEN** `devy up` completes, warns that it could not write `/etc/postgresql/16/main/conf.d/devy.conf`, and names the line `port = 5433` to add

#### Scenario: Postgres on brew
- **WHEN** `postgresql` is declared with `port: 5433` with the brew backend
- **THEN** devy writes no `devy.conf`, exports `DATABASE_URL=postgres://localhost:5433/postgres`, and warns that it cannot make postgresql listen on 5433 with brew and that `port` must be set in the data directory's `postgresql.conf`

#### Scenario: brew my.cnf without the include
- **WHEN** `mysql` is declared with `port: 3307` with the brew backend, `$(brew --prefix)/etc/my.cnf` exists without `!includedir` for `my.cnf.d`, and `devy up` runs twice with no change to `devy.yml`
- **THEN** both runs warn that `my.cnf` must include `my.cnf.d` for port 3307 to take effect

#### Scenario: Restart needed
- **WHEN** the user changes `mysql` in `devy.yml` to `port: 3307` and runs `devy up` with the apt backend
- **THEN** devy writes the new `devy.cnf` and tells the user to restart `mysql` if it is already running

#### Scenario: Unwritable file warned on every run
- **WHEN** `mysql` is declared with `port: 3307` with the apt backend, `/etc/mysql/conf.d/devy.cnf` cannot be written, and `devy up` runs twice with no other change
- **THEN** both runs warn that port 3307 is not applied and name the lines to add

#### Scenario: Port returns to the default
- **WHEN** `/etc/postgresql/16/main/conf.d/devy.conf` is devy-managed, and the user removes `port:` from `postgresql` in `devy.yml` and runs `devy up` with the apt backend
- **THEN** devy removes `/etc/postgresql/16/main/conf.d/devy.conf`

#### Scenario: Foreign file kept
- **WHEN** `/etc/mysql/conf.d/my.cnf` exists and `mysql` is declared with `port: 3307` with the apt backend
- **THEN** devy writes `/etc/mysql/conf.d/devy.cnf`, leaves `my.cnf` unchanged, and warns that `my.cnf` is read after `devy.cnf` and may override devy's settings, without showing the content of `my.cnf`

#### Scenario: Legacy my.cnf at the default port
- **WHEN** `/etc/mysql/conf.d/my.cnf` exists and `mysql` is declared without a port or `cli_args` with the apt backend
- **THEN** `devy up` and `devy check` warn that `my.cnf` overrides the default port 3306, and `my.cnf` is left unchanged

#### Scenario: Foreign file kept at the default port
- **WHEN** `/etc/mysql/conf.d/devy.cnf` exists without a `# devy-managed` first line and `mysql` has no port or `cli_args` with the apt backend
- **THEN** devy leaves the file unchanged

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
`devy start`, `devy restart`, `devy check`, and `devy status` SHALL resolve service ports with the same precedence as `devy up`, using `devy.lock` or, in a linked worktree, `.devy/worktree.yml`. They MUST NOT assign new random ports, write `devy.lock`, or write `.devy/worktree.yml`. When a port would be randomly assigned by `devy up` but is not yet recorded, `devy check` SHALL treat it as not yet assigned and exclude it from conflict detection. `devy start` and `devy restart` SHALL instead fail with `'<name>' has no port in devy.lock yet — run `devy up` first`, or, in a linked worktree, `'<name>' has no port in this worktree yet — run `devy up` first`.

#### Scenario: Start uses the locked port
- **WHEN** the backend is nix, `devy.lock` records `assigned_port: 51000` for `redis`, and the user runs `devy start redis`
- **THEN** redis is launched on port 51000 and the health check probes port 51000

#### Scenario: Start before up
- **WHEN** the backend is nix, `devy.lock` has no entry for `redis`, and the user runs `devy start redis`
- **THEN** devy fails, telling the user to run `devy up` first, and starts nothing

#### Scenario: Start in a worktree before up
- **WHEN** the backend is nix, the project is a linked worktree, `devy.lock` records `assigned_port: 51000` for `redis`, `.devy/worktree.yml` has no entry for `redis`, and the user runs `devy start redis`
- **THEN** devy fails with `'redis' has no port in this worktree yet — run `devy up` first` and does not use port 51000

#### Scenario: Check agrees with up on unassigned ports
- **WHEN** the backend is nix, `devy.yml` declares `mysql` and `mariadb` with no ports, and `devy.lock` has no entries for them
- **THEN** `devy check` does not report a port conflict between them

### Requirement: Port applicability per backend
devy SHALL treat a service's port as applicable when the active backend runs a per-project instance of the service that it can make listen on that port:
- Under nix on macOS and Linux, every built-in service.
- Under brew, apt and winget, none. These backends run one machine-wide instance of each service, so a service without an explicit port uses its default port, and devy never assigns, reuses or records a per-project port for it. This includes `postgresql`, `mysql` and `mariadb`, whose explicit ports are handled as described in "Applying ports to database services".

When an explicit non-default port is configured for a service whose port is not applicable, `devy up` and `devy check` MUST warn that devy cannot apply the port and that the service must be configured to listen on it manually. This warning SHALL NOT be shown for `mysql` and `mariadb` under brew, or `postgresql`, `mysql` and `mariadb` under apt, where devy writes the port into a config file instead.

#### Scenario: Explicit port not applicable
- **WHEN** the backend is brew and `devy.yml` declares `redis` with `port: 6380`
- **THEN** devy uses 6380 in `REDIS_PORT` and `REDIS_URL` and warns that it cannot make redis listen on 6380 with brew

#### Scenario: Database without a port on brew
- **WHEN** the backend is brew, `devy.yml` declares `postgresql` without a port, and `devy.lock` has no entry for it
- **THEN** `devy up` uses port 5432, exports `DATABASE_URL=postgres://localhost:5432/postgres`, writes no `devy.conf`, and records no `assigned_port` for `postgresql`

#### Scenario: Database with a previously assigned port on apt
- **WHEN** the backend is apt, `devy.yml` declares `mysql` without a port, and `devy.lock` records `assigned_port: 51000` for `mysql`
- **THEN** devy uses port 3306, exports `DATABASE_URL=mysql://root@127.0.0.1:3306/`, and the rewritten `devy.lock` has no `assigned_port` for `mysql`

#### Scenario: Explicit database port on apt
- **WHEN** the backend is apt and `devy.yml` declares `mariadb` with `port: 3307`
- **THEN** devy uses 3307 in `MARIADB_PORT` and `DATABASE_URL` and does not show the "cannot make mariadb listen" warning

### Requirement: Ports for docker-managed services
A docker-managed service's port SHALL always be applicable, so it follows the full port precedence: explicit, locked, then newly assigned. The resolved port SHALL be published on the host as `127.0.0.1:<resolved port>`, mapped to the module's container port. The container port SHALL be fixed per module and MUST NOT change with the resolved port. Exported `<NAME>_PORT` and module URLs SHALL use the host port.

#### Scenario: Random port under docker with brew packages
- **WHEN** `package_manager: brew`, `service_manager: docker`, and `redis` has no port and no lock entry
- **THEN** `devy up` assigns a free port such as 51000, publishes `127.0.0.1:51000:6379`, and exports `REDIS_URL=redis://127.0.0.1:51000`

#### Scenario: Two databases on default ports
- **WHEN** `mysql` and `mariadb` are both docker-managed with no explicit ports
- **THEN** `devy check` reports no port conflict and `devy up` gives them distinct host ports

### Requirement: Rewriting a devy config file keeps its owner and group
When devy replaces an existing service config file it writes (the database config files under "Applying ports to database services" and the loopback configs), the new file SHALL have, on Unix, the same owner and group as the file it replaces, and its mode without the setuid, setgid, sticky and group/world write bits. When devy cannot give the new file that owner and group (for example a non-root user replacing a file whose owner or group they cannot assign), devy MUST leave the existing file unchanged, and `devy up` SHALL warn as for any other failed write, naming the file and only devy's lines. A newly created file is owned by the user running devy. On Windows ownership is not changed.

#### Scenario: Root rewrite keeps the group
- **WHEN** `/etc/mysql/conf.d/devy.cnf` is owned by `root:mysql` with mode `0640`, and devy runs as root with `mysql` changed to `port: 3308` with the apt backend
- **THEN** devy rewrites the file with `port = 3308`, and the new file is owned by `root:mysql` with mode `0640`

#### Scenario: Ownership cannot be kept
- **WHEN** devy must rewrite a config file whose owner or group it cannot give to the new file
- **THEN** devy leaves the existing file unchanged, and `devy up` completes with a warning naming the file and the lines to put in it
