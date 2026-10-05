# Spec Delta

## MODIFIED Requirements

### Requirement: Per-project container and volume naming
Each docker-managed service SHALL run in a container named `devy-<project>-<canonical-name>`. `<project>` is the project `name` lower-cased with characters outside `[a-z0-9-]` replaced by `-`, followed by `-` and the first 8 hex characters of a hash of the project root path. The container SHALL carry labels `sh.devy.project=<project root>` and `sh.devy.service=<canonical-name>`. When devy can identify this machine, every container it creates SHALL also carry `sh.devy.host=<host id>`: 16 hex characters of a SHA-256 hash, with a fixed devy-specific prefix, of a machine identifier and the current user. The machine identifier is `/etc/machine-id` on Linux, the hardware UUID on macOS, or `MachineGuid` on Windows, falling back to the host name. Inside a docker or podman container, the host name is always included. Under WSL, outside a container, the distribution name (`WSL_DISTRO_NAME`) is always included, and without it there is no id. The user is the uid, or on Windows the user name. When devy can't identify this machine, it SHALL create containers without the `sh.devy.host` label. Persistent data SHALL be stored in a named volume with the same name as the container, mounted at the module's data path.

#### Scenario: Two projects run redis
- **WHEN** projects at `/src/a` and `/src/b`, both named `app`, each run docker-managed `redis`
- **THEN** two distinct containers `devy-app-<hashA>-redis` and `devy-app-<hashB>-redis` run concurrently with separate volumes

#### Scenario: Host label on a new container
- **WHEN** devy creates the container for docker-managed `redis` on a machine it can identify
- **THEN** the container carries `sh.devy.host` with this machine and user's id, and the label value contains no raw machine identifier

#### Scenario: Two WSL distributions share a daemon
- **WHEN** two WSL distributions with the same machine id and uid use one Docker Desktop daemon
- **THEN** containers devy creates from each carry different `sh.devy.host` values

### Requirement: Container credentials and ownership
devy SHALL pass credentials to containers (`MINIO_ROOT_USER`, `MINIO_ROOT_PASSWORD`, `MEILI_MASTER_KEY`) through a mode-0600 env file passed with `--env-file`, not as `-e` arguments, and SHALL delete the file after the container is created. Before reusing an existing container with the expected name, devy SHALL check that its `sh.devy.project` label equals the current project root, and otherwise fail with `container <name> belongs to another project`.

A container with the expected name SHALL also count as another owner's when its `sh.devy.host` label differs from this machine's host id, or when it has a `sh.devy.host` label and devy can't identify this machine. A container without a `sh.devy.host` label SHALL be treated as the project's, as before. For a container another owner holds:
- `devy up`, `start` and `restart` SHALL fail without starting, reusing or replacing it. When the label is another machine's, the error SHALL start with `container <name> belongs to another machine or user sharing this container daemon (its sh.devy.host label isn't this machine's)`. When this machine has no id, it SHALL start with `container <name> has a sh.devy.host label, but devy can't identify this machine (under WSL, run devy from a wsl.exe session)`. Both SHALL add a hint to check the container with `<cli> inspect <name>` and, only if its data is the user's, remove it with `<cli> rm -f <name>` and run devy again.
- `devy stop` and `devy down` SHALL treat the service as not running and leave the container alone.
- `devy down --volumes` SHALL warn `<service>: container <name> belongs to <owner> — not removing it` and keep the container and its volume.

devy SHALL NOT add the `sh.devy.host` label to an existing container it reuses. The label prevents accidental collisions between machines that share a daemon; it is not a security boundary.

#### Scenario: Credentials not in argv
- **WHEN** a docker-managed `minio` with `secret_key` is started
- **THEN** the `docker run` arguments do not contain the secret

#### Scenario: Start refuses another machine's container
- **WHEN** container `devy-app-<hash>-redis` records this project's root but its `sh.devy.host` label is another machine's id, and the user runs `devy start redis`
- **THEN** devy fails with `container devy-app-<hash>-redis belongs to another machine or user sharing this container daemon` and the inspect-and-remove hint, and does not start, remove or recreate the container

#### Scenario: Labelled container on an unidentified machine
- **WHEN** devy can't identify this machine, the project's container carries any `sh.devy.host` label, and the user runs `devy up`
- **THEN** devy fails with `container <name> has a sh.devy.host label, but devy can't identify this machine` and leaves the container alone

#### Scenario: Down skips another machine's container
- **WHEN** the project's redis container is labelled with another machine's id and the user runs `devy down --volumes`
- **THEN** devy reports redis as already stopped, warns that the container belongs to another machine or user and is not removed, and keeps the container and its volume

#### Scenario: Unlabelled container is the project's
- **WHEN** the project's redis container was created by an older devy and has no `sh.devy.host` label, and the user runs `devy stop redis`
- **THEN** devy stops it as before
