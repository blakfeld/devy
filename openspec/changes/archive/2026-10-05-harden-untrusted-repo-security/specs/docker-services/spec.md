# Spec Delta

## ADDED Requirements

### Requirement: Container credentials and ownership
devy SHALL pass credentials to containers (`MINIO_ROOT_USER`, `MINIO_ROOT_PASSWORD`, `MEILI_MASTER_KEY`) through a mode-0600 env file passed with `--env-file`, not as `-e` arguments, and SHALL delete the file after the container is created. Before reusing an existing container with the expected name, devy SHALL check that its `sh.devy.project` label equals the current project root, and otherwise fail with `container <name> belongs to another project`.

#### Scenario: Credentials not in argv
- **WHEN** a docker-managed `minio` with `secret_key` is started
- **THEN** the `docker run` arguments do not contain the secret

## MODIFIED Requirements

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
