# service-modules Specification

## Purpose
Catalogs the built-in service dependencies devy can run (databases, caches, queues, search engines, and tools), including their default ports, accepted config keys, health checks, and the environment variables they contribute.

## Requirements

### Requirement: Common service behavior
Every service module SHALL declare a default port and run its health check against `127.0.0.1` on the resolved port. The default port applies only when no port is configured or resolved. In practice that means `start`, `stop`, `restart`, `status` and `check` (see service-ports), because during `devy up` an unconfigured service gets its locked or a random port instead. Every service module SHALL also accept only its documented extra config keys (others are reported by `devy check`), and contribute `<NAME>_HOST` and `<NAME>_PORT` variables in addition to any module-specific variables.

#### Scenario: Unknown key on a service
- **WHEN** `devy.yml` declares `redis` with `maxmemory: 1gb`
- **THEN** `devy check` reports `maxmemory` as an unrecognized config key for `redis` and lists the known keys

### Requirement: PostgreSQL
The `postgresql` module (alias `postgres`) SHALL default to port 5432 with extra key `port`, SHALL be healthy when a PostgreSQL StartupMessage receives a reply beginning with `R` or `E`, and SHALL export `DATABASE_URL=postgres://localhost:<port>/postgres`.

#### Scenario: Default environment
- **WHEN** `postgresql` is declared with `port: 5432`
- **THEN** devy exports `DATABASE_URL=postgres://localhost:5432/postgres`

### Requirement: MySQL and MariaDB
The `mysql` and `mariadb` modules SHALL default to port 3306 with extra keys `port` and `cli_args`, SHALL be healthy when the server greeting's protocol byte is `0x0a`, and SHALL export `DATABASE_URL=mysql://root@127.0.0.1:<port>/`.

#### Scenario: MySQL custom port
- **WHEN** `mysql` is declared with `port: 3307`
- **THEN** devy exports `DATABASE_URL=mysql://root@127.0.0.1:3307/`

### Requirement: Redis
The `redis` module SHALL default to port 6379 with extra key `port`, SHALL be healthy when `PING` returns `+PONG`, and SHALL export `REDIS_URL=redis://127.0.0.1:<port>`.

#### Scenario: Redis custom port
- **WHEN** `redis` is declared with `port: 6380`
- **THEN** devy exports `REDIS_URL=redis://127.0.0.1:6380`

### Requirement: MongoDB
The `mongodb` module (alias `mongo`) SHALL default to port 27017 with extra key `port` and SHALL use a TCP health check with up to 60 attempts. It SHALL use service name `mongod` on Linux and `mongodb-community` elsewhere. On Homebrew it SHALL install `mongodb-community`, which requires `tap: mongodb/brew`. Under nix it SHALL run `mongod` from the project profile.

#### Scenario: Health wait budget
- **WHEN** `devy up` starts `mongodb`
- **THEN** devy polls TCP connectivity on the port up to 60 times before warning

#### Scenario: MongoDB under nix
- **WHEN** `devy up` starts `mongodb` resolved to port 51000 with the nix backend
- **THEN** devy runs `mongod --port 51000 --bind_ip 127.0.0.1 --dbpath <project_root>/.devy/data/mongodb`

### Requirement: Kafka
The `kafka` module SHALL default to port 9092 with extra keys `port` and `kraft`, and SHALL use a TCP health check with up to 120 attempts.

On backends other than nix, unless `kraft: true`, it SHALL start `zookeeper` before Kafka and stop it after Kafka, treating zookeeper failures as warnings.

Under nix, devy SHALL always run Kafka in KRaft mode, because nixpkgs ships Kafka 4, which has no ZooKeeper. It SHALL run Kafka from the project profile's Kafka distribution with a devy-generated `server.properties` under `.devy/data/kafka/`. The listener is `PLAINTEXT://127.0.0.1:<port>`, and the controller listener uses a second free port that is reused on later starts. devy SHALL format the storage directory once before the first start, and MUST NOT start or stop zookeeper. When `kraft` is not set, devy SHALL print an info line saying it is running in KRaft mode.

#### Scenario: KRaft mode
- **WHEN** `kafka` is declared with `kraft: true`
- **THEN** devy starts only Kafka and does not start zookeeper

