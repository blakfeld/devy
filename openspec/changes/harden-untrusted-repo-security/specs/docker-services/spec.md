# Spec Delta

## ADDED Requirements

### Requirement: Container credentials and ownership
devy SHALL pass credentials to containers (`MINIO_ROOT_USER`, `MINIO_ROOT_PASSWORD`, `MEILI_MASTER_KEY`) through a mode-0600 env file passed with `--env-file`, not as `-e` arguments, and SHALL delete the file after the container is created. Before reusing an existing container with the expected name, devy SHALL check that its `sh.devy.project` label equals the current project root, and otherwise fail with `container <name> belongs to another project`.

#### Scenario: Credentials not in argv
- **WHEN** a docker-managed `minio` with `secret_key` is started
- **THEN** the `docker run` arguments do not contain the secret

## MODIFIED Requirements

### Requirement: Built-in service images
Every built-in service module SHALL define a container image, a default tag, the container port, a data path for its volume, and the container environment and arguments needed to match devy's exported variables. Default tags SHALL be exact versions, never `latest`.

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
| nginx | `nginx` (`stable`) | 80 | no volume |
| elasticsearch | `docker.elastic.co/elasticsearch/elasticsearch` (`8.13.4`) | 9200 | `discovery.type=single-node`, `xpack.security.enabled=false` |
| opensearch | `opensearchproject/opensearch` (`2`) | 9200 | `discovery.type=single-node`, `DISABLE_SECURITY_PLUGIN=true` |
| meilisearch | `getmeili/meilisearch` (`v1.8`) | 7700 | `MEILI_MASTER_KEY` when `master_key` is set |
| minio | `minio/minio` (`RELEASE.2024-06-13T22-53-53Z`) | 9000 | runs `server /data`; publishes `console_port` to container port 9001 when set; credentials from `access_key`/`secret_key` |
| mailhog | `mailhog/mailhog` (`v1.0.1`) | 1025 | |
| vault | `hashicorp/vault` (`1.16`) | 8200 | dev mode with root token `root` when `dev_mode: true`; otherwise a file-storage server config |

#### Scenario: Postgres without a password
- **WHEN** `postgresql` is docker-managed and `devy up` completes
- **THEN** the exported `DATABASE_URL=postgres://localhost:<port>/postgres` connects without a password

#### Scenario: No floating default tag
- **WHEN** `minio` is docker-managed with no `version`
- **THEN** devy pulls `minio/minio:RELEASE.2024-06-13T22-53-53Z`, not `minio/minio:latest`
