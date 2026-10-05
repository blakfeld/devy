# docker-services Specification

## Purpose
Defines how devy runs service dependencies as per-project containers through a Docker-compatible CLI (Docker or Podman), as an alternative to package-manager-managed services.

## Requirements

### Requirement: Selecting docker-managed services
A service dependency SHALL be docker-managed when its own `service_manager` is `docker`, or when it has none and the top-level `service_manager` is `docker`. A per-dependency `service_manager: package` SHALL keep that service on the package manager even when the top-level setting is `docker`. Non-service dependencies MUST always use the package manager.

#### Scenario: Top-level docker
- **WHEN** `devy.yml` sets `service_manager: docker` and declares `redis` and `jq`
- **THEN** `redis` runs as a container and `jq` is installed by the package manager

#### Scenario: Per-dependency opt-in
- **WHEN** `devy.yml` leaves `service_manager` unset and declares `- postgres: { service_manager: docker }` and `redis`
- **THEN** postgres runs as a container and redis uses the package manager's service backend

#### Scenario: Per-dependency opt-out
- **WHEN** `devy.yml` sets `service_manager: docker` and declares `- redis: { service_manager: package }`
- **THEN** redis uses the package manager's service backend

### Requirement: Container runtime availability
When any dependency is docker-managed, `devy up`, `start`, `restart`, `stop`, and `down` SHALL use the CLI named by `container_cli` (`docker` by default, or `podman`). They SHALL check that the CLI is on PATH and that `<cli> info` succeeds before acting on containers. If the check fails, devy MUST fail with `<cli> is not available — install it or start its daemon, or set service_manager: package`. `--bootstrap` MUST NOT attempt to install a container runtime.

#### Scenario: Daemon not running
- **WHEN** `redis` is docker-managed and `docker info` fails
- **THEN** `devy up` fails with the not-available message before pulling or starting anything

#### Scenario: Podman selected
- **WHEN** `devy.yml` sets `container_cli: podman` and `redis` is docker-managed
- **THEN** devy runs `podman` for every container operation

### Requirement: Built-in service images
Every built-in service module SHALL define a container image, a default tag, the container port, a data path for its volume, and the container environment and arguments needed to match devy's exported variables. Default tags SHALL be exact or major-pinned versions, never `latest` or a moving channel tag such as `stable`; the image digest is locked in `devy.lock` on first pull, so a major-pinned tag does not move once locked. A module MAY also pin its default tag to a built-in digest; when the dependency sets neither `image` nor `version`, devy SHALL pull and run `<image>@<digest>`, record that reference as the locked `image_digest`, and SHALL NOT replace it with a different locked digest, even with `--update`; a built-in digest changes only with a devy release. Setting `image` or `version` SHALL bypass the built-in digest. When a docker-managed `minio` sets `image` to a repository MinIO no longer publishes, devy SHALL warn `<repository> is no longer published and gets no security fixes; consider pgsty/silo`. These repositories are `minio/minio` on Docker Hub (also written with a `docker.io/`, `index.docker.io/` or `registry-1.docker.io/` prefix, reported as `minio/minio`) and `quay.io/minio/minio`.

| Module | Image (default tag) | Container port | Notes |
|---|---|---|---|
| postgresql | `postgres` (`16`) | 5432 | `POSTGRES_HOST_AUTH_METHOD=trust`, data `/var/lib/postgresql/data` |
| mysql | `mysql` (`8.0`) | 3306 | `MYSQL_ALLOW_EMPTY_PASSWORD=yes`; allowed `cli_args` passed as server arguments |
| mariadb | `mariadb` (`11`) | 3306 | `MARIADB_ALLOW_EMPTY_ROOT_PASSWORD=1`; allowed `cli_args` passed as server arguments |
| redis | `redis` (`7`) | 6379 | data `/data` |
| mongodb | `mongo` (`7`) | 27017 | data `/data/db` |
| kafka | `apache/kafka` (`3.7.0`) | 9092 | KRaft single node; advertised listener `PLAINTEXT://127.0.0.1:<host port>` |
| rabbitmq | `rabbitmq` (`3`) | 5672 | data `/var/lib/rabbitmq` |
| memcached | `memcached` (`1`) | 11211 | no volume |
| nginx | `nginx` (`1.30.5`) | 80 | no volume |
| elasticsearch | `docker.elastic.co/elasticsearch/elasticsearch` (`8.13.4`) | 9200 | `discovery.type=single-node`, `xpack.security.enabled=false` |
| opensearch | `opensearchproject/opensearch` (`2`) | 9200 | `discovery.type=single-node`, `DISABLE_SECURITY_PLUGIN=true` |
| meilisearch | `getmeili/meilisearch` (`v1.8`) | 7700 | `MEILI_MASTER_KEY` when `master_key` is set |
| minio | `pgsty/silo` (`RELEASE.2026-09-16T00-00-00Z`, digest-pinned to `sha256:635197cb9f36d01bee221d34d1c7d7960f6a95c48b0b6c01d99cd13bdae51a46`) | 9000 | Silo, the maintained continuation of the pgsty/minio community fork, because MinIO no longer publishes `minio/minio` on Docker Hub and `pgsty/minio`'s final release (`RELEASE.2026-08-04T00-00-00Z`, before the rename) receives no further fixes; Silo keeps the MinIO flags, `MINIO_*` variables and data format; runs `server /data`; publishes `console_port` to container port 9001 when set; credentials from `access_key`/`secret_key` |
| mailhog | `mailhog/mailhog` (`v1.0.1`) | 1025 | |
| vault | `hashicorp/vault` (`1.16`) | 8200 | dev mode with root token `root` when `dev_mode: true`; otherwise a file-storage server config |

