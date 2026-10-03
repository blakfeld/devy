# Design

## Context

- **Where nix keeps service state.** Under nix, every built-in service keeps its state in `nix_data_dir(project_root, canonical) = .devy/data/<canonical>/` (`src/modules/mod.rs`). One-time initialization is guarded by an `InitStep.marker`, such as `PG_VERSION`.
- **Other backends.** Brew, apt and WinGet run services from shared system directories, such as `/opt/homebrew/var/postgresql@16` or `/var/lib/postgresql`, which several projects may share.
- **`devy up`'s service phase.** Services start in `start_service_if_needed` (`src/commands/up.rs`). Health-check timeouts there are warnings, not errors.
- **Concurrency guard.** `up` serializes on `.devy-lock` via `fs2`.
- **`LaunchSpec.seed_dirs` is unrelated.** It already exists and is unrelated to data seeding: it copies a package's config directory. The new feature uses "seed" only in the user-facing `seed:` key and a new module hook. Code names avoid `seed_dir` to prevent confusion.
- **Parallel changes touch the same requirements.** `add-docker-service-manager` and the sibling changes (`add-ai-init`, logs/doctor, MCP) also modify `cli` "Built-in subcommands" and `shell-integration` "Tab completion".

## Goals / Non-Goals

**Goals:**
- Snapshots that can't silently lose data. That means confirmation, rollback on failure, and a version guard.
- Seeds that run exactly once per data lifetime and behave the same on every backend that can reach the service over TCP.
- No new runtime requirements for users who don't use the feature.

**Non-Goals:**
- Snapshotting brew, apt or WinGet data. Logical dumps (`pg_dump`, `mysqldump`) could make this possible later, but they behave differently per engine and are out of scope.
- Hot or online snapshots. Services are stopped for consistency.
- Sharing snapshots between machines or committing them.
- Automatically re-running changed seeds.
- Seeding non-database services from files.

## Decisions

### D1: File-level archives of stopped data dirs, not logical dumps
Copying the stopped data directory works for every nix service the same way, whether it's postgres, mysql, redis, kafka, elasticsearch or minio, and it restores the exact state, including configuration under `.devy/data/<svc>/config`.
- **Rejected:** logical dumps, which need engine-specific tools and only cover databases.
- **Cost:** the data stays tied to the engine's major version, which D5 handles.

### D2: Pure-Rust `tar` + `flate2` (miniz_oxide backend) rather than shelling out to `tar`
- macOS ships bsdtar and Linux ships GNU tar, and their flags and handling of special files differ.
- devy must skip Unix sockets and pid files, which is easy while walking the tree ourselves.
- The format stays testable in unit tests with temp dirs.
- `flate2`'s default `miniz_oxide` backend adds no C toolchain requirement, so CI on all four targets stays simple.
- **Rejected:** `zstd`, which compresses better but needs a C build, and plain `.tar`, which is too large for multi-GB search indexes.
- **Implementation details:**
  - Archives store paths relative to the data dir.
  - Extraction rejects absolute paths and `..` components. The `tar` crate's `unpack_in` already does this.
  - Extraction keeps file permissions. Postgres refuses a data dir that isn't mode 0700.

### D3: Restore by rename-aside, then extract
For each service, devy works in this order:
1. Rename `.devy/data/<svc>` to `.devy/data/.<svc>.restore-old`.
2. Extract the archive into a fresh `.devy/data/<svc>.restoring`.
3. Rename that into place.
4. Delete the old copy.

Every step before the final delete can be undone. If extraction fails, devy deletes `.restoring` and renames `.restore-old` back. Leftover `.restore-old` or `.restoring` directories from a crash are detected on the next restore or save, and devy prints a warning naming them instead of deleting them silently.

Saving uses the same staging idea. It writes to `.devy/snapshots/.<name>.saving/`, then renames that to `<name>/`, swapping out the old snapshot when `--force` is given.

### D4: Stop, then archive, then restart only what was running
This reuses `Module::stop` / `wait_for_stopped` and `start` / `wait_for_ready`, the same paths that `devy stop` and `devy start` use (`service.rs`). Restart happens in a guard path so that a failed archive still brings services back up. Services are processed one at a time to keep downtime per service short.

### D5: Version guard on the major version
The manifest records each service's resolved version from `devy.lock`. Restore compares the text before the first `.`.

This is a heuristic, not a full compatibility table. Postgres and MongoDB data really is major-version-bound, while redis RDB files are usually forward-compatible. It is still worth having, because the guard only costs a `--force` to bypass, while a mistake costs a corrupted or unstartable data dir.

