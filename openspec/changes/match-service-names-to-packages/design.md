# Design

## Context

`Module::service_name(&self, dep)` (src/modules/mod.rs) returns `dep.name` by default. Only three modules override it: postgres (`postgresql`), mariadb (`mariadb`) and mongodb (`mongodb-community` on macOS, `mongod` on Linux). It doesn't take the package manager, so it can't follow what `install` does. `install` goes through each module's `package_name(pm)`, and under brew through `brew_formula_name` (src/package_manager/brew.rs). `brew_formula_name` appends `@<version>` for a user pin (`<major>` or `<major>.<minor>`, not `version_from_lock`).

What was verified:

- **Version pins (brew)**: `brew info` confirms `postgresql@16`, `mysql@8.4` and `mariadb@11.4` are separate formulae, each with its own `service`. devy installs `postgresql@16` and then runs `brew services start postgresql`. mongodb pins (`mongodb-community@7.0`) have the same problem.
- **Aliases**: `dep.name` is never canonicalized before `service_name`, so `elastic`, `meili` and `hashicorp-vault` reach `brew services` / `systemctl` / `net start` as written. The current main spec even records `brew services start elastic` as intended.
- **Elasticsearch (brew)**: `package_name` gives `elasticsearch-full` (elastic/tap), but the service name is `elasticsearch`.
- **RabbitMQ (apt)**: the package and unit are `rabbitmq-server`, but devy uses `rabbitmq`.
- **mysql (apt)**: the package is `mysql-server` but its unit is `mysql`. Today's `mysql` is right by accident. A naive "service name = package name" fix would break it. So apt needs unit names, not package names.

Consumers of the name: `start_via_pm`, every module's `is_running`/`stop`, `Module::log_source` (`devy logs`) and `legacy_service_note` (`devy check`). Config-dir lookups (`mysql_family_post_setup`, `loopback::etc_dir`) pass literal names (`"mysql"`, `"rabbitmq"`, ...) and don't use `service_name`. `config_prefix_args` maps those literals to the global brew prefix. So they're unaffected, and must stay that way.

## Goals / Non-Goals

**Goals:**
- Under brew, apt and winget, start, stop, status and logs act on the service of the package that `install` put on the machine.
- One source of truth for the brew versioned-formula rule, shared by install and service naming.
- Tests that fail today for each confirmed sub-case.

**Non-Goals:**
- Changing nix service or unit names. Nix units are named per project and have a legacy-name migration, so renaming them (for example canonicalizing `meili`) would orphan running units. That's a separate change if wanted.
- Stopping or cleaning up a service that an older devy started under the wrong name.
- Docker-managed services, which don't use `service_name`.

## Decisions

**D1. `service_name` takes the package manager.** The signature becomes `service_name(&self, pm: &dyn PackageManager, dep: &Dependency) -> Cow<'_, str>`, and every caller passes the `pm` it already has. This is the smallest change that lets the name depend on the backend. Alternative: let the brew and apt backends rewrite names inside `start_service`. Rejected: backends only receive a `&str` and can't see aliases, the module's package name, or `version_from_lock`.

**D2. Default implementation, by backend.**
- `nix`: the current behaviour, exactly (`dep.name`, or the module's existing fixed name). This keeps nix unit names and the legacy migration (`uses_legacy_service_name`) untouched.
- `brew`: the versioned formula of the module's brew package name. A new trait hook, `Module::service_package(&self, pm, dep) -> Cow<str>`, returns the base name. Its default is `canonical_name(&dep.name)`, and modules with a `package_name` return it, so elasticsearch gives `elasticsearch-full` and mongodb gives `mongodb-community`. The versioned suffix comes from a new `PackageManager::service_name_for(&self, dep: &Dependency) -> String`, which defaults to `dep.name.clone()`. Homebrew implements it with `brew_formula_name`, so install and service naming can't drift. The module calls `pm.service_name_for(&pm_dep(dep, base))`.
- `apt`: `Module::apt_unit` (default: the canonical name), overridden by rabbitmq (`rabbitmq-server`) and mongodb (`mongod`). Apt version pins (`name=version`) never change the unit name.
- `winget`: the canonical name.

The postgres, mariadb and mongodb overrides of `service_name` are removed and expressed through these hooks, so there's one code path.

**D3. A version from the lock doesn't version the service name.** This matches `brew_formula_name`: a version injected from `devy.lock` never selects a formula, so the service stays unversioned. Sharing the function (D2) enforces this.

**D4. Don't migrate stray services.** If an older devy started unversioned `postgresql` while `postgresql@16` was installed, that start failed or ran an unrelated keg. devy won't stop it. We record this in the README's brew notes rather than add cleanup logic that could stop a user's own service.

## Risks / Trade-offs

- [The signature change touches every service module] → It's mechanical. The compiler finds every call site, and existing per-module tests cover `is_running`/`stop`.
- [A user who worked around the bug, for example by running `brew services start postgresql` by hand, now sees `devy status` report the pinned service as stopped] → That's the correct report, and the README note (D4) explains it.
- [`config_prefix_args` matches unversioned literals (`"mysql"`)] → Config lookups keep passing literals. A test asserts that mysql `post_setup` under brew with a pin still resolves the global prefix.
- [Under brew, elasticsearch-full is only in elastic/tap, so the formula couldn't be checked locally] → The name comes from the existing `package_name`, which `install` already uses. The service is whatever that formula defines.

## Open Questions

- Should nix also canonicalize aliases (so `meili` becomes `.meilisearch`), using the existing legacy-unit migration? It's deferred, and the current behaviour stays in the spec.
- Redis under apt runs as `redis-server.service`, with `redis.service` as an alias. `systemctl` resolves the alias, so we keep `redis`. Switch to `redis-server` only if an alias problem shows up.
