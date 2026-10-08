# Proposal

## Why

Under the brew and apt backends, devy starts MinIO through the system service manager, so devy's loopback address and credentials never reach the server. Homebrew's service runs `minio server --certs-dir=<etc>/minio/certs --address=:9000 <var>/minio`. It has no `environment_variables`, so it listens on every interface with the built-in `minioadmin`/`minioadmin` login, and its console listens on a random port, also on every interface. When the user sets `access_key`/`secret_key`, devy exports them as `MINIO_ROOT_*` to the shell only. Clients then get access-denied, and the user believes the server is protected while it is open to the LAN with default credentials.

Credentials reach MinIO only through `LaunchSpec.env` (nix) and `secret_env` (docker). MinIO reads its listen address and root credentials only from command-line flags and the process environment (or from a file named by `MINIO_CONFIG_ENV_FILE`, which is itself an environment variable). The Homebrew service exposes neither, and `brew services` regenerates its plist on every start. So the file-based approach that `loopback.rs` uses for Kafka, ZooKeeper and RabbitMQ cannot secure it. Debian and Ubuntu ship no `minio` package, so the apt path only works with a third-party package whose unit devy does not control.

## What Changes

- **BREAKING**: Under the brew and apt backends, devy refuses to run a non-docker `minio` dependency. `devy check` reports an issue, and `devy up` fails during config validation, before anything is installed or started. The message says that the system service would listen on every interface with the default `minioadmin` login and ignore `access_key`/`secret_key`/`port`/`console_port`. It tells the user to set `docker: true` or use the nix backend.
- `devy service start` / `restart` for MinIO under brew or apt fail with the same message instead of running `brew services start minio` / `systemctl start minio`. This is defence in depth, for commands that skip `up`'s validation.
- nix, docker and winget behaviour is unchanged. Nothing is added to `loopback.rs`, because there is no MinIO config file that devy could own (see design.md).

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `service-modules`: The MinIO requirement gains a rule that devy refuses to run MinIO through the brew or apt service manager. The loopback requirement for Homebrew and apt names MinIO as a service that devy refuses instead of reconfiguring.

## Impact

- `src/modules/minio.rs`: backend-aware refusal in a new hook and in `start`, plus unit tests.
- `src/modules/mod.rs`: a new `Module` hook for backend-aware config issues, empty by default.
- `src/commands/check.rs`, `src/commands/up.rs`: call the new hook for non-docker deps.
- `tests/cli.rs`: a CLI test for `devy check` with `package_manager: brew` and `minio`.
- `README.md`: a note on the `minio` row that brew/apt need `docker: true` or nix.
- Users who run MinIO via Homebrew must switch to `docker: true` or nix.
