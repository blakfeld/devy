## MODIFIED Requirements

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

## ADDED Requirements

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
