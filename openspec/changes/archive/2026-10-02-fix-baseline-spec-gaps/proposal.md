# Proposal

## Why

Writing the baseline specs turned up eight places where devy's behavior contradicts what users are told or what devy itself records. The worst is that `devy up` exports `REDIS_URL`/`*_PORT` pointing at a randomly assigned port that, with the default nix backend, the service never listens on, so a fresh project gets connection strings that don't work. The other seven are smaller (nix ignores `version:`, `check` disagrees with `up` about ports, docs and completions are out of sync, a malformed `shell.nix`). They're fixed together because they share the port and nix-service plumbing, and each one makes devy's output untrustworthy.

## What Changes

- **Service ports reach the running process.** Under nix, launchd/systemd units now launch each service with module-provided arguments and environment: the resolved port, a 127.0.0.1 bind, and a project-local data dir under `.devy/data/<service>`. One-time initialization runs where needed (e.g. `initdb`, `mysqld --initialize-insecure`). On brew/apt, the existing conf.d support for postgresql/mysql/mariadb is unchanged. Services whose backend can't apply a port use their **default** port instead of a random one, so env vars stay truthful, and an explicit non-default port on such a backend produces a clear warning. **BREAKING (behavioral):** on brew/apt, services other than postgresql/mysql/mariadb no longer get random ports.
- **One port resolver for every command.** `start`, `restart`, `check` and `status` resolve ports the same way `up` does: explicit, then the `devy.lock` `assigned_port`, then (where ports can be applied) "to be assigned". `check` no longer reports a conflict for services that `up` would give distinct ports (e.g. mysql + mariadb under nix).
- **Nix honors `version:`.** Modules map a version to a versioned nixpkgs attribute (e.g. `nodejs_22`, `python312`, `postgresql_16`, `jdk21`, `go_1_26`), from an allowlist of versions current nixpkgs carries. When there's no mapping and the version came from `devy.yml`, `up` and `check` warn that it's ignored; versions applied from the lock map silently. `devy export` uses the same versioned attribute. The nix installed check matches versioned attribute names.
- **mongodb and kafka run under nix.** mongodb runs as `mongod` from `mongodb-ce`. Kafka runs from the `apacheKafka` scripts in KRaft mode with a one-time storage format. nixpkgs ships Kafka 4, which has no ZooKeeper, so under nix Kafka is always KRaft, even without `kraft: true`.
- **Broken nix attributes fixed.** `mysql` installs `mysql84` (nixpkgs dropped `mysql80`), and `rabbitmq` installs `rabbitmq-server` (there is no `rabbitmq` attribute).
- **typescript accepts `global_packages`** in its allowlist, so `devy check` stops flagging a key the module actually reads.
- **Completions cover the full CLI:** `pr`, `export` (with `--format shell|flake`) and `up --bootstrap`.
- **`devy export --format shell` writes a well-formed `shell.nix`** (the stray trailing `}}` is gone).
- **README:** remove the nonexistent ruby `gems:` option (Bundler via `Gemfile` covers it), and document the port behavior above.

## Capabilities

### New Capabilities
- None.

### Modified Capabilities
- `service-ports`: port resolution is shared by all commands and is backend-aware (default port where a port can't be applied); nix applies ports through service arguments.
- `service-management`: nix units carry per-service arguments, environment and data dirs with one-time init; `start`/`restart` use resolved ports; mongodb/kafka are supported under nix, and the unsupported-service error applies only to the generic module.
- `service-modules`: each service declares how it's launched under nix; MongoDB and Kafka gain nix launch details.
- `environment-check`: port conflict checking uses the shared resolver; unapplied explicit ports and ignored nix versions are reported as warnings.
- `package-managers`: nix installs versioned attributes and the installed check matches them.
- `dependency-modules`: modules map versions to nix attributes; typescript allows `global_packages`.
- `shell-integration`: completion covers `pr`, `export --format` and `up --bootstrap`.
- `nix-export`: the export uses versioned attributes and `shell.nix` is syntactically valid.

## Impact

- **Code:**
  - `src/commands/up.rs` (port resolution extracted)
  - `check.rs`, `status.rs` and `service.rs` (use the shared resolver)
  - `src/modules/mod.rs`: new trait hooks for nix launch spec, versioned nix attr and port applicability
  - every service module file
  - `node.rs`, `python.rs`, `java.rs`, `dotnet.rs`, `typescript.rs` and other versioned modules
  - `src/package_manager/nix.rs`: unit/plist generation with args and env, versioned install, installed-check matching
  - `src/commands/hook.rs`
  - `src/commands/export.rs`
  - `README.md`
- **Files on disk:** a new `.devy/data/<service>/` directory per project for nix-run services (already under `.devy/`, which projects typically ignore).
- **Compatibility:**
  - Existing nix-run services restart with new arguments on the next `devy up` after they're stopped.
  - brew/apt users with random ports in `devy.lock` for non-database services move back to default ports.
  - The `tests/cli.rs` mysql+mariadb conflict test changes to a case that still conflicts.
- **Dependencies:** no new crates.
