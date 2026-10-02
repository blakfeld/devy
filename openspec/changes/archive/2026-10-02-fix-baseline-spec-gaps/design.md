# Design

## Context

See proposal.md (Why). Current structure relevant to the fix:

- **Port resolution.** `resolve_service_ports` and `check_port_conflicts` live in `src/commands/up.rs`. `check.rs` calls only the conflict check, and does so on raw deps. `service.rs` (start/stop/restart) and `status.rs` resolve nothing, so they fall back to `extra_port(dep, key, default)`.
- **Package manager service API.** `PackageManager::start_service(&self, name: &str)` takes only a name.
  - The nix implementation (`src/package_manager/nix.rs`) looks up `modules::get(name).service_exec_name()` and writes a launchd plist or systemd unit with that binary and no arguments or environment.
  - Brew, apt and winget delegate to the system service manager, which uses the package's own config.
- **Module service hooks.** Service modules implement `start/stop/is_running/health_check/env_vars/post_setup`. postgres, mysql and mariadb `post_setup` write conf.d files via `pm.service_config_dir()`.
- **Nix attributes.**
  - Nix installs `nixpkgs#<attr>`, where `<attr>` is the `pm_dep` package name. The version is dropped.
  - `Module::nix_attr` is used only by `export`.
  - The installed check matches profile entries by name or attr-path tail.
- **Completion and export.** `hook.rs` has static completion templates per shell. `export.rs` builds `shell.nix` with string pushes and ends with a literal `"}}\n"`.

## Goals / Non-Goals

**Goals:**
- A single port-resolution function used by `up`, `check`, `status`, `start`, `stop` and `restart`.
- Nix-run services are fully self-contained per project, under `.devy/data/`, and listen where the env vars say.
- Versioned nix installs are deterministic and recognized by the installed check.

**Non-Goals:**
- **Per-project namespacing of nix service unit names.** These are still `sh.devy.<name>` and `devy-<name>`, so two projects can't run the same service under nix concurrently. Tracked separately.
- **Port wiring for brew/apt services beyond postgres/mysql/mariadb.** Editing shared, non-drop-in config files such as `redis.conf` is rejected below.
- **Winget port application.**
- **Pinning nixpkgs itself** (flake lock / channel). The versioned attribute is the pin.
- **Migrating existing nix data.** Services previously ran with their binary defaults, i.e. data in the binary's default location, which was usually broken for databases anyway.

## Decisions

### D1. A `ServicePlan` resolved once, shared by all commands
Move port resolution into a module-level helper, e.g. `src/commands/ports.rs`, roughly:
`resolve_ports(deps, lock, pm, mode) -> Vec<ResolvedPort>`, where `mode` is `Assign` (for `up`) or `ReadOnly` (all other commands).
- `ResolvedPort` is `Explicit(u16) | Locked(u16) | Assigned(u16) | Default(u16) | Unassigned`.
- The applicability check (`module.port_applicable(pm)`) decides whether `Locked`, `Assigned` and `Unassigned` are reachable, or whether resolution falls to `Default`.
- The conflict check skips `Unassigned`.
- The resolved port is written into `dep.extra[port_key]`, exactly as today, so `env_vars`, `health_check` and `post_setup` keep working unchanged.

*Alternative considered:* have `check` simulate random assignment. Rejected, because the result is non-deterministic and could hide real conflicts.

### D2. Port applicability lives on the module, keyed by backend
Add `fn port_applicable(&self, pm: &dyn PackageManager) -> bool` to `Module`.
- **Default:** `pm.name() == "nix"` for services that have a nix launch spec.
- **postgres/mysql/mariadb:** additionally return true when `pm.service_config_dir(..)` is `Some`.

Warnings for an explicit, non-applicable, non-default port come from `config_warnings` (seen by `check`) and the same helper during `up`.

*Alternative considered:* a static matrix in the PM. Rejected: modules already own their per-PM package names, and this keeps knowledge in one place.

### D3. Nix launch spec instead of a bare exec name
Replace `service_exec_name() -> Option<&str>` with:

```text
fn nix_launch(&self, dep, data_dir) -> Option<LaunchSpec>
LaunchSpec { exec, args: Vec<String>, env: Vec<(String, String)>,
             init: Option<InitStep> }
InitStep   { marker: PathBuf, cmd: Vec<String> }
```

