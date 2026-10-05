# Spec Delta

## MODIFIED Requirements

### Requirement: Log sources per backend
devy SHALL read logs from the place where the service's backend sends them:
- **nix on macOS:** the launchd agent's log file, `devy-<project>-<name>.log` in devy's private per-user directory under `$TMPDIR` (or `$XDG_RUNTIME_DIR`), as described in service-management.
- **nix on Linux:** the user journal for unit `devy-<project>-<name>.service`, via `journalctl --user`.
- **brew:** the log file paths that `brew services info --json <name>` reports. When it reports both standard output and standard error files, devy SHALL show both.
- **apt:** the system journal for the service's unit, via `journalctl -u <name>`. devy MUST NOT run `sudo` to read logs.
- **docker-managed services:** `<container_cli> logs` for the container `devy-<project>-<service>`. When that container belongs to another project or another machine (see docker-services), devy SHALL refuse with the same error `devy start` gives instead of showing its logs, and SHALL NOT send them with `--explain`.

`<name>` is the backend service name and `<project>` is the project slug, both defined in service-management. Under nix, devy SHALL read the legacy source (`<name>.log` in that same directory, or unit `devy-<name>.service`) instead only when this project owns a legacy-named unit for the service: the per-project source does not exist, the legacy source exists, and the legacy unit's data or working directory is inside this project's root. Otherwise devy MUST NOT show the legacy source.

#### Scenario: Nix on macOS
- **WHEN** the package manager is nix on macOS and the user runs `devy logs redis` in project `app`
- **THEN** devy shows the tail of `devy-app-<hash>-redis.log` in devy's per-user directory

#### Scenario: Nix on Linux
- **WHEN** the package manager is nix on Linux and the user runs `devy logs redis` in project `app`
- **THEN** devy shows journal entries for the user unit `devy-app-<hash>-redis.service`

#### Scenario: Logs from a not-yet-migrated service
- **WHEN** the package manager is nix on macOS, a legacy `sh.devy.redis` unit's data directory is under this project's root, `redis.log` exists in devy's per-user directory, and `devy-<project>-redis.log` does not
- **THEN** `devy logs redis` shows the tail of `redis.log`

#### Scenario: Another project's legacy log is not shown
- **WHEN** the package manager is nix on macOS, the legacy `sh.devy.redis` unit's data directory is under `/src/other`, `redis.log` exists in devy's per-user directory, `devy-<project>-redis.log` does not, and the user runs `devy logs redis` in `/src/app`
- **THEN** devy does not show `redis.log`

#### Scenario: Apt journal not readable
- **WHEN** the package manager is apt and the user is not allowed to read the system journal for the unit
- **THEN** devy reports that the journal could not be read and suggests running `sudo journalctl -u <name>` or adding the user to the `systemd-journal` group, without running sudo itself

#### Scenario: Docker-managed service
- **WHEN** `redis` is docker-managed in project `myapp` with `container_cli: podman`, and the user runs `devy logs redis -n 50`
- **THEN** devy shows the last 50 lines of `podman logs` for container `devy-myapp-redis`

## ADDED Requirements

### Requirement: Log files are read safely
For nix on macOS, devy SHALL read log files only from its private per-user directory, and only when that directory is a real directory owned by the current user with mode 0700. Otherwise devy SHALL fail, reporting `refusing to read service logs: <dir> is not owned by the current user — remove it so devy can create its own`, `refusing to read service logs: <dir> has mode <mode>, expected 700 — remove it so devy can create its own`, or `refusing to read service logs: <dir> is not a directory owned by the current user — remove it so devy can create its own`. `devy logs` MUST NOT create that directory. While it is missing, there are no logs.

On Unix, every log file devy opens, including brew's log files and each reopen while following, SHALL be a regular file that is not a symlink, owned by the current user or root, in a directory owned by the current user or root that is not world-writable unless it has the sticky bit. Otherwise devy SHALL refuse it with one of:
- `<path> is a symlink; devy does not follow it`
- `<path> is not a regular file`
- `<path> is owned by another user; devy will not read it`
- `<dir> is not a directory only you or root can write to; devy will not read logs from it`

For a single service the refusal SHALL fail the command, with the message included in the error. When showing all services, devy SHALL print the refusal for that service, show the others, and then fail with `Could not read the logs of <names>`. While following, devy SHALL print `not following: <reason>` (with the service prefix) and stop following that file. When following ends after such a refusal, devy SHALL fail with `Stopped following a log file devy refused to read`.

#### Scenario: Log directory is not private
- **WHEN** the package manager is nix on macOS, devy's per-user log directory has mode 777, and the user runs `devy logs redis`
- **THEN** devy fails, reporting `refusing to read service logs: <dir> has mode 777, expected 700 — remove it so devy can create its own` and prints no log lines

#### Scenario: Symlinked log file
- **WHEN** the service's log file is a symlink to another file, and the user runs `devy logs redis`
- **THEN** devy fails, reporting `<path> is a symlink; devy does not follow it`, and does not print the target's contents

#### Scenario: One refused log among several services
- **WHEN** brew is the package manager, `redis` and `nginx` are declared, redis's log file is a symlink, and the user runs `devy logs`
- **THEN** devy shows nginx's lines, reports that it refused redis's log, and fails with `Could not read the logs of redis`

#### Scenario: Docker service that cannot be checked among several services
- **WHEN** `redis` is docker-managed and `nginx` is not, the container runtime is unreachable (or redis's container belongs to another project or machine), and the user runs `devy logs`
- **THEN** devy shows nginx's lines, reports why redis's logs can't be read, and fails with `Could not read the logs of redis`
- **AND** with `--follow`, devy prints `redis | not following: <reason>`, keeps following nginx, and when following ends by itself it fails with an error naming redis; stopping with Ctrl-C exits 0

#### Scenario: Log directory missing
- **WHEN** the package manager is nix on macOS, devy's per-user log directory does not exist, and the user runs `devy logs redis`
- **THEN** devy prints `· No logs yet for redis (expected at <path>)`, exits 0, and does not create the directory
