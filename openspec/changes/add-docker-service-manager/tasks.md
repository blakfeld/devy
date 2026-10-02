# Tasks

Prerequisite: `fix-baseline-spec-gaps` is implemented (shared port resolver, `port_applicable`, `start_service` with launch data).

## 1. Configuration

- [ ] 1.1 Add `ServiceManagerChoice` (`package`/`docker`) and `ContainerCli` (`docker`/`podman`) enums, plus the top-level `service_manager` and `container_cli` fields on `DevyConfig`, all lowercase and defaulted (`src/config.rs`). Verify with unit tests: defaults, valid values, and parse errors for `service_manager: kubernetes`.
- [ ] 1.2 Add typed per-dependency `service_manager` and `image` to `DepConfig`/`Dependency` so they never land in `extra`. Verify with a unit test that `- redis: { service_manager: docker, image: mirror/redis }` parses and that `check` reports no unrecognized keys for them.
- [ ] 1.3 Add shared validation that rejects `service_manager`/`image` on non-built-in-service dependencies, with the message `<dep>: service_manager and image apply only to built-in services`, used by both `up` and `check`. Verify with unit tests for `node` and for a generic `foo`.
- [ ] 1.4 Document `service_manager`, `container_cli` and the per-dependency `service_manager`/`image` in the README `devy.yml` reference, including a "nix-free" example (`package_manager: brew` + `service_manager: docker`). Verify the example parses, via a unit test that loads it.

## 2. Container runtime layer

- [ ] 2.1 Add FNV-1a 64-bit hashing and a project-slug helper: name sanitized to `[a-z0-9-]` plus 8 hex characters of the root-path hash (`src/modules/helpers.rs`). Verify with unit tests: fixed known hashes, sanitization, and distinct slugs for the same name at different roots.
- [ ] 2.2 Create `src/service_runner/docker.rs` with a `CommandRunner` abstraction and a `ContainerRuntime` providing:
  - availability (`info`)
  - `image inspect`, `pull` and digest lookup
  - `container inspect` returning state and labels
  - run, start, stop, `rm -f` and `volume rm`

  Verify with unit tests on a fake `CommandRunner` that assert the exact argv for both `docker` and `podman`, and that parse sample `inspect` JSON (running, stopped, missing).
- [ ] 2.3 Implement the availability error `<cli> is not available — install it or start its daemon, or set service_manager: package`. Verify with a unit test using a fake runner where `info` fails.

## 3. Module container specs

- [ ] 3.1 Add `DockerSpec` and `Module::docker_spec(dep)`, returning `None` by default, with image/tag resolution: `version`, the `image` override, and a `:tag` in `image` winning over `version` with a warning. Verify with unit tests covering `redis:7.2` and `registry.corp.example/mirror/redis:7`, plus a tag-in-image case.
- [ ] 3.2 Implement `docker_spec` for postgresql, mysql (reusing the `cli_args` sanitizer) and mariadb. Verify with unit tests on env (trust/empty-password), container port, data path and args.
- [ ] 3.3 Implement `docker_spec` for redis, mongodb, rabbitmq, memcached, nginx, mailhog and meilisearch (`MEILI_MASTER_KEY`). Verify with per-module unit tests.
- [ ] 3.4 Implement `docker_spec` for elasticsearch (single-node, security off, heap), opensearch (single-node, security plugin disabled), minio (`server /data`, console port, credentials) and vault (dev mode vs `VAULT_LOCAL_CONFIG`). Verify with per-module unit tests.
- [ ] 3.5 Implement `docker_spec` for kafka in KRaft mode with an advertised listener on the host port, plus the zookeeper-mode warning when `kraft` isn't `true`. Verify with unit tests on env and on the warning text.
- [ ] 3.6 Verify with a unit test that every built-in service returns `Some(docker_spec)` and every non-service and the generic module return `None`.

## 4. Service runner and lifecycle

