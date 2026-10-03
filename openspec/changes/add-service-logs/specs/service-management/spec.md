## MODIFIED Requirements

### Requirement: Starting a single service
`devy start <name>` SHALL resolve the service's port as `devy up` does (using `devy.lock`), start the service when it is not running, and then wait for its health check on that port; a health-check timeout MUST be reported as a warning rather than a failure, and that warning MUST tell the user to run `devy logs <name>`.

#### Scenario: Already running
- **WHEN** the named service is already running
- **THEN** devy prints `○ <name> is already running` and exits 0 without starting it again

#### Scenario: Health check times out
- **WHEN** the service starts but never passes its health check within the configured attempts
- **THEN** devy warns that the service started but the health check timed out, asks the user to verify manually, and suggests `devy logs <name>`
- **AND** devy prints `✓ <name> started` and exits 0

#### Scenario: Locked port honored
- **WHEN** `devy.lock` records `assigned_port: 51000` for `redis` under the nix backend and the user runs `devy start redis`
- **THEN** redis listens on 51000 and the health check probes 51000

### Requirement: Readiness and shutdown polling
Waiting for a service SHALL poll its health check (or running state when stopping) up to a per-module attempt count with a fixed interval (default 10 attempts at 500 ms), failing with the attempt count when exhausted. Health waits SHALL print `Still waiting for <name> (<n>/<max>)` on every 10th attempt before the last, so a wait with the default 10 attempts prints no progress. Shutdown waits SHALL report no progress and SHALL fail with `<name> did not stop after <N> attempts — try stopping it manually or run devy logs <name>`.

#### Scenario: Service becomes healthy
- **WHEN** the health check succeeds on the third attempt
- **THEN** waiting returns successfully without further attempts

#### Scenario: Never healthy
- **WHEN** the health check fails on every attempt
- **THEN** waiting fails with `<name> did not become healthy after <N> attempts`

#### Scenario: Never stops
- **WHEN** a service is still running after every shutdown attempt
- **THEN** waiting fails with `<name> did not stop after <N> attempts — try stopping it manually or run devy logs <name>`
