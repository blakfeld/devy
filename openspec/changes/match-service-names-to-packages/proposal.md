# Proposal

## Why

devy installs a service under one package name but starts, stops and checks it under another. Under brew and apt, the name passed to `brew services` or `systemctl` is often the dependency name as written in `devy.yml`, not the package `install` used. So `devy up` can install a service and then fail to start it, start a different (uninstalled) version, or report it stopped forever. The current spec even records the wrong behaviour (`brew services start elastic` for an `elastic` dependency).

## What Changes

- Under brew, the service name becomes the Homebrew formula `install` uses: the module's brew package name, plus the `@<pin>` suffix when `version` is a user pin (not a version from `devy.lock`). For example, `postgresql` with `version: "16"` gets `brew services start postgresql@16`, and `elasticsearch` gets `elasticsearch-full`.
- Under brew, apt and winget, aliases are resolved first, so `elastic`, `meili` and `hashicorp-vault` use their canonical module's name instead of the alias.
- Under apt, a module names its systemd unit when that differs from the canonical name: `rabbitmq-server` for RabbitMQ (today devy uses `rabbitmq`, which doesn't exist).
- The nix backend's service names, and its legacy-unit migration, are unchanged.
- `devy logs`, `devy check`'s legacy-name note, and every module's status, start and stop all use the same derived name.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `service-management`: the "Backend service name" requirement changes. Under brew, apt and winget, the name now comes from the installed package rather than from the raw dependency name.

## Impact

- `src/modules/mod.rs`: `Module::service_name` and its default gain the package manager, so the name can depend on the backend. Callers: `start_via_pm`, `Module::log_source`, and `legacy_service_note` in `src/commands/check.rs`.
- Service modules: postgres, mysql, mariadb, mongodb, elasticsearch and rabbitmq (their overrides or package names), plus the `is_running`/`stop` call sites in every service module (memcached, mongodb, elasticsearch, opensearch, mysql, meilisearch, kafka, mailhog, minio, vault, postgres, rabbitmq, mariadb, redis, nginx).
- `src/package_manager/`: brew's versioned-formula rule (`brew_formula_name`) is shared with service naming, and not duplicated.
- Users who already have a stray unversioned service running (for example `postgresql` started by an older devy) will see devy control the pinned formula instead. The old service isn't stopped automatically (see design).
