# Proposal

## Why

Under the brew and apt backends, `postgresql`, `mysql` and `mariadb` without an explicit `port:` get a random port that the running server never uses, or `devy up` fails outright:

- **Random ports for machine-wide servers.** `port_applicable` is true for these modules whenever the backend has a service config directory, so port resolution assigns them a random port, records it in `devy.lock`, and exports it in `DATABASE_URL` and `<NAME>_PORT`. But brew and apt run one system-wide server per machine. Two projects with different random ports overwrite each other's config, and devy never restarts the running server, so it stays on whatever port it started with.
- **apt: `devy up` aborts for non-root users.** devy writes `/etc/mysql/conf.d/my.cnf` or `/etc/postgresql/<N>/main/conf.d/devy.conf` without sudo. The write fails with a permission error, `post_setup failed for <name>` is returned, and `up` stops. The loopback configs for Kafka and RabbitMQ already turn the same kind of `/etc` write failure into a warning.
- **brew postgresql: the file is never read.** devy writes `$(brew --prefix postgresql)/etc/devy.conf`. The formula's service runs `postgres -D $(brew --prefix)/var/postgresql@<N>`, whose `postgresql.conf` doesn't include that file. The server stays on 5432 while `DATABASE_URL` points at the random port.
- **brew mysql/mariadb: silently ignored.** Homebrew's stock `my.cnf` has no `!includedir my.cnf.d`, so `devy.cnf` is ignored. devy warns only when it rewrites `devy.cnf`, so every later `up` is silent.
- **Stale config.** When the port goes back to the default, devy leaves its old `devy.conf` / `my.cnf` with the previous port in place.

## What Changes

- **No automatic ports for system-wide databases.** Under brew and apt, `postgresql`, `mysql` and `mariadb` without an explicit port use their default port (5432 / 3306), like every other brew/apt service. devy no longer assigns or reuses a recorded random port for them, and stops recording `assigned_port` for them in `devy.lock` or `.devy/worktree.yml`. Nix and docker-managed services are unchanged.
- **Explicit ports are applied only where devy's file takes effect.**
  - apt (all three): devy writes its `conf.d` file as today, except that mysql/mariadb use a devy-owned `/etc/mysql/conf.d/devy.cnf` instead of `my.cnf`, so devy (when run as root) never overwrites a user's `my.cnf`. A `my.cnf` there is read after `devy.cnf` and overrides it (and the default port 3306 that an upgrade without `port:` falls back to, where an older devy running as root wrote its random port), so `up` and `devy check` warn whenever one exists, whatever the port, and say to delete it if an older devy wrote it. A write that fails (for example a permission error under `/etc` without root) becomes a warning (on every `up` while the setting isn't in place) naming the file and the lines to add, not a failed `up`.
  - brew mysql/mariadb: devy writes `my.cnf.d/devy.cnf` as today. While `my.cnf` doesn't include `my.cnf.d` and devy's settings differ from the defaults, devy warns on every `up` (not only when `devy.cnf` changes) and `devy check` reports it.
  - brew postgresql: devy can't apply the port. It warns with the existing "cannot make postgresql listen on port N with brew" message, and tells the user to set `port` in the data directory's `postgresql.conf`.
- **Restart notice.** When devy writes or removes a database config file under brew or apt, it tells the user to restart the service if it is already running.
- **Stale files removed.** When the port returns to the default (and no `cli_args` are set), devy removes its own apt `devy.conf` / `devy.cnf` if the file is devy-managed. devy also removes the unused `$(brew --prefix postgresql)/etc/devy.conf` it wrote before. Under brew mysql/mariadb, `devy.cnf` stays (it carries the loopback bind) with the default port. devy's apt `devy.cnf` starts with the `# devy-managed` line so devy can tell it apart from a user's file.
- **Ownership kept on rewrite.** When devy replaces one of its existing config files (database configs and the loopback configs), the new file keeps the old one's owner and group, so root rewriting a `root:mysql 0640` file no longer leaves it `root:root`. If the ownership can't be kept, the file is left unchanged and devy warns.

## Capabilities

### New Capabilities

None.

### Modified Capabilities
- `service-ports`: port applicability per backend, port precedence scenarios, and how database ports are applied under brew and apt (failure handling, brew postgresql, include warning, restart notice, stale file removal).

## Impact

- `src/modules/postgres.rs`, `src/modules/mysql.rs`, `src/modules/mariadb.rs`: `port_applicable` and `post_setup`.
- `src/modules/helpers.rs`: `write_mysql_config` / `mysql_family_post_setup` (apt `devy.cnf`, marker line, warning on every run, legacy `my.cnf` warning, non-fatal write failures, removal at defaults) and `write_owned_config` (keeps owner and group).
- `src/fs_safe.rs`: `write_atomic_with_owner`, which chowns the temporary file before the rename.
- `src/commands/ports.rs`: `unapplied_port_warning` must distinguish "can assign a per-project port" from "can apply an explicit port".
- `src/commands/check.rs`: report a brew `my.cnf` that doesn't include `my.cnf.d`, and an apt `conf.d/my.cnf` that overrides devy's settings.
- `src/modules/mod.rs`, `src/commands/ports.rs` tests and `README.md` (the "Ports" table that lists Homebrew/apt database support).
- Existing projects on brew/apt that already have an `assigned_port` for a database in `devy.lock` switch to the default port on the next `up`; the stale lock entry is dropped when the lock is rewritten.
