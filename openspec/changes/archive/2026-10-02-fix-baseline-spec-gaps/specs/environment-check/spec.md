## MODIFIED Requirements

### Requirement: Hard configuration errors
`devy check` SHALL fail immediately with an `error:` message and exit 1 in these cases:
- The configuration cannot be loaded.
- The package manager setting is invalid for the platform.
- A dependency fails package-manager config validation.
- Service ports, resolved as `devy up` would resolve them from `devy.yml` and `devy.lock`, are out of range or conflict. Ports not yet assigned are excluded.

#### Scenario: No config
- **WHEN** `devy check` runs outside any devy project
- **THEN** it exits non-zero and stderr mentions `devy.yml`

#### Scenario: Port conflict on default ports
- **WHEN** the backend is brew and `devy.yml` lists `elasticsearch` and `opensearch` with no explicit ports (both default to 9200)
- **THEN** `devy check` fails with `port conflict: 'elasticsearch' and 'opensearch' both use port 9200`

#### Scenario: No conflict for ports up would assign
- **WHEN** the backend is nix, `devy.yml` lists `mysql` and `mariadb` with no explicit ports, and `devy.lock` has no entries for them
- **THEN** `devy check` reports no port conflict

#### Scenario: Locked ports conflict
- **WHEN** `devy.lock` records the same `assigned_port` for `redis` and `memcached` under nix
- **THEN** `devy check` fails with a port conflict naming both

### Requirement: Issue detection
`devy check` SHALL count one issue for each of the following:
- An unrecognized option key on a module that declares its allowed keys. A warning SHALL list the known keys, or say the module accepts no extra keys.
- An invalid dependency `shell`.
- A dependency that is not installed.
- An installed service that is stopped.
- A configured environment variable or PATH entry that is missing from the written environment file.

A service that is not installed SHALL count as one issue, not two. Module configuration warnings SHALL be printed but SHALL NOT count as issues. These also include:
- an explicit port that the backend cannot apply
- a `devy.yml` `version` that the nix backend cannot honor

#### Scenario: Unrecognized key
- **WHEN** `redis` is configured with `prot: 6380`
- **THEN** `devy check` warns `redis: unrecognized config key `prot` — known keys: port` and counts one issue

#### Scenario: Environment not yet written
- **WHEN** `environment` declares two variables and `devy up` has never run
- **THEN** each variable is reported as `missing` and counted as an issue

#### Scenario: Uninstalled service
- **WHEN** a service dependency is not installed
- **THEN** exactly one issue is counted for it

#### Scenario: TypeScript global packages accepted
- **WHEN** `typescript` is configured with `global_packages: [eslint]`
- **THEN** `devy check` reports no unrecognized key for it

#### Scenario: Unhonored nix version is a warning
- **WHEN** the backend is nix and `jq` is declared with `version: "1.6"` and no versioned nix attribute exists for it
- **THEN** `devy check` warns that the version is ignored by nix and does not count an issue for it

#### Scenario: Changed environment value not detected
- **WHEN** `devy up` wrote `LOG_LEVEL=debug` and `devy.yml` now sets `LOG_LEVEL: info`
- **THEN** `devy check` reports `LOG_LEVEL` as `configured` and counts no issue for it
