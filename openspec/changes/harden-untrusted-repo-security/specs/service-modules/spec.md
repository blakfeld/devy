# Spec Delta

## ADDED Requirements

### Requirement: Service secrets stay off command lines
Credentials that devy passes to a service SHALL be given through the process environment (nix) or a mode-0600 env file passed with `--env-file` (docker), and never as command-line arguments. These credentials are the MinIO `access_key`/`secret_key` and the Meilisearch `master_key`.

#### Scenario: Meilisearch key not visible in ps
- **WHEN** `meilisearch` is declared with `master_key: abc123` under nix and is running
- **THEN** no process's arguments contain `abc123`

## MODIFIED Requirements

### Requirement: MinIO
The `minio` module SHALL default to port 9000 with extra keys `port`, `console_port`, `access_key`, and `secret_key`, SHALL export `MINIO_ROOT_USER` from `access_key`, `MINIO_ROOT_PASSWORD` from `secret_key`, and `MINIO_CONSOLE_ADDRESS=127.0.0.1:<console_port>` when each is set, and when credentials are set `devy check` SHALL warn (without counting an issue) that they are written in plaintext to `.shadowenv.d`. When no credentials are set, `devy check` and `devy up` SHALL warn that MinIO will use its default `minioadmin` credentials. `console_port` SHALL be validated like other port keys.

#### Scenario: Console port
- **WHEN** `minio` is declared with `console_port: 9001`
- **THEN** devy exports `MINIO_CONSOLE_ADDRESS=127.0.0.1:9001`

#### Scenario: Default credentials warning
- **WHEN** `minio` is declared without `access_key` and `secret_key`
- **THEN** `devy check` warns that the default `minioadmin` credentials are in use

### Requirement: Nix launch definitions for services
Every built-in service module SHALL define how it is launched under nix, so that every listening socket of the running process, including admin consoles, web UIs and clustering ports, binds to `127.0.0.1`, the main one on the resolved port, and keeps state under `.devy/data/<canonical-name>/`, which is also its working directory. Unix sockets (`<sockets>` below) go in `<data>` unless the socket path would exceed 100 bytes. In that case they go in a private per-user, per-project directory created as defined in filesystem-safety, because macOS limits socket paths to 103 bytes.

| Module | Launch |
|---|---|
| postgresql | `postgres -D <data> -p <port> -k <sockets> -c listen_addresses=127.0.0.1`, after a one-time `initdb -D <data>` |
| mysql | MySQL's own `mysqld` (from the `mysql` package, not the merged profile) `--no-defaults --datadir=<data>`, then each allowed `cli_args` token, then `--port=<port> --bind-address=127.0.0.1 --socket=<sockets>/mysql.sock --mysqlx=OFF`, after a one-time `mysqld --no-defaults --initialize-insecure --datadir=<data>` |
| mariadb | the same as mysql using `mariadbd`, without `--mysqlx=OFF`, initialized once with `mariadb-install-db --no-defaults --datadir=<data> --auth-root-authentication-method=normal` |
| redis | `redis-server --port <port> --bind 127.0.0.1 --dir <data>` |
| memcached | `memcached -p <port> -l 127.0.0.1` |
| rabbitmq | `rabbitmq-server`, with environment `RABBITMQ_NODE_PORT=<port>`, `RABBITMQ_DIST_PORT=<a second free port, kept across starts>`, `RABBITMQ_NODENAME=devy-<hash>@localhost`, `RABBITMQ_NODE_IP_ADDRESS=127.0.0.1`, `ERL_EPMD_ADDRESS=127.0.0.1`, `RABBITMQ_SERVER_ADDITIONAL_ERL_ARGS=-kernel inet_dist_use_interface {127,0,0,1}`, and `RABBITMQ_MNESIA_BASE`/`RABBITMQ_LOG_BASE` under `<data>` |
| nginx | `nginx -p <data> -c <data>/nginx.conf`, where devy writes a `daemon off;` config listening on `127.0.0.1:<port>` |
| elasticsearch, opensearch | the server binary with `-E http.host=127.0.0.1 -E transport.host=127.0.0.1 -E http.port=<port> -E path.data=<data>/data -E path.logs=<data>/logs` |
| meilisearch | `meilisearch --http-addr 127.0.0.1:<port> --db-path <data>`, with `MEILI_MASTER_KEY` in the environment when `master_key` is set |
| minio | `minio server <data> --address 127.0.0.1:<port> --console-address 127.0.0.1:<console_port>`, where `<console_port>` is the configured one or a free port chosen by devy, with `MINIO_ROOT_USER`/`MINIO_ROOT_PASSWORD` in the environment when set |
| mailhog | `MailHog -smtp-bind-addr 127.0.0.1:<smtp_port> -ui-bind-addr 127.0.0.1:<ui_port> -api-bind-addr 127.0.0.1:<ui_port>`, where `<ui_port>` is 8025 |
| vault | with `dev_mode: true`, `vault server -dev -dev-listen-address=127.0.0.1:<port> -dev-root-token-id=root`; otherwise `vault server -config=<data>/vault.hcl`, where devy writes file storage under `<data>` and a non-TLS listener on `127.0.0.1:<port>` |
| mongodb | `mongod --port <port> --bind_ip 127.0.0.1 --dbpath <data>` |
| kafka | as described in the Kafka requirement |

#### Scenario: Redis listens on the assigned port
- **WHEN** `devy up` assigns port 51000 to `redis` under nix
- **THEN** `redis-cli -p 51000 ping` returns `PONG` after the service starts

#### Scenario: Vault without dev mode
- **WHEN** `vault` is declared without `dev_mode` under nix
- **THEN** devy writes `.devy/data/vault/vault.hcl` with a listener on `127.0.0.1:<port>` and starts `vault server -config` with it

#### Scenario: MailHog UI is loopback-only
- **WHEN** `mailhog` runs under nix
- **THEN** port 8025 accepts connections on `127.0.0.1` and refuses them on the machine's LAN address

#### Scenario: MinIO console is loopback-only
- **WHEN** `minio` runs under nix with or without `console_port`
- **THEN** the console listens only on `127.0.0.1`

#### Scenario: Forced bind wins over cli_args order
- **WHEN** `mysql` runs under nix
- **THEN** `--bind-address=127.0.0.1` appears after every `cli_args` token on the command line
