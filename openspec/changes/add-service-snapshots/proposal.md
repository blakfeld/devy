# Proposal

## Why

Local databases drift. After a few migrations, a bad test run or a half-finished experiment, developers want to get back to a known-good state, and new teammates want a database that already has usable data in it. Today, devy creates an empty data directory and leaves the rest to the developer. Under nix, devy already owns each service's state in `.devy/data/<service>/`, so it can save and restore that state safely. Every backend can also load a seed file over the service's local port once the service is healthy.

## What Changes

- **`devy snapshot` subcommand group:**
  - `devy snapshot save <name> [--service <svc>]... [--force]` stops the selected services, archives each one's `.devy/data/<service>/` into `.devy/snapshots/<name>/`, and then restarts the services that were running before.
  - `devy snapshot restore <name> [--service <svc>]... [--yes] [--force]` replaces each service's data with the snapshot's copy. It asks for confirmation unless `--yes` is passed, refuses to run non-interactively without `--yes`, and rolls back if extraction fails. If the service's recorded major version differs from the installed one, it refuses unless `--force` is passed.
  - `devy snapshot list` shows each snapshot's name, creation time, services and size.
  - `devy snapshot delete <name>` removes a snapshot.
  - Snapshots live in `.devy/snapshots/`. On first save, devy writes a `.gitignore` there containing `*`, so snapshots are never committed by accident.
  - **Scope:** snapshots cover services whose data devy owns, which today means services run by the nix backend. Brew, apt and WinGet services keep their data in shared system directories that other projects may also use, so devy refuses to snapshot them and explains why. Docker-managed volumes are added in a later phase, once `add-docker-service-manager` lands.
- **Per-service `seed:` key.**
  - A service dependency can declare `seed:` as a file path, a list of file paths, or a command (`seed: { cmd: …, shell: … }`, the same shape as hooks).
  - `devy up` applies the seed once, after the service first passes its health check, and records that it did.
  - File seeds are loaded with the service's own client:
    - `.sql` for `postgresql` (via `psql`) and for `mysql` and `mariadb` (via `mysql`)
    - a file of redis commands for `redis` (via `redis-cli`)
    - `.js` for `mongodb` (via `mongosh`)
  - Every other service supports the command form only.
- **`devy seed [<service>]... [--force]`** applies pending seeds to running services. `--force` re-applies seeds that were already applied.
- **Seed drift warning.** When a seed's definition or file contents change after it was applied, `devy up` warns and suggests `devy seed <svc> --force`. It never re-runs the seed on its own, because seeds are rarely idempotent.
- **`devy check`** reports invalid `seed:` values, such as missing files or file types the module can't load, as issues.
- **Optional final phase: `devy seed --generate <service>`.** This reads the running database's schema only (never row data), asks Claude for realistic fake data through the shared `ai-assist` capability, and writes a `.sql` file for the user to review. It never executes the file.
- `snapshot` and `seed` become built-in subcommands, so they take precedence over project commands with the same names, and shell completion learns them.

## Capabilities

### New Capabilities
- `service-snapshots`: saving, listing, restoring and deleting point-in-time copies of devy-owned service data. Covers backend eligibility, safety (confirmation, rollback, version guard) and storage layout.
- `service-seeds`: the `seed:` key, the file types each service module can load, run-once tracking and drift warnings, the `devy seed` command, `devy check` validation, and AI seed generation (final phase, which depends on `ai-assist` from `add-ai-init`).

### Modified Capabilities
- `cli`: `snapshot` and `seed` are added to the built-in subcommands.
- `shell-integration`: completion covers `snapshot` (with its actions and flags) and `seed` (with `--force` and `--generate`).
- `environment-up`: the service start phase applies pending seeds after each service becomes ready, and warns about seed drift.

## Impact

- **New code:**
  - `src/commands/snapshot.rs` and `src/commands/seed.rs`
  - seed-loading support in the `postgres`, `mysql`, `mariadb`, `redis` and `mongodb` modules, with a new `Module` hook for loading a seed file
  - `seed` added to every service module's `known_extra_keys`
  - `src/cli.rs`
  - the completion scripts in `src/commands/hook.rs`
- **Changed code:** the service start phase in `src/commands/up.rs`, and `check.rs`, which gains seed validation.
- **Dependencies:** the `tar` and `flate2` crates (pure Rust, using the `miniz_oxide` backend).
- **Shared with in-flight work:**
  - `add-docker-service-manager`: the docker snapshot phase depends on it, and both changes modify `shell-integration`'s Tab completion requirement.
  - `add-ai-init`: owns `ai-assist`, which the `--generate` phase consumes.
- **Lock file:** `devy.lock` is unchanged. Seed state is machine-local and lives under `.devy/`.
