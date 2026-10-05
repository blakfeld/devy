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
The `minio` module SHALL default to port 9000 with extra keys `port`, `console_port`, `access_key`, and `secret_key`, SHALL export `MINIO_ROOT_USER` from `access_key`, `MINIO_ROOT_PASSWORD` from `secret_key`, and `MINIO_CONSOLE_ADDRESS=127.0.0.1:<console_port>` when each is set (under nix without `console_port`, `MINIO_CONSOLE_ADDRESS` SHALL name the console port devy chose, which `devy up` records in `<data>/console_port` before the service first starts and every launch reuses, so the two always agree; read-only commands such as `devy status` and `devy exec` only read an already recorded port and write nothing; a docker-managed MinIO without `console_port` publishes no console port and exports none), and when credentials are set `devy check` SHALL warn (without counting an issue) that they are written in plaintext to `.shadowenv.d`. When no credentials are set, `devy check` and `devy up` SHALL warn that MinIO will use its default `minioadmin` credentials. `console_port` SHALL be validated like other port keys.

#### Scenario: Console port
- **WHEN** `minio` is declared with `console_port: 9001`
- **THEN** devy exports `MINIO_CONSOLE_ADDRESS=127.0.0.1:9001`

#### Scenario: MinIO auto-picked console port exported
- **WHEN** `minio` is declared without `console_port` and the backend is nix
- **THEN** devy exports `MINIO_CONSOLE_ADDRESS=127.0.0.1:<p>`, where `<p>` is the port the nix launch passes to `--console-address`

#### Scenario: Default credentials warning
- **WHEN** `minio` is declared without `access_key` and `secret_key`
- **THEN** `devy check` warns that the default `minioadmin` credentials are in use

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
| mongodb | `mongod --port <port> --bind_ip 127.0.0.1 --dbpath <data> --unixSocketPrefix <socket dir>`, where `<socket dir>` is the same private socket directory postgres and mysql use (never `/tmp`) |
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

### Requirement: Writable search server config under nix
When devy starts `elasticsearch` or `opensearch` with the nix backend, it SHALL run the server with a project-local, writable config directory at `<project_root>/.devy/data/<canonical-name>/config/`, set as `ES_PATH_CONF` or `OPENSEARCH_PATH_CONF` respectively.
- **First start:** when that directory does not exist, devy SHALL create it as a writable copy of the installed package's `config/` directory. Later starts SHALL reuse it unchanged, keeping user edits.
- **Relative JVM paths:** in the copied `jvm.options`, devy SHALL rewrite the relative log and heap-dump paths to absolute paths under `.devy/data/<canonical-name>/logs/` and `.devy/data/<canonical-name>/data/`, because the package's start scripts run from the read-only package directory. This happens only when the directory is first created.
- **Read-only store:** devy MUST NOT write into the Nix store.
- **Elasticsearch** SHALL additionally run with `-E xpack.ml.enabled=false`, and with `ES_HOME` set to the installed package's directory.
- **OpenSearch:** when its package bundles the security plugin, it SHALL run with that plugin disabled, so it serves plain HTTP on `127.0.0.1:<port>`.

#### Scenario: First start seeds the config
- **WHEN** `devy up` starts `opensearch` under nix for the first time
- **THEN** `.devy/data/opensearch/config/` exists with the package's `opensearch.yml` and `jvm.options`, owner-writable, and the server's keystore is created there
- **AND** the copied `jvm.options` writes its GC log under `.devy/data/opensearch/logs/`

#### Scenario: User edits survive restarts
- **WHEN** the user edits `.devy/data/opensearch/config/jvm.options` and runs `devy restart opensearch`
- **THEN** devy does not overwrite the file, and the server starts with the edited options

#### Scenario: Elasticsearch becomes healthy
- **WHEN** `devy up` starts `elasticsearch` under nix with unfree installs allowed
- **THEN** the Elasticsearch health check passes on the resolved port, and nothing under `/nix/store` is modified

### Requirement: Service secrets stay off command lines
Credentials that devy passes to a service SHALL be given through the process environment (nix) or a mode-0600 env file passed with `--env-file` (docker), and never as command-line arguments. These credentials are the MinIO `access_key`/`secret_key` and the Meilisearch `master_key`.

#### Scenario: Meilisearch key not visible in ps
- **WHEN** `meilisearch` is declared with `master_key: abc123` under nix and is running
- **THEN** no process's arguments contain `abc123`

