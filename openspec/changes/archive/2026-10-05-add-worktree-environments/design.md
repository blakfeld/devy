# Design

## Context

See proposal.md (Why). The current state that shapes the approach:

- **Discovery and project roots.** `Config::find_config` (`src/config.rs`) walks up to the first directory with `devy.yml`, and stops at any directory containing `.git`. In a linked worktree `.git` is a file, and `.exists()` already treats it as a stopping point. So the project root of a worktree is already the worktree, and everything under `.devy/` (the nix profile, `.devy/data/`, the shadowenv file, stamps) is already separate for each checkout. The collisions come only from things keyed outside the project root: ports in the committed `devy.lock`, and the global nix unit names.
- **Nix unit names.** `src/package_manager/nix.rs` builds names from the bare service name: `sh.devy.{name}` (in `launchagent_path`, `start_service_macos`, `is_loaded_macos`, `stop_service_macos`), `devy-{name}.service` (in `systemd_unit_path`, `stop_service_linux`), and `$TMPDIR/devy-{name}.log`. `start_service_macos` unloads any loaded agent with that label before loading its own plist. That is how a worktree takes over the main checkout's service.
- **Docker naming** already uses `modules::helpers::project_slug(name, root)`, which is the sanitized name plus 8 hex characters of an FNV-1a hash of the root (`src/service_runner/mod.rs`). Containers already carry `sh.devy.project=<root>`.
- **Ports.** `src/commands/ports.rs::resolve_ports` takes `Option<&LockFile>` and implements the explicit → locked → assigned → default precedence. `up.rs` builds the new lock, including `assigned_port`, and writes it only when it changed.

## Goals / Non-Goals

**Goals:**
- Any number of checkouts of one repository can run `devy up` at the same time under nix or docker, without sharing ports, units or data.
- Worktrees never cause `devy.lock` port churn.
- Existing single-checkout users migrate without doing anything.

**Non-Goals:**
- Isolating brew, apt and WinGet services. They are single system-wide instances, so worktrees share them, as unrelated projects already do. Their ports aren't applicable anyway.
- Copying service data between checkouts. Users who need it can define a project command in devy.yml.
- Hooking `git worktree add/remove` to run `devy up` or `devy prune` automatically.
- Per-worktree overrides of `devy.yml` values such as environment or explicit ports.

## Decisions

### D1: Put the project slug in nix unit names, for every project
Labels become `sh.devy.<slug>.<name>`, units `devy-<slug>-<name>.service`, and logs `devy-<slug>-<name>.log` in devy's private per-user directory under `$TMPDIR` (or `$XDG_RUNTIME_DIR`), using the existing `project_slug`.
- **Alternative:** add a suffix only in worktrees, for example `sh.devy.<name>.<worktree-id>`. That fixes worktrees but leaves the identical bug between two unrelated projects that both declare `redis`.
- **Alternative:** hash the root only, without the name. That works, but `launchctl list`, `systemctl --user list-units` and the log directory become unreadable.

The slug keeps names readable and matches the docker container names users already see. One function in `nix.rs`, taking the project slug and the service name, replaces the eight `format!` call sites. The slug has to reach the nix backend, which today only gets `name`, so the `PackageManager` service methods gain a project context argument, or the backend gets the root at construction. The nix backend already receives `exec_dir` and `profile_bin` derived from the root, so constructing it with the root is the smaller change.

### D2: Record the owning root as `DEVY_PROJECT_ROOT` in the unit's environment
`prune` and slug-rename migration need to know which root a unit belongs to.
- **Alternatives:** a custom plist key (launchd ignores unknown keys, but systemd has no equivalent besides `X-` keys), or parsing the data directory back out of `ProgramArguments`, which works differently for every module.

An environment entry is written by the same code on both platforms (`EnvironmentVariables` / `Environment=`), and it's easy to parse back. The service process also sees it, which is harmless.

Legacy units have no such entry, so their ownership is decided by looking for this root's `.devy/data/` path or `.devy/nix-profile` path in `ProgramArguments`/`ExecStart` or `WorkingDirectory`. Every nix launch definition passes one of these.

### D3: Migrate lazily, whenever a service is started or stopped
A single per-service check (`find_stale_units(name, root)`) runs before start and stop. Read-only commands use the same lookup only to answer "is it running" (new name, else an owned legacy unit), and never remove anything, because `devy status` must not write files. It removes legacy-named and other-slug units owned by this root. On start, the removal happens only right before the new unit is written, so a start that fails earlier (for example on a port check) leaves a running legacy unit in place. This needs no new command and no state file, and it covers project renames.
- **Alternative:** a one-shot `devy migrate`. Users would never run it.
- **Cost:** a directory scan of `~/Library/LaunchAgents` or `~/.config/systemd/user` per service. These directories are small, and only `sh.devy.*` / `devy-*` entries are parsed.

