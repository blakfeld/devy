# Spec Delta

## MODIFIED Requirements

### Requirement: Log sources per backend
devy SHALL read logs from the place where the service's backend sends them:
- **nix on macOS:** the launchd agent's log file, `$TMPDIR/devy-<project>-<name>.log`.
- **nix on Linux:** the user journal for unit `devy-<project>-<name>.service`, via `journalctl --user`.
- **brew:** the log file paths that `brew services info --json <name>` reports. When it reports both standard output and standard error files, devy SHALL show both.
- **apt:** the system journal for the service's unit, via `journalctl -u <name>`. devy MUST NOT run `sudo` to read logs.
- **docker-managed services:** `<container_cli> logs` for the container `devy-<project>-<service>`.

`<name>` is the backend service name and `<project>` is the project slug, both defined in service-management. Under nix, when the per-project source does not exist but the legacy source (`$TMPDIR/devy-<name>.log`, or unit `devy-<name>.service`) does, devy SHALL read the legacy source.

#### Scenario: Nix on macOS
- **WHEN** the package manager is nix on macOS and the user runs `devy logs redis` in project `app`
- **THEN** devy shows the tail of `$TMPDIR/devy-app-<hash>-redis.log`

#### Scenario: Nix on Linux
- **WHEN** the package manager is nix on Linux and the user runs `devy logs redis` in project `app`
- **THEN** devy shows journal entries for the user unit `devy-app-<hash>-redis.service`

#### Scenario: Logs from a not-yet-migrated service
- **WHEN** the package manager is nix on macOS, `$TMPDIR/devy-redis.log` exists, and `$TMPDIR/devy-<project>-redis.log` does not
- **THEN** `devy logs redis` shows the tail of `$TMPDIR/devy-redis.log`

#### Scenario: Apt journal not readable
- **WHEN** the package manager is apt and the user is not allowed to read the system journal for the unit
- **THEN** devy reports that the journal could not be read and suggests running `sudo journalctl -u <name>` or adding the user to the `systemd-journal` group, without running sudo itself

#### Scenario: Docker-managed service
- **WHEN** `redis` is docker-managed in project `myapp` with `container_cli: podman`, and the user runs `devy logs redis -n 50`
- **THEN** devy shows the last 50 lines of `podman logs` for container `devy-myapp-redis`
