# Tasks

## 1. Reproduce the bugs with failing tests

- [ ] 1.1 In `src/commands/ports.rs` tests, add `brew_and_apt_databases_get_default_port_not_random`: with a `MockPackageManager` named `brew` (config dir `/opt/homebrew/etc`) and one named `apt` (config dir `/etc/postgresql/16/main/conf.d`), `resolve_ports` in `PortMode::Assign` for `postgresql`, `mysql` and `mariadb` without `port:` must return `ResolvedPort::Default(5432/3306)`, and a lock with `assigned_port: 51000` must not be reused. Verify it fails on the current code (it returns `Assigned`/`Locked`)
- [ ] 1.2 In `src/modules/helpers.rs` (or `mysql.rs`) tests, add `apt_mysql_unwritable_conf_dir_warns_instead_of_failing`: point `write_mysql_config`/`mysql_family_post_setup` for `apt` at a temp dir made read-only (on Unix; `#[cfg(unix)]`, skipped when running as root) with `port: 3307`, and assert `Ok(())` plus a warning naming the file and `port = 3307`. Add the same for postgres `post_setup` with `port: 5433`. Verify both fail now with a permission error
- [ ] 1.3 Add `brew_mysql_include_warning_repeats`: with a temp brew prefix whose `my.cnf` lacks `!includedir`, call the brew mysql write twice with `port: 3307` and assert the warning is produced both times (capture through the warning collector used by existing helpers tests). Verify the second call is silent today
- [ ] 1.4 Add `brew_postgres_writes_no_keg_config`: `PostgresModule::post_setup` with a brew mock whose config dir is a temp keg `etc` and `port: 5433` must create no `devy.conf`, and `unapplied_port_warning` must return the "cannot make postgresql listen" warning with the `postgresql.conf` hint. Verify it fails now (file is written, no warning)
- [ ] 1.5 Add `default_port_removes_devy_managed_db_config`: an apt temp conf dir holding a `# devy-managed` `devy.conf` (postgres) or `my.cnf` (mysql) with an old port, and `post_setup` at the default port, must remove it; a `my.cnf` without the marker must be left byte-identical. Verify the removal case fails now

## 2. Port applicability

- [ ] 2.1 Remove the `port_applicable` overrides from `src/modules/postgres.rs`, `mysql.rs` and `mariadb.rs` so the trait default (nix only) applies; keep `dep.docker ||` in `ports::port_applicable`. Update `port_applicable_per_backend` in `src/modules/mod.rs` so postgres/mysql/mariadb are not applicable under brew or apt. Verify test 1.1 passes and existing nix/docker port tests still pass
- [ ] 2.2 Add `Module::explicit_port_via_config(&self, pm) -> bool` (default `false`; mysql/mariadb `true` for brew and apt, postgres `true` for apt) and an optional `unapplied_port_hint(pm)` (postgres under brew: set `port` in the data directory's `postgresql.conf`). Make `unapplied_port_warning` return `None` when `explicit_port_via_config` is true and append the hint otherwise. Verify with `ports.rs` tests: apt mariadb `port: 3307` gives no warning, brew postgres `port: 5433` warns with the hint, brew redis `port: 6380` keeps today's message
- [ ] 2.3 Verify with a `ports.rs`/`up.rs` test that `recordable_port` and `worktree_ports_for` record nothing for brew/apt databases, including with an explicit port, and that `project_env` exports `DATABASE_URL` with 5432/3306 for brew/apt databases without a port and no lock entry

## 3. Applying explicit ports safely

- [ ] 3.1 Move `loopback::write_owned`'s write-on-change logic into a shared helper (e.g. `modules::helpers::write_owned_config`) that returns `Changed`, `Unchanged` or `Failed(warning)`, keeps the existing mode, and prints "restart the service if it is already running" on change. Keep loopback's warn-once behaviour on top of it. Verify existing `loopback.rs` tests pass unchanged
- [ ] 3.2 Route apt postgres `devy.conf` and apt mysql/mariadb `my.cnf` writes through it. Prefix apt `my.cnf` with `# devy-managed`. On failure, warn on every `up` naming the file and only devy's lines (never the existing file's content), and return `Ok`. Verify tests 1.2 pass and an apt `my.cnf` test asserts the marker line
- [ ] 3.3 In `write_mysql_config` (brew), drop the `changed` gate on the `!includedir` warning, so it fires whenever settings are customized and `my.cnf` lacks the include. Verify test 1.3 passes and that no warning appears at defaults or when the include is present
- [ ] 3.4 Add `Module::backend_config_warnings(dep, pm)` (default empty), implement it for mysql/mariadb under brew using the same read-only include check, and chain it in `src/commands/check.rs` next to `unapplied_port_warning`. Verify with a `check.rs` test over a temp brew prefix that `devy check` reports the missing include and writes nothing
- [ ] 3.5 In `PostgresModule::post_setup` under brew, write nothing; remove a `# devy-managed` `$(brew --prefix postgresql)/etc/devy.conf` if present (failure is a warning). Verify test 1.4 passes and a test with a stale devy-managed keg `devy.conf` sees it removed

## 4. Stale config removal

- [ ] 4.1 At the default port with no `cli_args`, remove apt postgres `devy.conf` and apt mysql/mariadb `my.cnf` only when the first line is `# devy-managed`, print the restart note on removal, and turn removal errors into warnings. Keep brew `my.cnf.d/devy.cnf` (loopback bind). Verify test 1.5 passes, plus a read-only-dir removal test that warns instead of failing

## 5. Docs and specs

- [ ] 5.1 Update `README.md`'s ports section (the backend table row "Homebrew, apt | `postgresql`, `mysql` and `mariadb` only…" and the precedence list) to say brew/apt databases use default ports, explicit ports are written to apt `conf.d` / brew `my.cnf.d` when possible, brew postgres needs manual configuration, and a stale pre-marker apt `my.cnf` must be deleted by hand. Verify `rg -n "conf.d|devy.conf" README.md` matches the new behaviour

## 6. Integration

- [ ] 6.1 Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` and `openspec validate fix-system-database-ports --strict`, all clean
- [ ] 6.2 Manually, on macOS with brew: `devy up` with `postgresql` and `mysql` and no ports exports 5432/3306 and writes no `devy.conf`; on a Debian/Ubuntu VM as a non-root user, `devy up` with `mysql` `port: 3307` completes with the "could not write /etc/mysql/conf.d/my.cnf" warning
