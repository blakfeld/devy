# Tasks

## 1. Quick fixes

- [x] 1.1 Add `global_packages` to typescript `known_extra_keys` (`src/modules/typescript.rs`). Verify a new unit test confirms `check` reports no unrecognized key for `typescript` with `global_packages`.
- [x] 1.2 Fix the trailing `}}` in `shell.nix` generation (`src/commands/export.rs`). Verify a unit test asserts balanced braces and that the output ends with `}\n`. If `nix-instantiate` is on PATH, also verify `nix-instantiate --parse` on generated output succeeds.
- [x] 1.3 Add `pr`, `export` (`--format` → `shell flake`) and `up --bootstrap` to the zsh, bash and fish completion templates (`src/commands/hook.rs`). Verify unit tests assert each snippet contains `pr`, `export`, `--bootstrap`, `shell flake`, and that the existing `tests/cli.rs` hook tests pass.
- [x] 1.4 Remove the ruby `gems:` example and the `Supports gems` table cell from `README.md`. Verify `grep -n gems README.md` returns nothing.

## 2. Shared port resolution

- [x] 2.1 Add `Module::port_applicable(pm)`: true under nix for built-in services, plus postgres/mysql/mariadb when `pm.service_config_dir` is `Some`. Verify unit tests cover the nix, brew, apt and winget combinations for redis and postgresql.
- [x] 2.2 Extract port resolution from `src/commands/up.rs` into a shared helper with `Assign` and `ReadOnly` modes. The precedence is explicit → locked (if applicable) → assigned or unassigned (if applicable) → default. Verify the existing `resolve_service_ports_*` tests are moved, still pass, and that new tests cover the backend-cannot-apply case falling back to the default port and ignoring the locked port.
- [x] 2.3 Make the conflict check operate on resolved ports and skip unassigned ones. Verify unit tests for: nix mysql+mariadb with no ports and no lock (no conflict), brew elasticsearch+opensearch (conflict on 9200), and a locked-port collision (conflict).
- [x] 2.4 Use the shared resolver in `up`, `check`, `status` and `service.rs` (start/stop/restart), in read-only mode everywhere except `up`. Verify a unit test confirms `devy start` health-checks the locked port, and that `check`/`status` never write `devy.lock`.
- [x] 2.5 Warn on an explicit non-default port the backend can't apply, through `config_warnings` (for `check`) and during `up`, and stop writing `assigned_port` for non-applicable services. Verify unit tests for the brew + redis `port: 6380` warning and the lock contents.
- [x] 2.6 Update the `tests/cli.rs` mysql+mariadb conflict test to a case that still conflicts (e.g. explicit identical ports), and add a nix-backend no-conflict case if the test harness can select nix without installing. Verify with `cargo test --test cli`.
- [x] 2.7 Update the README ports section to describe random ports under nix, the database conf.d support on brew/apt, and default ports otherwise. Verify the README examples match the spec's scenarios.

## 3. Nix service launch specs

