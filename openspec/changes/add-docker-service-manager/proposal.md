# Proposal

## Why

Many workplaces forbid Nix, and others won't let developers run databases and brokers through Homebrew or system packages, but they do allow Docker or Podman. Today, devy services only run through the package manager (nix launchd/systemd units, `brew services`, systemd, `net start`). Those teams can't use devy's service features at all. A container backend gives them per-project, isolated services that honor devy's port assignment, without touching the package manager.

## What Changes

- **Service manager setting.** A new top-level `service_manager: package | docker` key (default `package`, which is today's behavior). Each service dependency can override it with a per-dependency `service_manager` key, e.g. `- redis: { service_manager: docker }`.
- **Container CLI choice.** A new top-level `container_cli: docker | podman` key (default `docker`). Podman is driven through its Docker-compatible CLI.
- **Docker-managed services skip the package manager.**
  - "Installed" means the image is present locally, and installing means `pull`.
  - Starting runs a per-project container named `devy-<project>-<service>`. It publishes the resolved port on `127.0.0.1` and stores data in a named volume, `devy-<project>-<service>`.
  - Containers are recreated when their image, port or settings change. Data in volumes is kept.
- **Image selection.** Each built-in service declares an official image, a container port, a data path and the environment it needs, e.g. passwordless local auth so existing `DATABASE_URL` values keep working. `version` selects the image tag. A per-dependency `image` key overrides the image, for corporate registry mirrors.
- **Pinned image digests.** `devy.lock` records the docker source and the pulled image digest, so teammates pull the identical image. `--update` re-resolves the tag. The lock file format stays at version 1, with one new optional field.
- **Ports are always applicable under docker**, so random port assignment and `devy.lock` port reuse work for every service.
- **Package manager checked only when needed.** `devy up` requires the package manager only when some dependency actually uses it, so a project whose only dependencies are docker services doesn't need Nix, brew or apt. Docker availability (CLI present, daemon reachable) is checked instead, and `--bootstrap` never installs Docker.
- **`devy down --volumes`** also removes docker-managed containers and volumes. Plain `devy down` stops containers and keeps their data.
- **`devy services` and `devy status`** label docker-managed services, and completions include `down --volumes`.
- **Kafka under docker always runs in KRaft mode.** Zookeeper mode is not supported in containers, and `kraft: false` produces a warning.

Depends on `fix-baseline-spec-gaps` (shared port resolution and backend port applicability) and should be implemented after it.

## Capabilities

### New Capabilities
- `docker-services`: running service dependencies as containers. Covers runtime detection, per-module image specs, container and volume naming, lifecycle, recreation, image pinning and teardown.

### Modified Capabilities
- `project-config`: adds the new top-level keys `service_manager` and `container_cli`, and the per-dependency keys `service_manager` and `image`.
- `environment-up`: package manager availability is checked only when used; docker services are installed by pulling.
- `service-management`: listing labels the backend, and `down` gains `--volumes`.
- `lock-file`: adds the `image_digest` entry field and digest pinning for docker services.
- `service-ports`: ports are always applicable for docker-managed services.
- `shell-integration`: completion for `down --volumes`.

## Impact

- **Code:**
  - `src/config.rs`: new keys and validation.
  - A new `src/service_manager/` (or `src/container/`) module containing the docker runtime.
  - `src/modules/mod.rs`: a new `docker_spec` hook, implemented in each service module.
  - `src/commands/up.rs`, `down.rs`, `service.rs`, `check.rs`, `status.rs`: dispatch per service manager.
  - `src/lock.rs`: the `image_digest` field.
  - `src/cli.rs`: `down --volumes`.
  - `src/commands/hook.rs` and `README.md`.
- **External tools:** needs a `docker`- or `podman`-compatible CLI at runtime, only when a docker service is declared. No new crates; the CLI's JSON output is parsed with `serde_json`.
- **Compatibility:** opt-in only. Existing projects behave as before, and lock files without `image_digest` stay valid.