### D6: The seed record lives with the data
- **nix:** `.devy/data/<svc>/.devy-seed.json` holds the fingerprint and a timestamp. Wiping the data dir or restoring a snapshot then automatically gives the right "seeded?" answer. A snapshot taken after seeding restores as seeded.
- **Other backends:** devy can't observe the shared data directory, so the record goes in `.devy/state/seeds/<svc>.json`. This is documented as best-effort: if the user drops the brew database by hand, they run `devy seed --force`.
- **Rejected:** `devy.lock`. The lock is committed and shared, but seeded state is per-machine.
- **Fingerprint:** FNV-1a (the existing `modules::fnv1a`) over the canonical seed definition plus each file's bytes. It only detects changes and isn't a security boundary.

### D7: A new `Module` hook for file seeds
Add `fn seed_loader(&self, dep, file) -> Option<Result<SeedInvocation>>`, which returns the client program, args and stdin source, or `None` when the module only supports commands. Database modules implement it:
- **postgres:** `psql -X -v ON_ERROR_STOP=1 -h 127.0.0.1 -p <port> -d postgres -f <file>`
- **mysql / mariadb:** `mysql -h 127.0.0.1 -P <port> -u root`, with the file on stdin
- **redis:** `redis-cli -h 127.0.0.1 -p <port>`, with the file on stdin
- **mongodb:** `mongosh --quiet mongodb://127.0.0.1:<port> <file>`

Programs are resolved through the same PATH devy uses for setup tools (dependency-modules "Setup tools run from devy's own PATH"), so the nix profile's `psql` wins.

Arguments are passed as argv, never through `sh -c`, which avoids the injection class fixed in `python.rs`. Only the command form uses a shell. It reuses `exec::spawn_cmd`, which gets an extra-env parameter (existing callers pass none), so the resolved env is layered over the process env.

`known_extra_keys` gains `seed` for every module where `is_service()` is true. This is done centrally (the allowlist check in `check.rs` adds `seed` for services) rather than by editing 20 module allowlists by hand.

### D8: Where seeding happens in `up`
Seeding goes inside `start_service_if_needed`, after `wait_for_ready` succeeds. Services seed in declaration order, so a later service's command seed can depend on an earlier database. The merged env is already computed before phase 2. It is passed down so command seeds see `DATABASE_URL` and the other resolved values.

### D9: Locking and prompting
- `snapshot save`, `restore` and `delete`, plus `seed`, acquire `.devy-lock`. The guard code that `up::run` currently has inline is extracted into a shared helper in `shared.rs`, with the same error messages. `snapshot list` is read-only and takes no lock.
- The restore prompt uses `std::io::IsTerminal` on stdin, so no new crate is needed.

### D10: Docker volumes come in a later phase
Once `add-docker-service-manager` lands, eligibility extends to docker-managed services. Save and restore will run a throwaway container that mounts the named volume and the snapshot dir and archives `/data` with the same tar.gz layout, so manifests stay uniform.

This phase is planned as its own task group, gated on that change being archived. Until then, docker services are ineligible, with a message that says so.

### D11: AI generation is a separable final phase
`devy seed --generate` depends on `ai-assist`, which `add-ai-init` owns: the Anthropic client over `ureq`, `ANTHROPIC_API_KEY`, opt-in and redaction.
- **Schema only:** the schema comes from `pg_dump --schema-only` or `mysqldump --no-data`. No rows are ever read.
- **Never executed:** the output is written to a file and never run.
- **Separable:** if `add-ai-init` slips, every earlier phase ships without it.

## Risks / Trade-offs

- **Large data dirs make save and restore slow,** especially elasticsearch and kafka. → Print per-service progress and size, and let `--service` narrow the set. The design doesn't try to make archiving incremental.
- **Stopping services interrupts running apps.** → It's documented in help text, and services that were running are restarted automatically.
- **Major-version heuristic false positives or negatives.** → `--force` overrides, and the warning names both versions.
- **Seed state on shared backends is approximate.** → Documented, and `devy seed --force` is the escape hatch.
- **Seeds aren't idempotent.** → devy never re-runs one automatically. It only warns on drift.
- **Spec merge conflicts.** Several in-flight changes modify `cli` "Built-in subcommands" and `shell-integration` "Tab completion". → Whichever archives second must rebase its MODIFIED block onto the archived text. This is noted in tasks.
- **Disk usage under `.devy/snapshots`.** → `devy snapshot list` shows sizes, and `delete` exists. There is no automatic pruning.

## Migration Plan

The change is purely additive. Projects without `seed:` or snapshots see no behavior change. The only new dependencies are `tar` and `flate2`. To roll back, revert, then remove `.devy/snapshots/` and `.devy/state/seeds/` if wanted. The `.devy-seed.json` markers in data dirs are harmless leftovers.

## Open Questions

- Should `devy snapshot save` without a name default to a timestamp? This could be added later without changing the specified behavior.
- Should `devy status` show a seeded or not-seeded column? This is cosmetic and can be decided during implementation or later.
