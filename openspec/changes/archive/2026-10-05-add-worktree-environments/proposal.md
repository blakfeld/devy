# Proposal

## Why

Developers and coding agents increasingly work in several git worktrees of the same repo at once, but devy assumes one checkout per repo. Today a second worktree breaks the first one:

- **Ports collide.** `assigned_port` lives in the committed `devy.lock`, so every worktree of a branch inherits the same ports as the main checkout.
- **Nix services are hijacked.** Under nix, the launchd label (`sh.devy.<name>`) and systemd unit (`devy-<name>.service`) are global per user, not per project. Running `devy up` in a worktree on macOS rewrites and reloads the main checkout's `sh.devy.postgresql` agent so it points at the worktree's empty `.devy/data/postgresql/`. `devy down` in either checkout stops the other's services. The same happens between two unrelated projects that both declare `redis`.

Docker-managed services already avoid the naming problem: containers and volumes are named `devy-<project>-<hash>-<service>` from the project root. They still share ports through `devy.lock`.

## What Changes

- **Per-project nix service names.** Under nix, the launchd label becomes `sh.devy.<project>.<name>`, the systemd unit becomes `devy-<project>-<name>.service`, and the macOS log becomes `devy-<project>-<name>.log` in devy's private per-user directory under `$TMPDIR` (or `$XDG_RUNTIME_DIR`). `<project>` is the slug docker services already use: the sanitized project name plus 8 hex characters of a hash of the project root. This fixes collisions between unrelated projects as well as between worktrees.
  - **Migration:** when devy starts, stops or checks a nix service and finds a legacy `sh.devy.<name>` / `devy-<name>.service` unit that belongs to this project root, it stops and removes it. Units that belong to other projects are left alone.
- **Worktree detection.** devy recognizes a linked git worktree (`.git` is a file pointing into `<main>/.git/worktrees/`) and finds the main checkout. `devy status` shows `worktree of <main path>`.
- **Worktree-local ports.** In a linked worktree, port resolution reads and writes `.devy/worktree.yml` instead of `assigned_port` in `devy.lock`. `devy up` in a worktree keeps `devy.lock`'s existing `assigned_port` values unchanged, so worktree branches don't produce port churn in the committed lock. Explicit ports in `devy.yml` still win, and devy warns that they are shared with the main checkout.
- **`.devy/.gitignore`.** devy writes a `.devy/.gitignore` containing `*` the first time it creates `.devy/`, so `worktree.yml` and service data are never committed.
- **`devy prune [--yes] [--volumes]`.** Finds nix units and docker containers whose project root no longer exists, for example after `git worktree remove`, then lists them and removes them after confirmation. Container data volumes are kept unless `--volumes` is given.
- **Host label on containers.** devy labels every container it creates with `sh.devy.host=<id>`, a hash of a machine identifier and the user, so machines sharing one container daemon (WSL distributions, docker-outside-of-docker, forwarded sockets) don't act on each other's containers. `prune` removes only this machine's containers. `up`, `start` and `restart` refuse another machine's container, and `stop` and `down` skip it. The environment keys devy reads the identity from become reserved in `devy.yml`.
- **Safer log reading.** `devy logs` reads nix launchd logs only from devy's private per-user directory, and refuses log files that are symlinks, not owned by the user or root, or in a directory others can write to.
- `prune` becomes a built-in subcommand, and shell completion covers `prune`, `--yes` and `--volumes`.
- **Not included:** copying service data between checkouts. Each checkout keeps its own `.devy/data/`, so a new worktree starts with empty services. Users who need seeded data can define a project command in `devy.yml`.

## Capabilities

### New Capabilities
- `worktree-environments`: detecting linked worktrees and the main checkout, the worktree port file, `.devy/.gitignore`, and `devy prune`.

### Modified Capabilities
- `service-ports`: port precedence and persistence use `.devy/worktree.yml` in linked worktrees, and `devy.lock` ports are left untouched there.
- `service-management`: nix launchd labels and systemd unit names include the project slug, and legacy units are migrated.
- `service-logs`: nix log sources use the new label, unit and log file names, and log files are opened only when they and their directory are safe to read.
- `docker-services`: containers carry a `sh.devy.host` label, and commands leave another machine's container alone.
- `project-config`: environment keys devy reads the machine identity from are reserved.
- `environment-check`: `devy status` reports when it runs in a linked worktree.
- `cli`: `prune` is added to the built-in subcommands.
- `shell-integration`: completion covers `prune` and its flags.

## Impact

- **Code:**
  - `src/package_manager/nix.rs`: label, unit and log naming, project root recorded in units, and legacy migration.
  - `src/commands/ports.rs`: the worktree port source.
  - `src/commands/up.rs`: lock port preservation.
  - new `src/worktree.rs` (detection), `src/commands/prune.rs`, `src/service_runner/prune.rs` (container discovery) and `src/service_runner/host_id.rs` (the host label).
  - `src/service_runner/mod.rs` (host label checks) and `src/validate.rs` (reserved identity keys).
  - `src/commands/status.rs`, `src/commands/logs.rs`, `src/cli.rs`, and the completion scripts in `src/commands/hook.rs`.
- **Behavior change for existing nix users:** services keep running under their old labels until the next `devy start`, `stop`, `up` or `down`, which migrates them. The log file path changes.
- **Builds on `add-agent-cli-support` (archived):** the `cli` and `shell-integration` deltas start from its merged requirement text.
- **Behavior change for docker users on a shared daemon:** containers created by older versions have no host label. They keep working as before, but `devy prune` lists them without removing them.
- **No new crates.** Worktree detection reads `.git` files directly and doesn't shell out to `git`.
