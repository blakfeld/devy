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
At the default port with no `cli_args`, `post_setup` removes devy's apt `devy.conf` / `devy.cnf` only when the first line is `# devy-managed`; otherwise it leaves the file. For brew postgres, any devy-managed `$(brew --prefix postgresql)/etc/devy.conf` is removed on every `up` (it's never read). Removal failures are warnings.

### D6: apt mysql/mariadb write `conf.d/devy.cnf`, never `my.cnf`
Writing `/etc/mysql/conf.d/my.cnf` overwrote a user's own file whenever devy ran as root. devy now writes a file of its own, `/etc/mysql/conf.d/devy.cnf` (marker first line), and never writes or removes `my.cnf`. The marker-prefixed `my.cnf` was never released, so nothing needs migrating.

Load order: both packages' `/etc/mysql/my.cnf` (`mysql.cnf` / `mariadb.cnf`) does `!includedir /etc/mysql/conf.d/` and then the flavour's `mysql.conf.d/` / `mariadb.conf.d/`. `!includedir` reads only `*.cnf` files; both servers sort them with `strcmp` (MariaDB documents alphabetical order; MySQL's manual makes "no guarantee" but `my_dir` sorts), and the last value of an option wins. So `conf.d/my.cnf` (left by an older devy, without a marker) is read after `devy.cnf` and its `port` overrides devy's. Older devy gave apt mysql/mariadb a random port and, when run as root, wrote it to that file, so after upgrading without a `port:` devy exports 3306 while the server keeps the old port. So whenever `conf.d/my.cnf` exists, whatever the port and `cli_args`, every `up` (from `mysql_family_post_setup`, once per run, after writing or removing `devy.cnf`) warns that it is read after `devy.cnf` and overrides devy's settings and the default port 3306, and says to delete it if an older devy wrote it; `devy check` reports the same through `backend_config_warnings` (apt, non-docker). Only existence is checked (`symlink_metadata`); the file may hold credentials, so it is never read or shown.
- **Alternative:** a later-sorting name such as `zz-devy.cnf`. Rejected: MySQL documents the order as unspecified, files in `mysql.conf.d/` / `mariadb.conf.d/` override all of `conf.d/` anyway, and a user's own `my.cnf` should be able to override devy.
- **Alternative:** read `my.cnf` for a `port` line before warning. Rejected for simplicity; an existence check is enough to point at the file.

### D7: Rewrites keep the existing file's owner and group
`fs_safe::write_atomic` replaces a file with a new temporary file, so root rewriting a `root:mysql 0640` config left it `root:root`. `write_owned_config` (used by the database configs and loopback's `write_owned`) now looks up the existing regular file's uid and gid along with its mode (one `symlink_metadata`), and `fs_safe::write_atomic_with_owner` `fchown`s the temporary file to them, changing only the ids that differ, before setting the mode (a chown can clear setuid/setgid; the mode is already masked `& 0o755`) and renaming. If the chown fails, the temporary file is removed and the rename never happens, so `write_owned_config` returns `Failed` with a warning naming the owner and group it could not keep (see Risks) and ownership never changes silently. A new file is created as the caller, as before. Windows: no-op.

## Risks / Trade-offs

- [Existing brew/apt users whose `devy.lock` has a random database port see `DATABASE_URL` change to the default] → This is the fix: the old port never worked. The next `up` rewrites the lock without `assigned_port`. Note it in the README.
- [apt users who ran devy as root have a working devy-written random port in `/etc/postgresql/<N>/main/conf.d/devy.conf`] → At the default port devy removes it (marker present) and prints the restart note, so the server returns to 5432 after a restart, matching the exported URL.
- [Legacy apt `my.cnf` without a marker keeps a stale random port and overrides `devy.cnf`, or the default port 3306 that `DATABASE_URL` names when no `port:` is set] → devy can't tell it from a user's file, so it never touches it; `up` and `check` warn whenever it exists, whatever the port (D6), and the README tells users to delete it. A user's own `conf.d/my.cnf` also triggers the warning on every run; accepted, since devy can't tell the two apart and the override is real either way.
- [A non-root rewrite of a devy file whose group the user isn't in now fails] → Intended (D7): the warning names the lines, and the file keeps its ownership.
- [A non-root rewrite of a file owned by another uid in a directory the user can write also fails] → Intended (D7), since ownership must never change silently. Examples: a `my.cnf.d/devy.cnf` in the brew prefix left root-owned by an earlier `sudo devy up`, or a loopback config. Before D7 the rename silently made the user the owner; now the rewrite fails, the file is left unchanged, and devy warns (on every `up` for the database configs, once per message for loopback) naming the owner and group it couldn't keep, that the file was left unchanged, and the fix: `sudo chown` it to the user or delete it.
- [Warnings on every `up` are noisy] → They appear only while an explicit port the user asked for isn't in effect, which is exactly when the environment is wrong.

## Migration Plan

No user action for nix or docker. brew/apt database users get default ports on the next `up`; the lock entry's `assigned_port` is dropped when the lock is rewritten. Rollback is a revert: older devy would assign a new random port again.

## Decisions on review findings

Recorded as the user decided them:
1. **Machine-wide brew/apt database config is edited by whichever project runs `up` last.** Accepted for brew and apt; nix and docker run per-project instances. `default-authentication-plugin` stays allowed in `cli_args`.
2. **`create_dir_all` follows a symlinked parent directory (e.g. `my.cnf.d`).** Deferred by the user.
3. **mysql and mariadb both declared on apt with different ports share one file.** Accepted, not handled.
4. **`devy check` doesn't report an apt explicit port that can't be applied.** Out of scope; the spec doesn't require it.

## Open Questions

- Should `devy check` also flag a running brew/apt database whose listening port differs from the exported one (by probing)? Deferred: it needs a probe that doesn't misreport another project's server, and doesn't change this design.