- [ ] 4.1 Introduce the `ServiceRunner` trait, a `PackageRunner` wrapping existing module/PM behavior, and `runner_for(dep, config)` with per-dependency override precedence. Verify that existing `up`/`down`/`service` tests pass unchanged through `PackageRunner`, and that new tests cover the top-level docker, per-dependency opt-in and per-dependency opt-out selections.
- [ ] 4.2 Implement `DockerRunner`:
  - install means image present or pull
  - resolved means tag plus digest
  - `start` creates the container with name, labels, `127.0.0.1` port publishing, named volume, env and args; it reuses the container when the `sh.devy.config` hash matches, and otherwise does `rm -f` and recreates it
  - `stop`
  - `remove(volumes)`

  Verify with fake-runner unit tests for create, reuse, recreate-on-port-change (volume kept) and pull failure (`Failed to pull <ref>`).
- [ ] 4.3 Mark docker-managed services as always port-applicable in the shared resolver, and use the host port for env vars and health checks. Verify with unit tests where brew + docker redis gets a random port and docker-managed mysql + mariadb don't conflict.
- [ ] 4.4 Skip package-manager service-config `post_setup` (conf.d writers) for docker-managed services. Verify with a unit test that no conf.d write is attempted for docker-managed postgres on brew.

## 5. Commands

- [ ] 5.1 `up`:
  - Route install, start and health through runners.
  - Print `○ <dep> image present (docker)`, `→ Pulling <ref>` and `✓ Pulled <ref>`.
  - Ensure package-manager availability only when `needs_pm()`, and suppress the `auto` warning otherwise.
  - Ensure the container runtime is available when any dependency is docker-managed.

  Verify with unit tests for a docker-only project (no PM check) and a mixed project.
- [ ] 5.2 Lock: add an optional `image_digest`, record `source: docker`, set `resolved_version` to the tag, use the digest reference when unchanged and not `--update`, and re-resolve when the tag or repository changes or `--update` is set (`src/lock.rs`, `up.rs`). Verify with unit tests: digest round-trip, old locks still parse, unchanged package entries keep the write skipped, and the teammate-digest and `--update` cases.
- [ ] 5.3 Add `down --volumes`: stop docker containers, and with the flag remove each container and its volume, printing `✓ <dep> container and volume removed`. Package-managed services are unaffected (`src/cli.rs`, `down.rs`). Verify with unit tests for plain down (volume kept) and `--volumes`.
- [ ] 5.4 Make `services`, `start`, `stop`, `restart`, `check` and `status` use runners. Docker services are labeled `(docker)` in `services` and `status`; a missing image counts as not installed and a missing or stopped container as stopped. Verify with unit tests on table output and issue counts.
- [ ] 5.5 Add `--volumes` after `down` in the zsh, bash and fish completion templates (`src/commands/hook.rs`). Verify with a unit test that asserts each snippet contains `--volumes`.
- [ ] 5.6 Add a README section, "Running services with Docker or Podman", covering:
  - naming and volumes
  - `down --volumes`
  - digest pinning
  - registry mirrors via `image`
  - Kafka KRaft-only
  - mailhog on Apple Silicon
  - switching from package-managed services means fresh data

  Verify that the README commands match the CLI help output.

## 6. Integration checks

- [ ] 6.1 Add an `integration.yml` job on `ubuntu-latest`, where Docker is preinstalled. It writes a `devy.yml` with `package_manager: apt`, `service_manager: docker`, `redis` and `postgresql`, runs `devy up`, and asserts:
  - `redis-cli -p $REDIS_PORT ping` (or `docker exec`) succeeds
  - `pg_isready -h 127.0.0.1 -p $POSTGRESQL_PORT` succeeds
  - `devy check` passes
  - `devy down --volumes` removes the containers and volumes

  Verify that the workflow passes via manual dispatch.
- [ ] 6.2 Run `cargo test`, `cargo clippy -- -D warnings` and `cargo fmt --check`. Verify all pass.
- [ ] 6.3 Run `openspec validate add-docker-service-manager --strict`. Verify the change is valid.
- [ ] 6.4 Manually smoke-test on macOS with OrbStack or Docker Desktop, and with Podman, using a docker-only project (redis, postgresql, kafka) and no Nix installed. Verify that `devy up`, `devy restart redis` (same port) and `devy down --volumes` work.
