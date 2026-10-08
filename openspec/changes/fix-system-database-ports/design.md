# Design

## Context

See proposal.md (Why). The current state that shapes the approach:

- **One flag, two meanings.** `Module::port_applicable(pm)` (`src/modules/mod.rs`, overridden in `postgres.rs`, `mysql.rs`, `mariadb.rs` to `pm.name() == "nix" || pm.service_config_dir(..).is_some()`) is read through `ports::port_applicable` (`src/commands/ports.rs`) in five places. Some ask "may devy give this service a per-project port?" (`resolve_one`'s locked/assigned branch, `recordable_port`, `worktree_ports_for`, `project_env`'s unassigned case, `shared_fixed_port_warning`). One asks "does devy apply an explicit port?" (`unapplied_port_warning`). For brew/apt databases the honest answers differ: no per-project port, but an explicit port can sometimes be written to config.
- **Config dirs.** brew's `service_config_dir("postgresql")` is `$(brew --prefix postgresql)/etc` (the keg), which the formula's `postgres -D var/postgresql@N` never reads. brew mysql/mariadb use `$(brew --prefix)/etc`. apt returns `/etc/mysql/conf.d` and `/etc/postgresql/<N>/main/conf.d`, which Debian's stock configs include but which only root (or `postgres`) can write.
- **Failure path.** `up.rs` wraps `post_setup` with `post_setup failed for <name>` and `?`, so any write error aborts `up`.
- **Prior art.** `src/modules/loopback.rs` already has the pattern this change needs: files carry `# devy-managed` (`DEVY_MARKER`) on line one, `write_owned` writes only on change, prints "restart the service if it is already running" on success, and turns write failures into warnings that name only the lines to add.

## Goals / Non-Goals

**Goals:**
- `DATABASE_URL` and `<NAME>_PORT` always name the port the brew/apt server actually listens on, or devy says loudly why it may not.
- `devy up` never fails because `/etc` isn't writable.
- devy cleans up the files it owns when they're no longer needed.

**Non-Goals:**
- Running `sudo` to write `/etc`. devy never escalates for config files.
- Restarting brew/apt services automatically. They're machine-wide and may serve other projects.
- Editing the user's `my.cnf` or the brew postgres data directory's `postgresql.conf`.
- Per-project isolation of brew/apt databases (see the worktree spec: these services are shared).

## Decisions

### D1: brew/apt databases never get a per-project port
`postgresql`, `mysql` and `mariadb` drop their `port_applicable` overrides and use the trait default (`is_service() && pm.name() == "nix"`). Docker-managed deps keep `dep.docker ||` in `ports::port_applicable`. So without `port:` they use 5432/3306, record nothing, and match every other brew/apt service.
- **Alternative:** keep random ports where devy's file is read and writable (apt as root, brew mysql with the include). Rejected: the config is machine-wide, so two projects with different random ports overwrite each other and the server (never restarted) listens on neither project's port reliably. A random port only makes sense for a per-project instance.

### D2: A separate module hook for "explicit port written to config"
Add `Module::explicit_port_via_config(&self, pm) -> bool` (default `false`). mysql/mariadb return `true` for brew and apt; postgres returns `true` for apt only. `unapplied_port_warning` returns `None` when it is true; every other `port_applicable` caller is unchanged. For brew postgres, `unapplied_port_warning` fires as for redis, with an extra hint naming the data directory's `postgresql.conf` (a module-provided hint string, so ports.rs stays generic).
- **Alternative:** make `port_applicable` return an enum (`PerProject` / `ExplicitOnly` / `No`). Cleaner in theory but touches every caller and every module for one special case.
- **Alternative for brew postgres:** write `include_dir` into the data dir's `postgresql.conf`, or run `ALTER SYSTEM SET port`. Both edit a file devy doesn't own (or need a running server and credentials), so they're rejected.

### D3: Reuse loopback's owned-file writer for database configs
Generalize `loopback::write_owned` (or extract it to `helpers`) so the database paths use it: marker on line one, write only when content differs, "restart if running" note on change, and write failures returned as a warning instead of an error. Database configs differ from loopback in one way: the warning is shown on **every** `up` while the file doesn't hold devy's settings (not warn-once), because the exported port is wrong until the user acts. The warning names only the lines to add (the existing file may hold the user's credentials).
- **Alternative:** detect whether the service is running (`pm.is_service_running`) and only then warn about a restart. Rejected for simplicity and consistency with loopback; the note says "if it is already running".

### D4: brew mysql include warning on every run
`write_mysql_config` currently gates the `!includedir` warning on `changed`. Drop that gate: warn whenever settings are customized and `my.cnf` doesn't include `my.cnf.d`. `devy check` gets the same check, read-only (it reads `my.cnf`, never writes). `Module::config_warnings(dep)` has no package manager, so add `Module::backend_config_warnings(dep, pm)` (default empty), implemented by mysql/mariadb and chained in `check.rs` next to `unapplied_port_warning`.

### D5: Stale-file removal is marker-gated
At the default port with no `cli_args`, `post_setup` removes devy's apt `devy.conf` / `my.cnf` only when the first line is `# devy-managed`; otherwise it leaves the file. apt `my.cnf` gains the marker so new files qualify. Files written by earlier devy versions (apt `my.cnf` without a marker) are therefore never removed; this is the safe choice since devy can't prove it owns them. For brew postgres, any devy-managed `$(brew --prefix postgresql)/etc/devy.conf` is removed on every `up` (it's never read). Removal failures are warnings.

## Risks / Trade-offs

- [Existing brew/apt users whose `devy.lock` has a random database port see `DATABASE_URL` change to the default] → This is the fix: the old port never worked. The next `up` rewrites the lock without `assigned_port`. Note it in the README.
- [apt users who ran devy as root have a working devy-written random port in `/etc/postgresql/<N>/main/conf.d/devy.conf`] → At the default port devy removes it (marker present) and prints the restart note, so the server returns to 5432 after a restart, matching the exported URL.
- [Legacy apt `my.cnf` without a marker keeps a stale random port] → devy can't tell it from a user's file. Accepted; the README tells users to delete it.
- [Warnings on every `up` are noisy] → They appear only while an explicit port the user asked for isn't in effect, which is exactly when the environment is wrong.

## Migration Plan

No user action for nix or docker. brew/apt database users get default ports on the next `up`; the lock entry's `assigned_port` is dropped when the lock is rewritten. Rollback is a revert: older devy would assign a new random port again.

## Open Questions

- Should `devy check` also flag a running brew/apt database whose listening port differs from the exported one (by probing)? Deferred: it needs a probe that doesn't misreport another project's server, and doesn't change this design.