#### Scenario: Postgres without a password
- **WHEN** `postgresql` is docker-managed and `devy up` completes
- **THEN** the exported `DATABASE_URL=postgres://localhost:<port>/postgres` connects without a password

#### Scenario: No moving channel tag
- **WHEN** `nginx` is docker-managed with no `version`
- **THEN** devy pulls `nginx:1.30.5`, not `nginx:stable`

#### Scenario: No floating default tag
- **WHEN** `minio` is docker-managed with no `version`
- **THEN** devy pulls `pgsty/silo@sha256:635197cb9f36d01bee221d34d1c7d7960f6a95c48b0b6c01d99cd13bdae51a46`, not `pgsty/silo:latest`
- **AND** `devy.lock` records `resolved_version: RELEASE.2026-09-16T00-00-00Z` and that reference as `image_digest`

#### Scenario: Override bypasses the built-in digest
- **WHEN** `minio` is docker-managed with `version: RELEASE.2026-09-16T00-00-00Z` or with `image` set
- **THEN** devy pulls the tag, not the built-in digest, and locks the digest it resolves

#### Scenario: Update keeps the built-in digest
- **WHEN** `minio` is docker-managed with no `version` or `image`, `devy.lock` records an older `pgsty/silo` digest, and the user runs `devy up --update`
- **THEN** devy pulls and locks `pgsty/silo@sha256:635197cb9f36d01bee221d34d1c7d7960f6a95c48b0b6c01d99cd13bdae51a46`

#### Scenario: Unmaintained upstream image
- **WHEN** a docker-managed `minio` sets `image: minio/minio` or `image: registry-1.docker.io/minio/minio:latest`
- **THEN** devy warns `minio/minio is no longer published and gets no security fixes; consider pgsty/silo`

#### Scenario: Unmaintained Quay image
- **WHEN** a docker-managed `minio` sets `image: quay.io/minio/minio`
- **THEN** devy warns `quay.io/minio/minio is no longer published and gets no security fixes; consider pgsty/silo`

### Requirement: Image tag and override
A docker-managed service's image tag SHALL be its `version` when set, and otherwise the module's default tag. A per-dependency `image` key SHALL replace the module's image repository. If `image` itself contains a tag, that tag MUST be used and `version` MUST be ignored with a warning.

#### Scenario: Version selects tag
- **WHEN** docker-managed `redis` is declared with `version: "7.2"`
- **THEN** devy uses image `redis:7.2`

#### Scenario: Registry mirror
- **WHEN** docker-managed `redis` is declared with `image: registry.corp.example/mirror/redis`
- **THEN** devy uses image `registry.corp.example/mirror/redis:7`

### Requirement: Image presence and pulling
For a docker-managed service, "installed" SHALL mean the resolved image reference is present locally (`<cli> image inspect` succeeds), and installing SHALL run `<cli> pull <reference>`. `devy up` prints `○ <dep> image present (docker)` or `→ Pulling <reference>` / `✓ Pulled <reference>`. A failed pull MUST fail with `Failed to pull <reference>`.

#### Scenario: Image already present
- **WHEN** `redis:7` is already present locally
- **THEN** `devy up` does not pull it and prints that the image is present

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

### Requirement: Container lifecycle
Starting a docker-managed service SHALL work as follows:
- **No container:** devy creates and starts one with `<cli> run -d`, publishing `127.0.0.1:<resolved port>:<container port>`, plus any secondary port (MinIO console).
- **Existing container, same configuration:** devy SHALL start it with `<cli> start`. The configuration is compared through a `sh.devy.config` label holding a hash of image reference, ports, environment, and arguments.
- **Existing container, different configuration:** devy SHALL remove and recreate it, keeping its volume.

A service SHALL be running when `<cli> inspect` reports its container state as running. Stopping SHALL run `<cli> stop` on the container.

#### Scenario: Port change recreates the container
- **WHEN** docker-managed `redis` ran on port 51000 and `devy.yml` now sets `port: 6380`
- **THEN** `devy up` removes the old container, creates a new one publishing `127.0.0.1:6380:6379`, and the volume's data is preserved

#### Scenario: Restart reuses the container
- **WHEN** docker-managed `redis` was stopped and nothing in its configuration changed
- **THEN** `devy start redis` runs `docker start` on the existing container

### Requirement: Readiness of containers
After starting a docker-managed service, devy SHALL use the module's existing health check against `127.0.0.1:<resolved port>`, with the same attempt budgets and warning-only failure behavior as package-managed services.

#### Scenario: Redis container health
- **WHEN** docker-managed `redis` starts on port 51000
- **THEN** devy waits until `PING` on `127.0.0.1:51000` returns `+PONG`

### Requirement: Kafka in containers
A docker-managed Kafka SHALL always run in KRaft mode. When `kraft` is not `true`, `devy up` and `devy check` SHALL warn that zookeeper mode is not supported with docker and that KRaft is used. Devy MUST NOT start a zookeeper container.

#### Scenario: Kraft unset
- **WHEN** docker-managed `kafka` is declared without `kraft`
- **THEN** devy warns that KRaft is used and starts a single Kafka container

### Requirement: Docker services are not supported for generic dependencies
A dependency handled by the generic module (no built-in image definition) that sets `service_manager: docker` MUST be rejected during config validation with `<dep>: service_manager and image apply only to built-in services`, the same message as for any other non-service dependency.

#### Scenario: Unknown dependency
- **WHEN** `devy.yml` declares `- foo: { service_manager: docker }`
- **THEN** `devy up` and `devy check` fail with the not-supported message

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
