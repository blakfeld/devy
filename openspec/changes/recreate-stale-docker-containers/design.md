# Design

## Context

See proposal.md (Why). The relevant code today:
- `DockerRunner::start` (`src/service_runner/mod.rs`) already does the right thing for every container state: same `sh.devy.config` hash and running → no-op; same hash and stopped → `<cli> start`; different or missing hash → `<cli> rm -f` then `<cli> run`, keeping the named volume; foreign container → refused.
- `start_service_if_needed` (`src/commands/up.rs`) and `service::start_impl` (`src/commands/service.rs`) skip `start` when `runner.is_running(dep)? && !runner.needs_migration(dep)`.
- `DockerRunner::is_running` is true for any running container this project owns, whatever its hash, and `needs_migration` is always false for docker. So a running stale container never reaches `start`.
- `is_running` is also used by `devy services`, `stop`, `restart`, `down` and `wait_for_stopped`, where "running" must keep meaning "the container is up".
- `config_hash(&RunSpec)` is private to the module; the spec's label value is carried in `run.labels` under `CONFIG_LABEL`.

## Goals / Non-Goals

**Goals:**
- `devy up` and `devy start` recreate a running container whose configuration changed, through the existing recreate path in `DockerRunner::start`.
- Running containers with an unchanged configuration stay untouched (no extra `<cli>` calls beyond one inspect).
- Tests that fail on the current code at the command level (`start_service_if_needed`, `start_impl`), not only at `DockerRunner::start`.

**Non-Goals:**
- Changing what `is_running` means for listing, stopping or `down`.
- Showing "stale" in `devy services` output.
- Detecting configuration drift for package-managed services.

## Decisions

### D1. A separate read-only hook, `needs_recreate`, rather than changing `is_running`
Add `fn needs_recreate(&self, dep: &Dependency) -> Result<bool>` to `ServiceRunner`, defaulting to `Ok(false)`. `DockerRunner` implements it as: inspect the container; return `false` when it is missing, not running, or foreign (the `start` path or the foreign check handles those); otherwise build `run_spec(dep)` and return whether the container's `sh.devy.config` label differs from the spec's (a missing label differs). The hash comparison is factored into one private helper shared with `start`, so the two can't drift.
- **Alternative: make `DockerRunner::is_running` return false for stale containers.** Rejected: `stop`, `restart`, `down` and `wait_for_stopped` would then think a running stale container is stopped, skip stopping it, and `wait_for_stopped` would return early.
- **Alternative: for docker, always call `start` and let it no-op.** Rejected: the commands could no longer print `○ … already running` accurately without a second inspect, and it couples the commands to docker semantics.
- **Alternative: name it `is_current` (true = up to date).** Equivalent; `needs_recreate` mirrors the existing `needs_migration` and makes the default (`false`) the safe no-op.

### D2. Callers combine the checks in one place
Both `start_service_if_needed` and `start_impl` change their skip condition to `is_running && !needs_migration && !needs_recreate`. When `needs_recreate` is true they print `Recreating <name> container (configuration changed)` (an `output::step`) before calling `start`; otherwise the existing `Starting …` step. A small shared helper may compute the decision so the two commands can't diverge, but the messages stay per command.

### D3. `needs_recreate` errors propagate
If `run_spec` or inspect fails (e.g. the docker daemon is down), the error is returned with the service name in context, as `start` would have failed the same way. Swallowing it would hide a broken configuration behind "already running".

### D4. No change to `restart`
`restart` already stops and starts, which reaches the recreate path. It is left as is.

## Risks / Trade-offs

- [`devy up` now briefly takes down a running service when its config changed] → This is the documented behavior; the volume is kept and the readiness check runs afterwards. The `Recreating …` message makes the downtime visible.
- [Containers created by an older devy without `sh.devy.config` get recreated once on the next `up`] → Same as the existing `start` behavior for stopped containers; one-time and data-preserving.
- [An extra inspect and `run_spec` build per running docker service on `up`] → One local `<cli> inspect` per service; negligible next to the existing calls.
- [Hash includes resolved image reference, so `devy up --update` recreates every service whose digest moved] → Intended: that is what the lock and README promise.

## Open Questions

- Should `devy services` mark a running container whose configuration is stale (e.g. `● redis (docker, stale)`)? Deferred; it doesn't affect this fix and can be added later without changing the hook.