### Requirement: Loopback listeners under Homebrew and apt
Under the brew and apt backends, `devy up` SHALL keep Kafka, the ZooKeeper it starts for Kafka without KRaft, and RabbitMQ on loopback, following the MySQL `my.cnf.d/devy.cnf` rule: devy writes only files it owns (first line `# devy-managed`) with `fs_safe::write_atomic`, never silently rewrites a config it does not own, and instead warns once per distinct content of that file, naming the lines to add. A rewrite SHALL keep the existing file's permission bits. A write that fails (for example `/etc` is not writable without root, or the file is a symlink) SHALL be a warning, shown once, naming only the lines to add (never the file's other content), not a failed `up`. Docker-managed services skip this, and nix launches with its own loopback configs.
- **Kafka** reads `$(brew --prefix)/etc/kafka/server.properties` (brew) or `/etc/kafka/server.properties` / `/etc/kafka/kraft/server.properties` (apt). When every `listeners` entry already binds `127.0.0.1`, `localhost` or `::1`, nothing happens. When the brew file still has the stock `listeners=PLAINTEXT://:9092,CONTROLLER://:9093` and loopback `advertised.listeners`, devy rewrites that line to `listeners=PLAINTEXT://127.0.0.1:9092,CONTROLLER://127.0.0.1:9093`, keeping the rest of the file, because `brew services` can't be pointed at another file. Otherwise (and always under apt) devy warns; a setting line whose key Java would end at anything but `=` (Java ends a key at the first `=`, `:` or whitespace, then takes one `=` or `:` after any whitespace as the separator, so `key: value`, `key value` and `key value=x` are read differently from `key=value`, the last setting winning) always gets a warning, as does any line ending in an odd number of backslashes (a Java continuation line). When no file is found, or it cannot be read, devy warns.
- **ZooKeeper** reads `$(brew --prefix)/etc/zookeeper/zoo.cfg` (brew) or `/etc/zookeeper/conf/zoo.cfg` (apt) and needs `clientPortAddress=127.0.0.1` and `admin.enableServer=false`, a loopback `secureClientPortAddress` when `secureClientPort` is set, and a loopback `metricsProvider.httpHost` when `metricsProvider.className` (the Prometheus provider, which listens on port 7000 by default) or `metricsProvider.httpPort` is set, all in `key=value` lines as for Kafka. A brew `zoo.cfg` holding only `key=value` lines with the stock sample's keys (`tickTime`, `initLimit`, `syncLimit`, `dataDir`, `clientPort`) gets the first two lines appended; any other gets a warning, as does a missing or unreadable `zoo.cfg`.
- **RabbitMQ**: devy owns `<etc>/rabbitmq/conf.d/90-devy.conf`, where `<etc>` is `$(brew --prefix)/etc` or `/etc`, read automatically by RabbitMQ 3.9+. It sets `listeners.tcp.default = 127.0.0.1:<port>` (the dependency's `port`), `distribution.listener.interface = 127.0.0.1` only when the node is named `<name>@localhost` (otherwise `rabbitmqctl` could not reach the node), and a loopback listener line for each of `rabbitmq_management`, `rabbitmq_prometheus`, `rabbitmq_stomp`, `rabbitmq_mqtt`, `rabbitmq_stream` (with `stream.advertised_host = localhost` unless the user's config sets `stream.advertised_host`), `rabbitmq_web_stomp` and `rabbitmq_web_mqtt` that is enabled, explicitly in `enabled_plugins` or as a dependency of an enabled plugin (any `*_management` plugin, `rabbitmq_top` and `rabbitmq_tracing` imply `rabbitmq_management`; `web_stomp` implies `stomp`, `web_mqtt` implies `mqtt`, the `*_examples` plugins imply their web plugin, `stream_management` implies `stream`), since RabbitMQ refuses settings for disabled plugins. A listener the user's own `rabbitmq.conf` or other `conf.d` files already set is never overridden; devy warns once if it doesn't bind loopback. devy configures no TLS listener, so it also warns once about the user's `listeners.ssl.*` (and `stomp`/`mqtt`/`stream` `.listeners.ssl.*`) lines that don't bind loopback, and about a `management.ssl.port`, `prometheus.ssl.port` or `web_*.ssl.port` without a loopback `*.ssl.ip`. devy writes `rabbitmq-env.conf` with `NODENAME="rabbit@localhost"` and `export ERL_EPMD_ADDRESS="127.0.0.1"` (no `NODE_IP_ADDRESS`, which RabbitMQ turns into a listener that overrides the config file's) only when there is none or it is devy's own; an existing foreign one gets a warning naming the lines to add when it lacks an exported `ERL_EPMD_ADDRESS=127.0.0.1` (the file is sourced without `set -a`, so the assignment needs `export` on its line, a later `export ERL_EPMD_ADDRESS` or a preceding `set -a`), when its node is not `@localhost`, or when it sets `NODE_IP_ADDRESS` or `NODE_PORT` (with or without the `RABBITMQ_` prefix; either one replaces the config file's AMQP listener, on every interface when `NODE_IP_ADDRESS` is unset) without a loopback `NODE_IP_ADDRESS` and a `NODE_PORT` equal to the dependency's port (default 5672). When it sets `CONFIG_FILE`, `CONFIG_FILES` or `ENABLED_PLUGINS_FILE` to a location other than the defaults devy reads (`<etc>/rabbitmq/rabbitmq.conf` or `<etc>/rabbitmq/rabbitmq`, `<etc>/rabbitmq/conf.d` and `<etc>/rabbitmq/enabled_plugins`, compared after dropping trailing slashes, and as resolved paths when both exist), devy warns once that those files are not inspected. A foreign file at devy's `conf.d` path is left alone with a warning.

#### Scenario: Stock Homebrew Kafka
- **WHEN** `kafka` is installed with brew and `etc/kafka/server.properties` has the stock listeners
- **THEN** `devy up` rewrites the listeners to `PLAINTEXT://127.0.0.1:9092,CONTROLLER://127.0.0.1:9093` and leaves every other line unchanged

#### Scenario: User-edited Kafka config
- **WHEN** the brew `server.properties` adds an `EXTERNAL://:9094` listener
- **THEN** devy leaves the file unchanged and warns once, repeating the warning only after the file changes

#### Scenario: Homebrew RabbitMQ env file
- **WHEN** brew's stock `rabbitmq-env.conf` sets only `NODE_IP_ADDRESS=127.0.0.1`
- **THEN** devy leaves it unchanged, warns that `ERL_EPMD_ADDRESS` is missing, and writes `conf.d/90-devy.conf` binding AMQP, Erlang distribution and the management UI to `127.0.0.1`
