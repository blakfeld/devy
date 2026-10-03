# Proposal

## Why

Developers and coding agents increasingly work in several git worktrees of the same repo at once, but devy assumes one checkout per repo. Today a second worktree breaks the first one:

- **Ports collide.** `assigned_port` lives in the committed `devy.lock`, so every worktree of a branch inherits the same ports as the main checkout.
- **Nix services are hijacked.** Under nix, the launchd label (`sh.devy.<name>`) and systemd unit (`devy-<name>.service`) are global per user, not per project. Running `devy up` in a worktree on macOS rewrites and reloads the main checkout's `sh.devy.postgresql` agent so it points at the worktree's empty `.devy/data/postgresql/`. `devy down` in either checkout stops the other's services. The same happens between two unrelated projects that both declare `redis`.
- **Every worktree starts empty.** Each checkout has its own `.devy/data/`, so a new worktree gets a blank database and has to be migrated and seeded from scratch.

Docker-managed services already avoid the naming problem: containers and volumes are named `devy-<project>-<hash>-<service>` from the project root. They still share ports through `devy.lock`.

## What Changes

- **Per-project nix service names.** Under nix, the launchd label becomes `sh.devy.<project>.<name>`, the systemd unit becomes `devy-<project>-<name>.service`, and the macOS log becomes `$TMPDIR/devy-<project>-<name>.log`. `<project>` is the slug docker services already use: the sanitized project name plus 8 hex characters of a hash of the project root. This fixes collisions between unrelated projects as well as between worktrees.
  - **Migration:** when devy starts, stops or checks a nix service and finds a legacy `sh.devy.<name>` / `devy-<name>.service` unit that belongs to this project root, it stops and removes it. Units that belong to other projects are left alone.
- **Worktree detection.** devy recognizes a linked git worktree (`.git` is a file pointing into `<main>/.git/worktrees/`) and finds the main checkout. `devy status` shows `worktree of <main path>`.
- **Worktree-local ports.** In a linked worktree, port resolution reads and writes `.devy/worktree.yml` instead of `assigned_port` in `devy.lock`. `devy up` in a worktree keeps `devy.lock`'s existing `assigned_port` values unchanged, so worktree branches don't produce port churn in the committed lock. Explicit ports in `devy.yml` still win, and devy warns that they are shared with the main checkout.
- **`.devy/.gitignore`.** devy writes a `.devy/.gitignore` containing `*` the first time it creates `.devy/`, so `worktree.yml`, service data and snapshots are never committed.
- **`devy up --from [<worktree>]`.** Before starting services, devy copies each eligible service's data from another checkout. The default source is the main checkout; you can also name a worktree by path or by the branch checked out there. It reuses the archive and extract code from `add-service-snapshots`: the source service is stopped, archived, then restarted if it was running. Services that already have data in the target are skipped with a hint. Only nix-managed services are eligible, matching snapshot eligibility.
- **`devy prune [--yes]`.** Finds nix units and docker containers and volumes whose project root no longer exists, for example after `git worktree remove`, then lists them and removes them after confirmation.
- `prune` becomes a built-in subcommand, and shell completion covers `prune`, `--yes`, and `--from` after `up`.

## Capabilities

### New Capabilities
- `worktree-environments`: detecting linked worktrees and the main checkout, the worktree port file, `.devy/.gitignore`, `devy up --from`, and `devy prune`.

### Modified Capabilities
- `service-ports`: port precedence and persistence use `.devy/worktree.yml` in linked worktrees, and `devy.lock` ports are left untouched there.
- `service-management`: nix launchd labels and systemd unit names include the project slug, and legacy units are migrated.
- `service-logs`: nix log sources use the new label, unit and log file names.
- `environment-check`: `devy status` reports when it runs in a linked worktree.
- `cli`: `prune` is added to the built-in subcommands.
- `shell-integration`: completion covers `prune`, its flag, and `up --from`.

## Impact

- **Code:**
  - `src/package_manager/nix.rs`: label, unit and log naming, project root recorded in units, and legacy migration.
  - `src/commands/ports.rs`: the worktree port source.
  - `src/commands/up.rs`: lock port preservation and `--from`.
  - new `src/worktree.rs` (detection) and `src/commands/prune.rs`.
  - `src/commands/status.rs`, `src/commands/logs.rs`, `src/cli.rs`, and the completion scripts in `src/commands/hook.rs`.
- **Behavior change for existing nix users:** services keep running under their old labels until the next `devy start`, `stop`, `up` or `down`, which migrates them. The log file path changes.
- **Dependencies:** `devy up --from` needs the archive and extract helpers from `add-service-snapshots`, so it lands after them. Port isolation, naming and `prune` don't depend on that change.
- **Shared with in-flight work:** `add-service-snapshots` and `add-agent-cli-support` also modify the `cli` built-in list and `shell-integration` completion. `services --json` from `add-agent-cli-support` should report the worktree port source.
- **No new crates.** Worktree detection reads `.git` files directly and doesn't shell out to `git`.