- **API change:** change `PackageManager::start_service` to take `(&self, name, launch: Option<&LaunchSpec>)`. Brew, apt and winget ignore the launch spec, and nix requires it. `nix_launch` returns `Result<Option<LaunchSpec>>`, because modules that generate config files can fail to write them.
- **Project root:** `Module::start` gains a `project_root` parameter. A shared `start_via_pm` helper creates `<root>/.devy/data/<canonical>` and calls `nix_launch` only when the backend is nix, so other backends never get a data dir. The nix PM already knows the root through its profile path, so `start_service` doesn't take it.
- **Init:** the nix backend runs `init` synchronously when `marker` doesn't exist, failing with `Failed to initialize <dep>`.
- **Generated configs:** nginx.conf, vault.hcl and kafka server.properties are written by the module before it returns the spec, and rewritten each start so port changes apply.
- **Unit generation:**
  - Both units also get `PATH=<profile bin>:/usr/bin:/bin:/usr/sbin:/sbin` (unless the spec sets PATH), so wrapper scripts such as Kafka's and RabbitMQ's find their helpers. The init step runs with the same environment.
  - Plist `ProgramArguments` = `[bin, args…]`, plus an `EnvironmentVariables` dict.
  - systemd `ExecStart` gets args quoted per systemd rules, and `Environment="K=V"` lines.
  - Both are XML- or systemd-escaped.
- **Data dir:** `<root>/.devy/data/<canonical>`. The project root is threaded through the `start` call, since modules currently don't see it.

*Alternative considered:* keep `start_service(name)` and look the spec up inside nix.rs, as is done today. Rejected because nix.rs would need the dep config and project root, and the global `modules::get(name)` lookup is what made the bug invisible.

### D4. Unit files rewritten on every start
Always rewrite the plist or unit and reload (`launchctl unload` if loaded, then `load`; `systemctl --user daemon-reload`) before starting. This makes port and config changes take effect, and upgrades existing argument-less units on the next start.

### D5. Kafka under nix uses the distribution scripts, always in KRaft mode
nixpkgs `apacheKafka` ships the Kafka distribution's `bin/*.sh` scripts.
- **KRaft mode:**
  - Generate `server.properties` with `process.roles=broker,controller`, a controller listener on a second free port (persisted inside the generated file and reused if present), and `log.dirs=<data>/logs`.
  - Init step: `kafka-storage.sh format -t <generated uuid> -c server.properties`, with marker `<data>/logs/meta.properties`.
- **No zookeeper mode under nix.** nixpkgs `apacheKafka` is 4.x (4.3.1 when this was implemented). Kafka 4 removed ZooKeeper, so the package has no `zookeeper-server-start.sh`, and `apacheKafka_3_9` no longer evaluates. Under nix, devy runs KRaft regardless of `kraft`, prints an info line when `kraft` isn't set, and never starts or stops `zookeeper`. Zookeeper mode is unchanged on brew, apt and winget.

### D6. Versioned nix attributes
Add `fn nix_versioned_attr(&self, version: &str) -> Option<String>` to `Module`.
- **Default:** None.
- **Mappings:** implemented per module as listed in the dependency-modules delta. Each one parses `major` or `major.minor` and checks it against a small allowlist of versions known to exist in nixpkgs-unstable, so typos fall back with a warning instead of a failed install. The allowlists were checked against nixpkgs when this was implemented. `nodejs_20`, `mysql80`, `go_1_22`–`go_1_25`, `jdk23`/`jdk24` and `postgresql_13` no longer evaluate there, so they're excluded.
- **Who uses it:**
  - `pm_dep` for nix: the attr becomes the package name.
  - `nix_attr`, so `export` matches.
  - The nix installed check, which compares against the full attr name.
