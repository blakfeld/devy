## ADDED Requirements

### Requirement: Ports for docker-managed services
A docker-managed service's port SHALL always be applicable, so it follows the full port precedence: explicit, locked, then newly assigned. The resolved port SHALL be published on the host as `127.0.0.1:<resolved port>`, mapped to the module's container port. The container port SHALL be fixed per module and MUST NOT change with the resolved port. Exported `<NAME>_PORT` and module URLs SHALL use the host port.

#### Scenario: Random port under docker with brew packages
- **WHEN** `package_manager: brew`, `service_manager: docker`, and `redis` has no port and no lock entry
- **THEN** `devy up` assigns a free port such as 51000, publishes `127.0.0.1:51000:6379`, and exports `REDIS_URL=redis://127.0.0.1:51000`

#### Scenario: Two databases on default ports
- **WHEN** `mysql` and `mariadb` are both docker-managed with no explicit ports
- **THEN** `devy check` reports no port conflict and `devy up` gives them distinct host ports
