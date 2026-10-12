# Proposal

## Why

Docker-managed services are never recreated when their configuration changes. The config-hash comparison (`sh.devy.config` label) and the recreate logic live in the docker runner's start step, but `devy up` and `devy start` only reach that step when the service is *not* running. A running container counts as running whatever its hash, so changing `port`, `version` or `image` in `devy.yml`, or running `devy up --update`, leaves the old container running. Meanwhile the exported environment (`REDIS_PORT`, `DATABASE_URL`, …) and `devy.lock` move to the new values, so the project points at a port or image that isn't there. The README ("Changing a port, image or setting recreates the container on the next `devy up`") and the docker-services spec's "Port change recreates the container" scenario promise the opposite. Today only `devy restart` or `devy down` + `devy up` apply the change.

## What Changes

- `devy up` and `devy start <name>` treat a running docker-managed container whose `sh.devy.config` hash differs from the current configuration (or that lacks the label) as needing a start, so the existing recreate path removes it and creates a new one, keeping the data volume.
- In that case both commands print `Recreating <name> container (configuration changed)` instead of `○ … already running`.
- A running container whose configuration matches is still left alone and reported as already running.
- Commands that only report or stop services (`devy services`, `devy stop`, `devy down`) keep today's notion of "running" and do not compare configuration.
- No change for package-managed services.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `docker-services`: the "Container lifecycle" requirement now states that a *running* container with a different configuration is recreated by `devy up` and `devy start`, not only a stopped one.

## Impact

- `src/service_runner/mod.rs`: a new `ServiceRunner` hook (default: configuration is current), implemented by `DockerRunner` by comparing the inspected `sh.devy.config` label with the hash of `run_spec`.
- `src/commands/up.rs` (`start_service_if_needed`) and `src/commands/service.rs` (`start_impl`): consult the hook before skipping a running service.
- Tests in `src/service_runner/tests.rs`, `src/commands/up.rs` and `src/commands/service.rs`.
- README's docker section already documents the intended behavior; it only gains the new message.
