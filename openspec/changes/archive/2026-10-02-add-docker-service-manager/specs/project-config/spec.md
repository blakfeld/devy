## MODIFIED Requirements

### Requirement: Strict top-level schema
`devy.yml` SHALL accept only the following top-level keys:
- `name`
- `dependencies`
- `environment`
- `commands`
- `hooks`
- `package_manager`
- `service_manager`
- `container_cli`

Any other top-level key SHALL be a parse error. Every key SHALL be optional. `name` SHALL default to `project`, and the collections SHALL default to empty.

#### Scenario: Misspelled top-level key
- **WHEN** `devy.yml` contains `dependecies:`
- **THEN** loading fails with `Failed to parse <path>` and a non-zero exit

#### Scenario: Malformed YAML
- **WHEN** `devy.yml` is not valid YAML
- **THEN** loading fails with `Failed to parse <path>`

#### Scenario: Minimal config
- **WHEN** `devy.yml` contains only `dependencies: []`
- **THEN** the config loads with the project name `project` and no environment, commands or hooks

### Requirement: Dependency entry forms
Each item in `dependencies` SHALL be one of:
- a bare string naming the dependency
- a single-key map from the name to a configuration map
- a single-key map with a null value

A configuration map SHALL accept `version`, `tap`, `after_install`, `shell`, `service_manager` and `image`. It SHALL keep every other key as a module-specific option (for example `port` or `cli_args`). A map item with more than one key SHALL be rejected.

#### Scenario: String form
- **WHEN** a dependency is written as `- redis`
- **THEN** it is a dependency named `redis` with no version and no options

#### Scenario: Map form with options
- **WHEN** a dependency is written as `- mysql: { version: "8.1", port: 3307 }`
- **THEN** it is a dependency named `mysql` with version `8.1` and the module option `port: 3307`

#### Scenario: Null configuration
- **WHEN** a dependency is written as `- mysql:` with no value
- **THEN** it is treated like the string form `- mysql`

#### Scenario: Multiple keys in one item
- **WHEN** a single list item is `{ redis: ~, mysql: ~ }`
- **THEN** devy fails with `dependency entry has multiple keys (redis, mysql); each dependency must be its own list item` (keys listed in the order they appear)

#### Scenario: Service manager and image are not module options
- **WHEN** a dependency is written as `- redis: { service_manager: docker, image: mirror/redis }`
- **THEN** `service_manager` and `image` are read as dependency settings and `devy check` does not report them as unrecognized module keys

## ADDED Requirements

### Requirement: Service manager setting
The top-level `service_manager` SHALL accept exactly `package` or `docker` in lowercase and SHALL default to `package`. The per-dependency `service_manager` SHALL accept the same values. Any other value SHALL be a parse error.

A per-dependency `service_manager` or `image` on a dependency that is not a built-in service MUST be rejected during config validation with `<dep>: service_manager and image apply only to built-in services`.

#### Scenario: Invalid value
- **WHEN** `devy.yml` sets `service_manager: kubernetes`
- **THEN** loading fails with a parse error

#### Scenario: Setting on a non-service
- **WHEN** `devy.yml` declares `- node: { service_manager: docker }`
- **THEN** `devy up` and `devy check` fail with the applies-only-to-built-in-services message

### Requirement: Container CLI setting
The top-level `container_cli` SHALL accept exactly `docker` or `podman` in lowercase and SHALL default to `docker`. It SHALL have no effect when no dependency is docker-managed.

#### Scenario: Unused setting
- **WHEN** `devy.yml` sets `container_cli: podman` and no dependency is docker-managed
- **THEN** devy never invokes `podman`
