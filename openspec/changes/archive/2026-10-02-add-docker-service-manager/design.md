# Design

## Context

See proposal.md (Why). This builds on `fix-baseline-spec-gaps`, which introduces:
- a shared port resolver with backend applicability (`Module::port_applicable`)
- `Module::nix_launch` / `LaunchSpec`
- a `PackageManager::start_service(name, launch, root)` that carries per-service launch data

Relevant current structure:
- **Service lifecycle.** `Module::{is_running,start,stop}` call `pm.*_service(service_name)`. `up.rs`, `down.rs`, `service.rs`, `check.rs` and `status.rs` call these methods directly on the module with a `&dyn PackageManager`.
- **Install.** `Module::{is_installed,install,resolved_version}` delegate to the PM through `pm_dep`.
- **Package manager availability.** `up.rs` calls `pm.ensure_available(bootstrap)` unconditionally before loading the lock, and `detect()` runs in every command.
- **Config.** `DepConfig` has typed fields `version/tap/after_install/shell`, and everything else is flattened into `extra`. `DevyConfig` uses `deny_unknown_fields`.
- **Lock.** `LockedDep { resolved_version, source, assigned_port }` in YAML. Version 1 is enforced.

## Goals / Non-Goals

**Goals:**
- Run any built-in service as a per-project container, with ports, data and env vars consistent with what devy exports.
- A project whose only dependencies are docker services must never require Nix, brew or apt.
- Reproducible images via lock-file digests.
- Work with Docker Desktop, OrbStack, Colima, Linux dockerd and Podman (rootful or rootless) through the Docker-compatible CLI.

**Non-Goals:**
- **Compose files.** devy doesn't generate or consume `docker-compose.yml`. A `devy export --format compose` is a possible follow-up.
- **Containerized language runtimes.** node, python and the like stay with the package manager.
- **Arbitrary user-defined containers.** Generic dependencies can't use docker; the `image` override only applies to built-in services.
- **Installing Docker or Podman**, including through `--bootstrap`.
- **Remote or non-local Docker hosts.** Health checks assume published ports reach `127.0.0.1`. `DOCKER_HOST` pointing at a remote daemon is unsupported.

## Decisions

### D1. A `ServiceRunner` seam between commands and backends
Introduce `src/service_runner/` with a `ServiceRunner` trait that has two implementations:
- **`PackageRunner`:** wraps today's module + PM calls, including `nix_launch` from the prior change.
- **`DockerRunner`:** wraps the container CLI.

The trait's operations:
- `is_installed`
- `install`
- `resolved` (version, digest)
- `is_running`
- `start`
- `stop`
- `remove(volumes)`
- `label()`

`runner_for(dep, config)` picks the implementation from the per-dependency `service_manager`, falling back to the top-level one; non-services always get `PackageRunner`. `up`, `down`, `service`, `check` and `status` iterate dependencies through `runner_for` instead of calling module methods directly.

*Alternatives considered:*
- Implement docker as a fifth `PackageManager`. Rejected: the PM is project-wide, but docker must coexist with brew/apt for non-service dependencies in the same project.
- Branch inside each module. Rejected, because it would duplicate the logic across 15 modules.

### D2. Modules describe containers declaratively
Add `fn docker_spec(&self, dep: &Dependency) -> Option<DockerSpec>` to `Module`, returning None by default. `DockerSpec` holds:
- the image repository
- the default tag
- the container port
- extra published ports, as (host, container)
- the data mount path (`Option`)
- env pairs
- command args

Modules build the spec from the already-resolved `dep.extra`, the same values their `env_vars` use, which keeps container settings and exported variables in sync. The per-dependency `image` field replaces the repository, and a `:tag` inside `image` wins over `version`.

Notable specs:
- **mysql/mariadb:** reuse the `write_mysql_config` sanitizer to turn `cli_args` into `--key=value` server args.
- **elasticsearch:** adds `ES_JAVA_OPTS=-Xms512m -Xmx512m`.
- **vault without dev mode:** uses `VAULT_LOCAL_CONFIG` with file storage at `/vault/file`, a `tls_disable` listener, and `disable_mlock: true`, running `server`.
- **kafka:** uses `apache/kafka` KRaft env:
  - `KAFKA_NODE_ID=1`
  - `KAFKA_PROCESS_ROLES=broker,controller`
  - `KAFKA_LISTENERS=PLAINTEXT://:9092,CONTROLLER://:9093`
  - `KAFKA_ADVERTISED_LISTENERS=PLAINTEXT://127.0.0.1:<host port>`
  - `KAFKA_CONTROLLER_QUORUM_VOTERS=1@localhost:9093`
  - `KAFKA_CONTROLLER_LISTENER_NAMES=CONTROLLER`
  - offsets replication factor 1

### D3. Shell out to the CLI, parse JSON
Use `std::process::Command` against `docker` or `podman`; no Docker API crate, which keeps Podman support free.

