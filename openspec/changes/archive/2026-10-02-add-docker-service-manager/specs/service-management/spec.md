## MODIFIED Requirements

### Requirement: Listing services
`devy services` SHALL print each declared service with a running or stopped indicator, suffixing docker-managed services with `(docker)`, and SHALL print `No services defined.` and exit 0 when no service dependencies exist.

#### Scenario: Mixed running state
- **WHEN** `devy.yml` declares `redis` (running) and `postgresql` (stopped)
- **THEN** devy prints a "Services" header, a filled `●` next to `redis`, and a dimmed `○` next to `postgresql`

#### Scenario: No services
- **WHEN** `devy.yml` declares only non-service dependencies
- **THEN** devy prints `No services defined.` and exits 0

#### Scenario: Docker-managed service labeled
- **WHEN** `redis` is docker-managed and running
- **THEN** devy prints `● redis (docker)`

### Requirement: Bringing the environment down
`devy down` SHALL run the `before_down` hook, stop every running service in declaration order waiting for each to stop, and then run the `after_down` hook; it MUST NOT modify `devy.lock` or the shadowenv file. Docker-managed services SHALL be stopped with the container CLI and their containers and volumes kept, unless `devy down --volumes` is given, in which case devy SHALL also remove each docker-managed service's container and named volume and print `✓ <dep> container and volume removed`. `--volumes` SHALL have no effect on package-managed services.

#### Scenario: Mixed services
- **WHEN** `devy.yml` declares `redis` (running) and `postgresql` (stopped)
- **THEN** devy prints `○ postgresql already stopped`, stops `redis`, prints `✓ redis stopped`, and prints `✓ all services stopped`

#### Scenario: Nothing running
- **WHEN** all declared services are already stopped
- **THEN** devy prints `○ nothing to stop`

#### Scenario: No services declared
- **WHEN** `devy.yml` declares no service dependencies
- **THEN** devy prints `○ no services defined` and exits 0

#### Scenario: Stop failure skips after_down
- **WHEN** stopping a service fails
- **THEN** devy exits with an error and does not run the `after_down` hook

#### Scenario: Plain down keeps data
- **WHEN** docker-managed `postgresql` is running and the user runs `devy down`
- **THEN** devy stops the container and its volume `devy-<project>-postgresql` still exists

#### Scenario: Down with volumes
- **WHEN** docker-managed `postgresql` exists and the user runs `devy down --volumes`
- **THEN** devy stops and removes the container and removes the volume `devy-<project>-postgresql`
