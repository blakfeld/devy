## MODIFIED Requirements

### Requirement: Hard configuration errors
`devy check` SHALL fail immediately with an `error:` message and exit 1 in these cases:
- The configuration cannot be loaded. This includes a malformed `${` reference in an `environment` value.
- The package manager setting is invalid for the platform.
- A dependency fails package-manager config validation.
- Service ports, resolved as `devy up` would resolve them from `devy.yml` and `devy.lock`, are out of range or conflict. Ports not yet assigned are excluded.
- An `environment` value references a name the project environment does not define, or references form a cycle (project-config, Environment references). A reference to a service port that is not assigned yet SHALL NOT be an error, because `devy up` assigns it.

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

#### Scenario: Undefined environment reference
- **WHEN** `environment` sets `URL: "${NOPE}"` and nothing defines `NOPE`
- **THEN** `devy check` fails with `environment.URL: ${NOPE} is not defined` and exits 1

#### Scenario: Reference to a port up will assign
- **WHEN** the backend is nix, `devy up` has never run, and `environment` references `${REDIS_PORT}` for a declared `redis`
- **THEN** `devy check` reports no error for the reference
