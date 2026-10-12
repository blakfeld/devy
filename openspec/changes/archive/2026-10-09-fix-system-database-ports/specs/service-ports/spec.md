## MODIFIED Requirements

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

## ADDED Requirements

### Requirement: Rewriting a devy config file keeps its owner and group
When devy replaces an existing service config file it writes (the database config files under "Applying ports to database services" and the loopback configs), the new file SHALL have, on Unix, the same owner and group as the file it replaces, and its mode without the setuid, setgid, sticky and group/world write bits. When devy cannot give the new file that owner and group (for example a non-root user replacing a file whose owner or group they cannot assign), devy MUST leave the existing file unchanged, and `devy up` SHALL warn as for any other failed write, naming the file and only devy's lines. A newly created file is owned by the user running devy. On Windows ownership is not changed.

#### Scenario: Root rewrite keeps the group
- **WHEN** `/etc/mysql/conf.d/devy.cnf` is owned by `root:mysql` with mode `0640`, and devy runs as root with `mysql` changed to `port: 3308` with the apt backend
- **THEN** devy rewrites the file with `port = 3308`, and the new file is owned by `root:mysql` with mode `0640`

#### Scenario: Ownership cannot be kept
- **WHEN** devy must rewrite a config file whose owner or group it cannot give to the new file
- **THEN** devy leaves the existing file unchanged, and `devy up` completes with a warning naming the file and the lines to put in it
