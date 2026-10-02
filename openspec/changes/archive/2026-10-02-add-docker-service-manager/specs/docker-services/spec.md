## Purpose

Defines how devy runs service dependencies as per-project containers through a Docker-compatible CLI (Docker or Podman), as an alternative to package-manager-managed services.

## ADDED Requirements

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
Every built-in service module SHALL define a container image, a default tag, the container port, a data path for its volume, and the container environment and arguments needed to match devy's exported variables:

| Module | Image (default tag) | Container port | Notes |
|---|---|---|---|
| postgresql | `postgres` (`16`) | 5432 | `POSTGRES_HOST_AUTH_METHOD=trust`, data `/var/lib/postgresql/data` |
| mysql | `mysql` (`8.0`) | 3306 | `MYSQL_ALLOW_EMPTY_PASSWORD=yes`; sanitized `cli_args` passed as server arguments |
| mariadb | `mariadb` (`11`) | 3306 | `MARIADB_ALLOW_EMPTY_ROOT_PASSWORD=1`; sanitized `cli_args` passed as server arguments |
| redis | `redis` (`7`) | 6379 | data `/data` |
| mongodb | `mongo` (`7`) | 27017 | data `/data/db` |
| kafka | `apache/kafka` (`3.7.0`) | 9092 | KRaft single node; advertised listener `PLAINTEXT://127.0.0.1:<host port>` |
| rabbitmq | `rabbitmq` (`3`) | 5672 | data `/var/lib/rabbitmq` |
| memcached | `memcached` (`1`) | 11211 | no volume |
| nginx | `nginx` (`stable`) | 80 | no volume |
| elasticsearch | `docker.elastic.co/elasticsearch/elasticsearch` (`8.13.4`) | 9200 | `discovery.type=single-node`, `xpack.security.enabled=false` |
| opensearch | `opensearchproject/opensearch` (`2`) | 9200 | `discovery.type=single-node`, `DISABLE_SECURITY_PLUGIN=true` |
| meilisearch | `getmeili/meilisearch` (`v1.8`) | 7700 | `MEILI_MASTER_KEY` when `master_key` is set |
| minio | `minio/minio` (`latest`) | 9000 | runs `server /data`; publishes `console_port` to container port 9001 when set; credentials from `access_key`/`secret_key` |
| mailhog | `mailhog/mailhog` (`latest`) | 1025 | |
| vault | `hashicorp/vault` (`1.16`) | 8200 | dev mode with root token `root` when `dev_mode: true`; otherwise a file-storage server config |

#### Scenario: Postgres without a password
- **WHEN** `postgresql` is docker-managed and `devy up` completes
- **THEN** the exported `DATABASE_URL=postgres://localhost:<port>/postgres` connects without a password

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
Each docker-managed service SHALL run in a container named `devy-<project>-<canonical-name>`. `<project>` is the project `name` lower-cased with characters outside `[a-z0-9-]` replaced by `-`, followed by `-` and the first 8 hex characters of a hash of the project root path. The container SHALL carry labels `sh.devy.project=<project root>` and `sh.devy.service=<canonical-name>`. Persistent data SHALL be stored in a named volume with the same name as the container, mounted at the module's data path.

#### Scenario: Two projects run redis
- **WHEN** projects at `/src/a` and `/src/b`, both named `app`, each run docker-managed `redis`
- **THEN** two distinct containers `devy-app-<hashA>-redis` and `devy-app-<hashB>-redis` run concurrently with separate volumes

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