- **Where the version came from:** `up` currently overwrites `dep.version` from the lock, so a flag is needed to tell an explicit version from a lock-pinned one. Add `version_from_lock: bool` to `Dependency` (not part of any file format), set where the lock is applied. The warning fires only when `version.is_some() && !version_from_lock && attr.is_none()`, and never for modules that install outside the package manager (`source()` is set, e.g. rustup or rbenv).
- **Installed check matches the attribute path.** Nix 2.20+ profile JSON (v3) has no `pname` or `version`, only `attrPath`. The check matches the attribute-path tail exactly, falling back to `pname` (and then the element name) only for entries without one. This also fixes mapped names whose pname differs (`mysql84` → `mysql`, `jdk21` → `openjdk`), which were never recognized as installed.
- **Lock-pinned versions don't force a reinstall.** The first `devy up` installs the unversioned attribute (e.g. `postgresql`) and locks its version. The next run pins that version, which now maps to `postgresql_18`. Installing that next to `postgresql` would conflict in the profile. So when the version came from the lock and the versioned attribute isn't installed, the unversioned attribute installed at exactly the locked version also counts as installed.
- **Unversioned defaults.** `mysql` defaults to `mysql84` (nixpkgs dropped `mysql80`), `rabbitmq` installs `rabbitmq-server` (the old `rabbitmq` attribute doesn't exist), and `mongodb` installs `mongodb-ce`.

### D7. Trivial fixes
- **typescript:** add `"global_packages"` to `known_extra_keys`.
- **hook.rs:** add `pr` and `export` to the subcommand lists for all three shells, `--bootstrap` to the `up` flags, and `--format` with values `shell flake` for `export`.
- **export.rs:** replace the final `"}}\n"` with `"}\n"`, and add a unit test that counts balanced braces.
- **README:** delete the ruby `gems:` example and table cell. Rewrite the ports section to describe applicability per backend.

### D8. Hardening from implementation review
- **Short socket paths.** `<root>/.devy/data/postgresql/.s.PGSQL.<port>` already exceeds macOS's 103-byte `sun_path` in moderately nested checkouts. Sockets stay in the data dir when the path fits in 100 bytes, and otherwise move to `/tmp/devy-<fnv1a(data dir)>`. FNV is used because `DefaultHasher` isn't stable across Rust releases.
- **MySQL family.** `--no-defaults` (first argument) keeps `/etc/my.cnf`, devy's own apt `conf.d` and `~/.my.cnf` out. mysqld gets `--mysqlx=OFF` so the X plugin doesn't bind `*:33060`. MariaDB is initialized with `--auth-root-authentication-method=normal` so root can log in over TCP.
- **RabbitMQ.** The distribution port defaults to the node port + 20000, which overflows for OS-assigned ports. A second free port is persisted in `<data>/dist_port` and passed as `RABBITMQ_DIST_PORT`. `RABBITMQ_NODENAME=devy-<hash>@localhost` keeps the node apart from a system broker.
- **Working directory.** Units and init steps run in the data dir (`WorkingDirectory`). Kafka gets `LOG_DIR=<data>/app-logs`, because `kafka-run-class.sh` otherwise logs into the read-only store.
- **Lock versions from another backend are ignored.** `up` applies a lock version only when the entry's `source` matches the active one. A Homebrew-resolved `22.11.0` would otherwise make nix install `nodejs_22`, and then flip back to `nodejs` once the lock was rewritten.
- **MariaDB is preferred over MySQL in the profile.** `mysql84` and `mariadb` share ~20 binary names, so the second install failed with a profile conflict. `mariadb` is installed at `--priority 4` (default 5, lower wins), so its tools own the shared names in any install order. mariadb's `bin/mysqld` is a symlink to `mariadbd`, so the mysql launch spec sets `exec_package: mysql84`. The nix backend then resolves `mysqld` (service and init) from the first of that package's store outputs that has a `bin/`. Store paths aren't ordered, and `-man` may come first. The legacy nix-env style has no priority on install, so the conflict remains there.
- **`devy start` needs `devy up` first** when the port is applicable but unassigned. Otherwise the service would listen on the default port while `devy up` later exports a random one and leaves the running service alone.

## Risks / Trade-offs

- **Version-specific service flags.** Service CLI flags differ across versions (e.g. mysqld 8.4 removed some options, and the opensearch `-E` syntax). → Use only long-stable flags. Integration-test redis and postgres under nix in CI on Linux and macOS.
- **Slow mongodb installs under nix.** nixpkgs `mongodb` is unfree and source-built, so installs can be very slow. → Use `mongodb-ce` (the binary package) as the nix attr, and document `NIXPKGS_ALLOW_UNFREE` if needed.
- **Unit name collisions.** Unit names are global, so a second project's `devy up` restarts the first project's redis with new args. → This is pre-existing, and documented as a non-goal. The rewrite-on-start behavior makes the collision explicit rather than silent.
- **Behavior change for brew/apt users.** Services on brew/apt that previously had random ports move to their default ports. → Note it in the changelog. Ports in `devy.lock` are ignored for non-applicable services, and `up` rewrites the lock with the default.
- **Versioned attrs can vanish.** Attributes like `nodejs_18` are eventually dropped from nixpkgs. → The allowlist is versioned in code; removed versions fall back to the default attr with a warning.
- **Signature churn.** The signature change to `start_service` touches all four PMs and the test doubles in `package_manager/mod.rs`. → The change is mechanical; the compiler enforces it.

## Migration Plan

- **No user action needed.** Existing nix-run services, which were probably not working, are relaunched with correct args on the next `devy up` once stopped, or on `devy restart`.
- **`devy.lock` stays at v1.** Its shape is unchanged; `assigned_port` simply stops being written for non-applicable services.
- **Rollback:** revert the release. `.devy/data/` is left behind, harmless and gitignored.

## Open Questions

- ~~Whether `mongodb-ce` is present in nixpkgs-unstable.~~ Resolved: `mongodb-ce` 8.2 is present and ships `mongod`, so no fallback is needed. Both `mongodb` and `mongodb-ce` are unfree, and devy's `nix profile install` doesn't pass `--impure` or allow unfree packages, so installing mongodb under nix still fails until that's handled (follow-up).
- Elasticsearch/OpenSearch under nix probably try to write their keystore into `ES_PATH_CONF`/`OPENSEARCH_PATH_CONF`, which default to the read-only store. Copying the config into the data dir is a follow-up.
- The legacy `nix-env` install style still matches on `pname`, so versioned and mapped attributes (`nodejs_22`, `mysql84`, `jdk21`) are reinstalled on every `devy up` there (follow-up).
- Under Nix 2.20+, profile entries carry no version, so `resolved_version` is recorded as `unknown` and lock pinning has no effect under nix. Deriving the version from the store path is a follow-up.