- [x] 3.1 Introduce `LaunchSpec`/`InitStep` and `Module::nix_launch(dep, data_dir)`, replacing `service_exec_name`. Change `PackageManager::start_service` to accept the launch spec and project root, and update brew, apt, winget and the test doubles. Verify `cargo build` and the existing tests pass.
- [x] 3.2 Nix backend: create `.devy/data/<canonical>`, run the init step once (gated on its marker, failing with `Failed to initialize <dep>`), and write the plist (`ProgramArguments` + `EnvironmentVariables`) or systemd unit (`ExecStart` args + `Environment=`) with correct escaping, rewriting and reloading on every start. Verify unit tests on the generated plist and unit text, including escaping of spaces and quotes, and that init is skipped when the marker exists.
- [x] 3.3 Implement `nix_launch` for postgresql (initdb, `-p`, `-k`, `listen_addresses`), mysql (`--initialize-insecure`, sanitized `cli_args`) and mariadb (`mariadb-install-db`). Verify per-module unit tests on args and init markers.
- [x] 3.4 Implement `nix_launch` for redis, memcached, rabbitmq (env vars), elasticsearch, opensearch, meilisearch (`master_key`), minio (`console_port`, credentials env) and mailhog. Verify per-module unit tests on args and env.
- [x] 3.5 Implement `nix_launch` for nginx (generated `daemon off;` config) and vault (dev mode args, or generated `vault.hcl`). Verify unit tests on the generated config contents.
- [x] 3.6 Implement `nix_launch` for mongodb (`mongod`; set nix attr to `mongodb-ce` per design, falling back to `mongodb`). Verify a unit test on args, and confirm `nix eval nixpkgs#mongodb-ce.name` resolves (or record the fallback in design.md).
- [x] 3.7 Implement Kafka under nix: generated `server.properties` (KRaft with a persisted controller port, plus a storage-format init step), always KRaft because nixpkgs Kafka 4 has no ZooKeeper. Verify unit tests for KRaft with and without `kraft: true`, that zookeeper is never started under nix, and confirm the `apacheKafka` profile ships `kafka-server-start.sh` and `kafka-storage.sh`.
- [x] 3.8 Limit the "not yet supported with the nix backend" error to generic-module services. Verify a unit test confirms every built-in service returns `Some` from `nix_launch` and the generic module errors.
- [x] 3.9 Extend `.github/workflows/integration.yml` nix jobs (Linux and macOS) to declare `redis` and `postgresql`, run `devy up`, and assert `redis-cli -p $REDIS_PORT ping` and `pg_isready -h 127.0.0.1 -p $POSTGRESQL_PORT` succeed, then `devy down`. Verify the workflow passes on a branch push or manual dispatch.

## 4. Versioned nix attributes

- [x] 4.1 Add a serde-skipped `version_from_lock` flag to `Dependency`, set where `up` applies lock pinning. Verify a unit test confirms an explicit version leaves it false and a lock-applied version sets it true.
- [x] 4.2 Add `Module::nix_versioned_attr(version)` with allowlisted mappings, checked against current nixpkgs, for node/typescript, python, postgresql, mysql, java, dotnet and go. Verify table-driven unit tests cover the short form, full version and unsupported versions for each.
- [x] 4.3 Use the versioned attr in the nix `pm_dep` path and in `nix_attr` (export). Verify unit tests confirm `nix profile install` receives `nixpkgs#nodejs_22` and `devy export` emits `pkgs.nodejs_22`.
- [x] 4.4 Make the nix installed check match the exact attr devy would install, including versioned names and mapped names like `mysql80`/`jdk21`. Verify unit tests with profile JSON fixtures for a version match, a version change (not installed) and an unversioned match.
- [x] 4.5 Emit the version-not-supported warning in `up` and `check` only for explicit unmapped versions. Verify unit tests for the explicit `jq 1.6` warning and for no warning on a lock-pinned version.
- [x] 4.6 Document nix version behavior in the README `devy.yml` reference. Verify the README states which modules honor versions under nix.

## 5. Integration checks

- [x] 5.1 Run `cargo test`, `cargo clippy -- -D warnings` and `cargo fmt --check`. Verify all pass.
- [x] 5.2 Run `openspec validate fix-baseline-spec-gaps --strict`. Verify the change is valid.
- [x] 5.3 Manually smoke-test on macOS with nix: a project with `redis`, `postgresql`, `mysql`, `mariadb` (no ports) and `kafka` (`kraft: true`). Verify `devy check` passes after `devy up`, every `*_PORT` env var is reachable, and `devy restart redis` keeps the same port.
  - Done (2026-10-02, macOS, Nix 2.34). redis, postgresql, mysql and kafka (`kraft: true`): `devy check` passes, clients connect on every locked port (127.0.0.1 only), `devy restart redis` keeps its port, and a second `devy up` re-initializes nothing and leaves `devy.lock` unchanged. mysql + mariadb together: the first attempt hit a nix profile file conflict, fixed by preferring mariadb (`--priority 4`) and running MySQL's own `mysqld`. Both then start and report 8.4.11 and 11.4.12-MariaDB, and `devy check` passes.
