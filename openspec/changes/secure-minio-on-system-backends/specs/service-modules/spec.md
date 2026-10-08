## MODIFIED Requirements

### Requirement: MinIO
The `minio` module SHALL default to port 9000 with extra keys `port`, `console_port`, `access_key`, and `secret_key`, SHALL export `MINIO_ROOT_USER` from `access_key`, `MINIO_ROOT_PASSWORD` from `secret_key`, and `MINIO_CONSOLE_ADDRESS=127.0.0.1:<console_port>` when each is set (under nix without `console_port`, `MINIO_CONSOLE_ADDRESS` SHALL name the console port devy chose, which `devy up` records in `<data>/console_port` before the service first starts and every launch reuses, so the two always agree; read-only commands such as `devy status` and `devy exec` only read an already recorded port and write nothing; a docker-managed MinIO without `console_port` publishes no console port and exports none), and when credentials are set `devy check` SHALL warn (without counting an issue) that they are written in plaintext to `.shadowenv.d`. When no credentials are set, `devy check` and `devy up` SHALL warn that MinIO will use its default `minioadmin` credentials. `console_port` SHALL be validated like other port keys. Under the brew and apt backends, devy SHALL refuse to run a `minio` dependency that is not docker-managed, because the system service listens on every interface with the default `minioadmin` login and ignores `access_key`, `secret_key`, `port` and `console_port`: `devy check` SHALL report an issue, `devy up` SHALL fail before installing or starting anything, and `devy service start`/`restart` SHALL fail without invoking the service manager. The message SHALL name the backend and tell the user to set `docker: true` or use the nix backend.

#### Scenario: Console port
- **WHEN** `minio` is declared with `console_port: 9001`
- **THEN** devy exports `MINIO_CONSOLE_ADDRESS=127.0.0.1:9001`

#### Scenario: MinIO auto-picked console port exported
- **WHEN** `minio` is declared without `console_port` and the backend is nix
- **THEN** devy exports `MINIO_CONSOLE_ADDRESS=127.0.0.1:<p>`, where `<p>` is the port the nix launch passes to `--console-address`

#### Scenario: Default credentials warning
- **WHEN** `minio` is declared without `access_key` and `secret_key`
- **THEN** `devy check` warns that the default `minioadmin` credentials are in use

#### Scenario: Homebrew MinIO refused by check
- **WHEN** `devy.yml` sets `package_manager: brew` and declares `minio` without `docker: true`
- **THEN** `devy check` reports an issue for `minio` saying the Homebrew service would listen on every interface with the default `minioadmin` login, suggesting `docker: true` or the nix backend, and exits non-zero

#### Scenario: Homebrew MinIO refused by up before install
- **WHEN** `devy up` runs with `package_manager: brew` and a non-docker `minio` that sets `access_key` and `secret_key`
- **THEN** `devy up` fails during config validation and neither installs nor starts MinIO through Homebrew

#### Scenario: Service start refused under apt
- **WHEN** `devy service start minio` runs under the apt backend for a non-docker `minio`
- **THEN** the command fails with the same message and `systemctl start` is not run

#### Scenario: Docker-managed MinIO under brew still allowed
- **WHEN** `devy.yml` sets `package_manager: brew` and declares `minio` with `docker: true`
- **THEN** `devy check` reports no MinIO backend issue and the container is published on `127.0.0.1` with the configured credentials

### Requirement: Loopback listeners under Homebrew and apt
Under the brew and apt backends, `devy up` SHALL keep Kafka, the ZooKeeper it starts for Kafka without KRaft, and RabbitMQ on loopback, following the MySQL `my.cnf.d/devy.cnf` rule: devy writes only files it owns (first line `# devy-managed`) with `fs_safe::write_atomic`, never silently rewrites a config it does not own, and instead warns once per distinct content of that file, naming the lines to add. A rewrite SHALL keep the existing file's permission bits. A write that fails (for example `/etc` is not writable without root, or the file is a symlink) SHALL be a warning, shown once, naming only the lines to add (never the file's other content), not a failed `up`. Docker-managed services skip this, and nix launches with its own loopback configs. MinIO is not reconfigured here: no file it reads sets its listen address or credentials, so devy refuses to run it under these backends (see the MinIO requirement).
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

#### Scenario: MinIO under Homebrew is refused, not reconfigured
- **WHEN** `devy up` runs under brew with a non-docker `minio`
- **THEN** devy writes no MinIO config file under `$(brew --prefix)/etc` and fails with the MinIO backend message instead