### D4: Detect worktrees by reading `.git` files, not by running `git`
`src/worktree.rs` finds the `.git` at or above the project root.
- If `.git` is a directory, the project is not in a linked worktree.
- If `.git` is a file, devy reads its `gitdir:` line, resolving a relative path against the file's directory (git ≥2.48 can write relative paths). It accepts the directory as a linked worktree only when it has a `commondir` file and sits under `<common>/worktrees/`.
- It reads `commondir`, which is usually `../..`, instead of assuming the grandparent directory.
- It requires git's back-link: `<common>/worktrees/<id>/gitdir` must point back at this checkout's `.git` file. A copied or forged `.git` file therefore can't claim another repository's main checkout.
- On Windows, it refuses `gitdir:` and `commondir` targets on a UNC share unless the project itself is on that same share, and always refuses device paths. Merely touching a UNC path connects to that host and can leak the user's credentials.
- On Windows, it also refuses a `.git` that is a link, and any target whose path passes through a link, since a link can lead to a share. Only reparse points that redirect a path count as links: symlinks, junctions and mount points (the name-surrogate tags std reports as symlinks). OneDrive / Cloud Files placeholders, dedup and WOF compression leave the path where it is, so a repository in such a folder is still detected. Files are read without opening the reparse point itself, which for those tags could return the stub instead of the content.
- The main checkout is `<common>/..`, unless `<common>/config` has `bare = true`.

Every command calls this, through port resolution and status, so it must be cheap. It must also work when `git` isn't on PATH, for example when git itself comes from the project's nix profile.

### D5: Keep worktree ports in `.devy/worktree.yml`
Alternatives considered:
- **Write worktree ports into `devy.lock`.** Every worktree branch commits different ports, and merges conflict.
- **Deterministic offsets per worktree** (base + hash). Offsets collide with each other and with unrelated listeners, and losing the "free port from the OS" guarantee is worse than storing a file.
- **A root-level `devy.local.lock`.** That needs an entry in the user's own `.gitignore`.

`.devy/` is already per checkout and already devy-owned. `.devy/.gitignore` with `*` makes it safe without touching the user's ignore rules.

In code, `resolve_ports` takes a `PortSource` enum (`Lock(&LockFile)` / `Worktree(&WorktreePorts)`) instead of `Option<&LockFile>`. Callers get it from one helper that does the worktree detection, so `up`, `start`, `restart`, `check`, `status` and docker resolution all agree.

### D6: Preserve `devy.lock` ports in a worktree by copying them from the previous lock
`up.rs` builds the new lock exactly as today. In a worktree, it then replaces every entry's `assigned_port` with the previous lock's value for that name (or `None`). The existing "write only on change" comparison then keeps the lock untouched when only ports differ. The worktree's resolved ports go to `.devy/worktree.yml` through the same atomic temp-file-and-rename helper the lock uses.

### D7: `prune` discovers resources from the user's unit directories and the container CLI
- **Nix:** devy scans `~/Library/LaunchAgents/sh.devy.*.plist` or `~/.config/systemd/user/devy-*.service` and reads `DEVY_PROJECT_ROOT`. A unit is stale when its recorded root has been removed (see below).
- **Docker:** `<cli> ps -a --filter label=sh.devy.project --format …` lists containers, and the label gives the root. The CLI is `docker`, falling back to `podman`, because there may be no `devy.yml` from which to read `container_cli`. Only containers labelled with this machine's `sh.devy.host` id are removed (D8). Others are counted in a note, and unlabelled containers of removed checkouts are listed with the commands to remove them by hand. Each container is checked again right before removal and removed by its ID.
- **Volumes are kept by default.** A data volume is the only copy of a service's data, and a mistaken prune of a container is recoverable while one of its volume is not. `--volumes` also removes the volume named like the container, but only when that container mounts it, so a same-named volume belonging to something else is left alone. Volumes whose container is already gone aren't found, because a volume records no project root.
  - **Alternative:** remove volumes by default, like `docker compose down -v`. Rejected: prune acts on several projects at once, and its "removed" test is a heuristic.

A recorded root counts as removed only when it is an absolute path that no longer exists, its parent directory still exists, and, on Linux, that parent is not an `/etc/fstab` mount point that is currently unmounted. A root that exists but has no `devy.yml`, for example a checkout switched to a branch without one, is kept. Requiring the parent guards against an unmounted drive or network share making every project on it look removed. Roots that aren't valid UTF-8 never count as removed.