| Operation | Command |
|---|---|
| Runtime availability | `<cli> info --format '{{json .ServerVersion}}'` |
| Image present | `<cli> image inspect <ref>` |
| Pull | `<cli> pull <ref>` |
| Image digest | `<cli> image inspect --format '{{json .RepoDigests}}' <ref>`, taking the first entry matching the repository |
| Container state | `<cli> container inspect --format '{{json .}}' <name>`, using `.State.Running` and `.Config.Labels` (a missing container is a distinct error) |
| Create | `<cli> run -d --name <n> --label … -p 127.0.0.1:<h>:<c> -v <vol>:<path> -e K=V <ref> <args…>` |
| Start / stop | `<cli> start` / `<cli> stop` |
| Remove | `<cli> rm -f`, then `<cli> volume rm` |

Commands go through a small `CommandRunner` trait, so unit tests can assert the exact argv without needing a daemon.

### D4. Stable hashes without new crates
The project-name suffix and the `sh.devy.config` label both need hashes that are stable across Rust versions. `std`'s `DefaultHasher` doesn't guarantee that, so use a ~10-line FNV-1a 64-bit implementation in `helpers.rs`. The config hash covers:
- the image reference
- the sorted env
- the args
- all port mappings
- the volume mount

### D5. Lock file stays v1, with an optional `image_digest`
Add `image_digest: Option<String>` to `LockedDep` with `skip_serializing_if = "Option::is_none"`. Old locks parse unchanged, and package-managed entries are byte-for-byte unchanged, so the write is still skipped when nothing changed.

Choosing the image reference:
- If the lock has a digest, the repository and tag are unchanged, and `--update` isn't set, use `<repo>@sha256:…`.
- Otherwise use `<repo>:<tag>`, and record the digest after pulling.

`source` is `docker`.

### D6. Lazy package-manager checks
`detect()` still runs, because the config must be valid. `ensure_available` moves behind a `needs_pm()` check, which is true if any dependency uses `PackageRunner` or shadowenv isn't on PATH and env vars must be written. `container_runtime.ensure_available()` runs when any dependency uses `DockerRunner`.

One consequence: the `auto` → nix warning still prints for docker-only projects. To avoid that, the warning is suppressed when `needs_pm()` is false, so users banned from nix aren't nagged.

### D7. Teardown
`devy down` calls `runner.stop()` for every service. `devy down --volumes` (a new clap flag) additionally calls `remove(volumes: true)` on docker runners only. `check` and `status` count a missing image as "not installed", and a stopped container, or none at all, as "stopped".

### D8. Config surface
- `DevyConfig` gains `service_manager: ServiceManagerChoice` (default `Package`) and `container_cli: ContainerCli` (default `Docker`).
- `DepConfig` gains typed `service_manager: Option<ServiceManagerChoice>` and `image: Option<String>`. Typing them keeps them out of `extra`, so the allowlist check doesn't flag them.
- Validation of the not-a-built-in-service rule happens in the shared dependency validation step that both `up` and `check` already run.

## Risks / Trade-offs

- [**`mailhog/mailhog` is amd64-only.**] It runs under emulation on Apple Silicon, which is slow but works. → Document it. A user can point `image` at a multi-arch fork. Switching the default to Mailpit is a possible follow-up.
- [**Rootless Podman can't publish ports below 1024.**] → Every container port mapping uses a host port ≥ 1024 except an explicit nginx `port: 80`. devy warns in that case, just as it does for nginx today.
- [**Floating default tags.**] Defaults like `minio/minio:latest` and `mailhog/mailhog:latest` move. → The lock-file digest pins them per project. `--update` is the explicit refresh.
- [**`docker run` failures after `docker rm -f`**] (e.g. the port is in use) leave no container behind. → Recreate fails with `Failed to start <dep> service: <cli stderr>`, and the volume is untouched, so a retry is safe.
- [**Docker Desktop not running**] is the most common failure. → Run the availability check before any pull or start, with an actionable message.
- [**Behavior differs from package-managed databases**] (e.g. trust auth, empty root password). → This is acceptable for local development and matches devy's current `DATABASE_URL`s. The spec documents it.

## Migration Plan

- The feature is opt-in. Existing projects see no change, and lock files gain fields only for docker-managed services.
- A team that switches from package-managed to docker services gets fresh, empty databases in new volumes. Their old data stays in the package-managed service, and the README will say so.
- **Rollback:** older devy binaries still parse a lock containing `image_digest`, since the lock structs (`LockedDep`) don't use `deny_unknown_fields`. They do reject the new `service_manager`/`container_cli` keys in `devy.yml`, which is expected for a new config feature.

## Open Questions

- Exact default tags (e.g. `elasticsearch:8.13.4`, `apache/kafka:3.7.0`, `meilisearch:v1.8`) can be bumped to current releases at implementation time without changing behavior.
