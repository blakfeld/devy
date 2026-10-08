## MODIFIED Requirements

### Requirement: Container lifecycle
Starting a docker-managed service SHALL work as follows:
- **No container:** devy creates and starts one with `<cli> run -d`, publishing `127.0.0.1:<resolved port>:<container port>`, plus any secondary port (MinIO console).
- **Existing container, same configuration:** devy SHALL start it with `<cli> start` when it is stopped, and leave it untouched when it is running. The configuration is compared through a `sh.devy.config` label holding a hash of image reference, ports, environment, and arguments.
- **Existing container, different configuration:** devy SHALL remove and recreate it, keeping its volume, whether the container is stopped or running. A container without the `sh.devy.config` label counts as having a different configuration.

`devy up` and `devy start <name>` SHALL NOT treat a running container whose configuration differs as already running: they SHALL print `Recreating <name> container (configuration changed)`, recreate it as above and then run the readiness check. Only a running container with the same configuration SHALL be reported as already running. A container that belongs to another project or machine SHALL be refused exactly as before and never recreated.

A service SHALL be running when `<cli> inspect` reports its container state as running; commands that only report or stop services (`devy services`, `devy stop`, `devy down`) SHALL NOT compare configuration. Stopping SHALL run `<cli> stop` on the container.

#### Scenario: Port change recreates the container
- **WHEN** docker-managed `redis` ran on port 51000 and `devy.yml` now sets `port: 6380`
- **THEN** `devy up` removes the old container, creates a new one publishing `127.0.0.1:6380:6379`, and the volume's data is preserved

#### Scenario: Port change recreates a running container
- **WHEN** docker-managed `redis` is running on port 51000 and `devy.yml` now sets `port: 6380`
- **THEN** `devy up` prints `Recreating redis container (configuration changed)`, removes the running container, creates a new one publishing `127.0.0.1:6380:6379`, and does not print `○ redis service already running`

#### Scenario: Image update recreates a running container
- **WHEN** docker-managed `postgresql` is running and `devy up --update` resolves a new digest for its tag
- **THEN** devy recreates the container from the new digest, keeping its volume, and `devy.lock` records that digest

#### Scenario: Start recreates a running container with changed configuration
- **WHEN** docker-managed `redis` is running and its `version` in `devy.yml` changed since the container was created
- **THEN** `devy start redis` recreates the container with the new image instead of printing `○ redis is already running`

#### Scenario: Unchanged running container is left alone
- **WHEN** docker-managed `redis` is running and nothing in its configuration changed
- **THEN** `devy up` prints `○ redis service already running` and runs no `<cli> rm`, `<cli> run` or `<cli> start`

#### Scenario: Restart reuses the container
- **WHEN** docker-managed `redis` was stopped and nothing in its configuration changed
- **THEN** `devy start redis` runs `docker start` on the existing container

#### Scenario: Listing does not compare configuration
- **WHEN** docker-managed `redis` is running with a configuration that differs from `devy.yml`
- **THEN** `devy services` still shows it as running and does not recreate it