Docker resources are checked only against a local daemon. When the CLI may be talking to a remote daemon (a non-local `DOCKER_HOST` or `CONTAINER_HOST`, a docker context with a non-local endpoint, or podman reporting a remote service), devy skips docker with a note, because the recorded roots are paths on another machine.

Units without `DEVY_PROJECT_ROOT` (legacy ones) are never pruned. Their owner is unknown, and lazy migration handles them.

### D8: Label containers with a host id
A local socket doesn't mean a single machine. Two WSL distributions can share Docker Desktop's daemon, a devcontainer can use its host's socket (docker-outside-of-docker), and a socket can be forwarded over ssh. Containers from these machines can record the same project path, so a root that is removed on one machine can still be live on another, and `devy up` on one could reuse or replace the other's container.

devy labels every container it creates with `sh.devy.host=<id>`, 16 hex characters of a SHA-256 hash, with a fixed devy-specific prefix, so no raw identifier is written to a label. The hashed parts are:
- a machine identifier: `/etc/machine-id` (or dbus's copy) on Linux, `IOPlatformUUID` from `ioreg` on macOS, `MachineGuid` from the registry on Windows, else the host name
- the host name as well inside a docker or podman container (`/.dockerenv`, `/run/.containerenv`), which otherwise shares the host's machine id
- the distribution name under WSL (`WSL_DISTRO_NAME`), since a distribution cloned with `wsl --import` keeps its machine id
- the user: the uid, or on Windows the domain and user name

With any required part missing (for example WSL sessions not started by `wsl.exe`, which lack `WSL_DISTRO_NAME`), there is no id. devy then creates unlabelled containers, refuses labelled ones, and prunes no containers, rather than share an id with other unidentified machines. System tools are run from fixed system paths, and the environment keys devy reads are reserved in `devy.yml`, so a project can't change the id.

Prune removes only containers carrying this machine's id. `up`, `start` and `restart` refuse a container labelled for another machine, or any labelled container when this machine has no id. `stop` and `down` treat it as not running, and `down --volumes` keeps it. Unlabelled containers, from older versions, keep their previous behaviour so existing setups don't break, but prune never removes them.

This is not a security boundary. Anyone with access to the daemon can set any label. It only prevents accidental collisions between cooperating machines.
- **Alternative:** check the daemon's own identity (`docker info` ID). Every machine sharing the daemon sees the same ID, so it can't tell them apart.
- **Alternative:** skip docker in prune whenever the daemon might be shared. That can't be detected reliably for a local socket.

## Risks / Trade-offs

- **Moving or renaming a checkout directory changes its slug, which orphans running units.** The data directory moves with the checkout, and the old unit's recorded root no longer exists, so `devy prune` cleans it up. The next `devy up` starts fresh units. If the old unit still holds the locked port, the existing port-in-use failure points at the cause. → The `prune` hint is included in that failure message.
- **Until the first start or stop, a legacy unit and the new name coexist in devy's view.** Read-only commands fall back to the owned legacy unit when reporting running state, so `devy status` stays accurate without migrating. `devy check` prints a non-failing note.
- **Explicit ports in `devy.yml` still collide between checkouts.** That's by design: devy only warns. Users who need both running remove the explicit port.
- **The host id can change.** Rebuilding a devcontainer (new host name) or changing the machine id or user changes the label, and the old containers then count as another machine's. `up` fails with a hint to inspect and remove the container by hand, and prune ignores them. → The error and the README say how to check and remove them.
- **Containers from older versions have no host label.** They keep working, but prune only lists them, because on a shared daemon they may be another machine's. Running `devy up` doesn't add the label; a container gets it when it is next recreated.
- **Multi-user Homebrew.** `devy logs` refuses log files owned by another user, or in a directory another user can write to. On a Mac where Homebrew is owned by a different admin account, brew services' log files belong to that account, so `devy logs` refuses them and the user reads them directly instead.
- **Nix logs need the private directory.** A per-user log directory that is not owned by the user with mode 0700 is refused, not repaired, so `devy logs` fails until the user removes it and the next `devy start` recreates it.

## Migration Plan

1. Ship the naming change (D1–D3) together with port isolation. Neither is useful alone in a worktree, and lazy migration handles existing users the first time devy touches each service.
2. Downgrading: an older devy can't see per-project units. Run `devy down` with the new version before downgrading, or remove `sh.devy.<slug>.*` agents by hand. This is noted in the release notes.

## Open Questions

- Should `prune` also offer to delete stale `devy-*.log` files from devy's per-user log directory? They're harmless and the OS clears them, so the default is no.