#### Scenario: Zookeeper mode
- **WHEN** `kafka` is declared without `kraft` and the backend is brew, apt, or winget
- **THEN** devy starts zookeeper first and then Kafka

#### Scenario: Zookeeper mode is not available under nix
- **WHEN** `kafka` is declared without `kraft` and the backend is nix
- **THEN** devy starts Kafka in KRaft mode and does not start zookeeper

#### Scenario: KRaft under nix
- **WHEN** `devy up` starts `kafka` with `kraft: true` under nix for the first time
- **THEN** devy formats `.devy/data/kafka` storage and then starts the Kafka server listening on `127.0.0.1:<port>`
- **AND** on later starts devy does not format the storage again

### Requirement: Simple TCP services
The `rabbitmq` (port 5672), `memcached` (port 11211), and `nginx` (port 80) modules SHALL accept extra key `port`, SHALL use a TCP health check, and SHALL contribute only the host and port variables; on Linux, starting `nginx` on a port below 1024 SHALL warn that root or `CAP_NET_BIND_SERVICE` is required. Under `devy up` this only happens when `port` is set explicitly below 1024, since an unconfigured nginx gets a locked or random port. `devy start nginx` without a configured port uses the default 80 and always warns on Linux.

#### Scenario: Nginx privileged port on Linux
- **WHEN** `nginx` is started on Linux with port 80
- **THEN** devy warns that binding a port below 1024 requires root or CAP_NET_BIND_SERVICE

### Requirement: Elasticsearch and OpenSearch
The `elasticsearch` (alias `elastic`) and `opensearch` modules SHALL default to port 9200 with extra key `port` and up to 120 health attempts; Elasticsearch SHALL be healthy when `GET /` returns a cluster `status` of green, yellow, or red (or no status), and OpenSearch SHALL use a TCP health check. On Homebrew, Elasticsearch SHALL install `elasticsearch-full`, but start and check the service as `elasticsearch` (or the alias as written). devy neither adds nor checks the `elastic/tap` tap, so unless the user sets `tap: elastic/tap` the install fails.

#### Scenario: Elasticsearch unexpected status
- **WHEN** `GET http://127.0.0.1:9200/` returns `status: "unknown"`
- **THEN** the Elasticsearch health check fails

### Requirement: Meilisearch
The `meilisearch` module (alias `meili`) SHALL default to port 7700 with extra keys `port` and `master_key`, SHALL use a TCP health check with up to 60 attempts, and when `master_key` is set to a string SHALL export `MEILI_MASTER_KEY`. In that case `devy up` SHALL warn on every run, from the post-setup step, `Meilisearch: master_key is set in plaintext in devy.yml…`.

#### Scenario: Master key set
- **WHEN** `meilisearch` is declared with `master_key: secret`
- **THEN** `devy up` exports `MEILI_MASTER_KEY=secret` and warns about plaintext storage

### Requirement: MinIO
The `minio` module SHALL default to port 9000 with extra keys `port`, `console_port`, `access_key`, and `secret_key`, SHALL export `MINIO_ROOT_USER` from `access_key`, `MINIO_ROOT_PASSWORD` from `secret_key`, and `MINIO_CONSOLE_ADDRESS=:<console_port>` when each is set, and when credentials are set `devy check` SHALL warn (without counting an issue) that they are written in plaintext to `.shadowenv.d`.

#### Scenario: Console port
- **WHEN** `minio` is declared with `console_port: 9001`
- **THEN** devy exports `MINIO_CONSOLE_ADDRESS=:9001`

### Requirement: MailHog
The `mailhog` module SHALL use `smtp_port` (default 1025) as its port key and only extra key, SHALL use a TCP health check, and SHALL export `SMTP_HOST=127.0.0.1` and `SMTP_PORT=<smtp_port>`.

#### Scenario: Custom SMTP port
- **WHEN** `mailhog` is declared with `smtp_port: 2525`
- **THEN** devy exports `SMTP_PORT=2525` and `MAILHOG_PORT=2525`

### Requirement: Vault
The `vault` module (alias `hashicorp-vault`) SHALL default to port 8200 with extra keys `port` and `dev_mode`, SHALL use a TCP health check, and SHALL export `VAULT_ADDR=http://127.0.0.1:<port>`, plus `VAULT_TOKEN=root` when `dev_mode: true`. In that case `devy up` warns on every run: `Vault: VAULT_TOKEN="root" (dev mode). Override via environment in devy.yml before deploying.` `dev_mode` SHALL only affect these variables; devy does not start Vault in dev mode.

#### Scenario: Dev mode
- **WHEN** `vault` is declared with `dev_mode: true`
- **THEN** devy exports `VAULT_ADDR=http://127.0.0.1:8200` and `VAULT_TOKEN=root`

### Requirement: Nix launch definitions for services
Every built-in service module SHALL define how it is launched under nix, so the running process listens on the resolved port on `127.0.0.1` and keeps state under `.devy/data/<canonical-name>/`, which is also its working directory. Unix sockets (`<sockets>` below) go in `<data>` unless the socket path would exceed 100 bytes. In that case they go in a short per-project directory `/tmp/devy-<hash>`, because macOS limits socket paths to 103 bytes.

| Module | Launch |
|---|---|
| postgresql | `postgres -D <data> -p <port> -k <sockets> -c listen_addresses=127.0.0.1`, after a one-time `initdb -D <data>` |
| mysql | MySQL's own `mysqld` (from the `mysql` package, not the merged profile) `--no-defaults --datadir=<data> --port=<port> --bind-address=127.0.0.1 --socket=<sockets>/mysql.sock --mysqlx=OFF`, after a one-time `mysqld --no-defaults --initialize-insecure --datadir=<data>`, plus each sanitized `cli_args` token |
| mariadb | the same as mysql using `mariadbd`, without `--mysqlx=OFF`, initialized once with `mariadb-install-db --no-defaults --datadir=<data> --auth-root-authentication-method=normal` |
| redis | `redis-server --port <port> --bind 127.0.0.1 --dir <data>` |
| memcached | `memcached -p <port> -l 127.0.0.1` |
| rabbitmq | `rabbitmq-server`, with environment `RABBITMQ_NODE_PORT=<port>`, `RABBITMQ_DIST_PORT=<a second free port, kept across starts>`, `RABBITMQ_NODENAME=devy-<hash>@localhost`, `RABBITMQ_NODE_IP_ADDRESS=127.0.0.1`, and `RABBITMQ_MNESIA_BASE`/`RABBITMQ_LOG_BASE` under `<data>` |
| nginx | `nginx -p <data> -c <data>/nginx.conf`, where devy writes a `daemon off;` config listening on `127.0.0.1:<port>` |
| elasticsearch, opensearch | the server binary with `-E http.host=127.0.0.1 -E http.port=<port> -E path.data=<data>/data -E path.logs=<data>/logs` |
| meilisearch | `meilisearch --http-addr 127.0.0.1:<port> --db-path <data>`, plus `--master-key <key>` when set |
| minio | `minio server <data> --address 127.0.0.1:<port>`, plus `--console-address :<console_port>` when set, with `MINIO_ROOT_USER`/`MINIO_ROOT_PASSWORD` in the environment when set |
| mailhog | `MailHog -smtp-bind-addr 127.0.0.1:<smtp_port>` |
| vault | with `dev_mode: true`, `vault server -dev -dev-listen-address=127.0.0.1:<port> -dev-root-token-id=root`; otherwise `vault server -config=<data>/vault.hcl`, where devy writes file storage under `<data>` and a non-TLS listener on `127.0.0.1:<port>` |
| mongodb | `mongod --port <port> --bind_ip 127.0.0.1 --dbpath <data>` |
| kafka | as described in the Kafka requirement |

#### Scenario: Redis listens on the assigned port
- **WHEN** `devy up` assigns port 51000 to `redis` under nix
- **THEN** `redis-cli -p 51000 ping` returns `PONG` after the service starts

#### Scenario: Vault without dev mode
- **WHEN** `vault` is declared without `dev_mode` under nix
- **THEN** devy writes `.devy/data/vault/vault.hcl` with a listener on `127.0.0.1:<port>` and starts `vault server -config` with it
