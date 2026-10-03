# Design

## Context

See proposal.md (Why). The current state that shapes the approach:

- **Discovery and project roots.** `Config::find_config` (`src/config.rs`) walks up to the first directory with `devy.yml`, and stops at any directory containing `.git`. In a linked worktree `.git` is a file, and `.exists()` already treats it as a stopping point. So the project root of a worktree is already the worktree, and everything under `.devy/` (the nix profile, `.devy/data/`, the shadowenv file, stamps) is already separate for each checkout. The collisions come only from things keyed outside the project root: ports in the committed `devy.lock`, and the global nix unit names.
- **Nix unit names.** `src/package_manager/nix.rs` builds names from the bare service name: `sh.devy.{name}` (in `launchagent_path`, `start_service_macos`, `is_loaded_macos`, `stop_service_macos`), `devy-{name}.service` (in `systemd_unit_path`, `stop_service_linux`), and `$TMPDIR/devy-{name}.log`. `start_service_macos` unloads any loaded agent with that label before loading its own plist. That is how a worktree takes over the main checkout's service.
- **Docker naming** already uses `modules::helpers::project_slug(name, root)`, which is the sanitized name plus 8 hex characters of an FNV-1a hash of the root (`src/service_runner/mod.rs`). Containers already carry `sh.devy.project=<root>`.
- **Ports.** `src/commands/ports.rs::resolve_ports` takes `Option<&LockFile>` and implements the explicit → locked → assigned → default precedence. `up.rs` builds the new lock, including `assigned_port`, and writes it only when it changed.
- **Snapshots.** `add-service-snapshots` (not yet implemented) defines the archive and extract helpers, the eligibility check, and a shared `.devy-lock` helper in `commands/shared.rs`. `--from` builds on those.

## Goals / Non-Goals

**Goals:**
- Any number of checkouts of one repository can run `devy up` at the same time under nix or docker, without sharing ports, units or data.
- Worktrees never cause `devy.lock` port churn.
- Existing single-checkout users migrate without doing anything.

**Non-Goals:**
- Isolating brew, apt and WinGet services. They are single system-wide instances, so worktrees share them, as unrelated projects already do. Their ports aren't applicable anyway.
- `--from` for docker volumes. It follows `add-service-snapshots`' docker phase.
- Hooking `git worktree add/remove` to run `devy up` or `devy prune` automatically.
- Per-worktree overrides of `devy.yml` values such as environment or explicit ports.

## Decisions

### D1: Put the project slug in nix unit names, for every project
Labels become `sh.devy.<slug>.<name>`, units `devy-<slug>-<name>.service`, and logs `$TMPDIR/devy-<slug>-<name>.log`, using the existing `project_slug`.
- **Alternative:** add a suffix only in worktrees, for example `sh.devy.<name>.<worktree-id>`. That fixes worktrees but leaves the identical bug between two unrelated projects that both declare `redis`.
- **Alternative:** hash the root only, without the name. That works, but `launchctl list`, `systemctl --user list-units` and `$TMPDIR` become unreadable.

The slug keeps names readable and matches the docker container names users already see. One function in `nix.rs`, taking the project slug and the service name, replaces the eight `format!` call sites. The slug has to reach the nix backend, which today only gets `name`, so the `PackageManager` service methods gain a project context argument, or the backend gets the root at construction. The nix backend already receives `exec_dir` and `profile_bin` derived from the root, so constructing it with the root is the smaller change.

### D2: Record the owning root as `DEVY_PROJECT_ROOT` in the unit's environment
`prune` and slug-rename migration need to know which root a unit belongs to.
- **Alternatives:** a custom plist key (launchd ignores unknown keys, but systemd has no equivalent besides `X-` keys), or parsing the data directory back out of `ProgramArguments`, which works differently for every module.

An environment entry is written by the same code on both platforms (`EnvironmentVariables` / `Environment=`), and it's easy to parse back. The service process also sees it, which is harmless.

Legacy units have no such entry, so their ownership is decided by looking for this root's `.devy/data/` path or `.devy/nix-profile` path in `ProgramArguments`/`ExecStart` or `WorkingDirectory`. Every nix launch definition passes one of these.

### D3: Migrate lazily, whenever a service is started or stopped
A single per-service check (`find_stale_units(name, root)`) runs before start and stop. Read-only commands use the same lookup only to answer "is it running" (new name, else an owned legacy unit), and never remove anything, because `devy status` must not write files. It removes legacy-named and other-slug units owned by this root. This needs no new command and no state file, and it covers project renames.
- **Alternative:** a one-shot `devy migrate`. Users would never run it.
- **Cost:** a directory scan of `~/Library/LaunchAgents` or `~/.config/systemd/user` per service. These directories are small, and only `sh.devy.*` / `devy-*` entries are parsed.

### D4: Detect worktrees by reading `.git` files, not by running `git`
`src/worktree.rs` finds the `.git` at or above the project root.
- If `.git` is a directory, the project is not in a linked worktree.
- If `.git` is a file, devy reads its `gitdir:` line, resolving a relative path against the file's directory (git ≥2.48 can write relative paths). It accepts the directory as a linked worktree only when it has a `commondir` file and sits under `<common>/worktrees/`.
- It reads `commondir`, which is usually `../..`, instead of assuming the grandparent directory.
- The main checkout is `<common>/..`, unless `<common>/config` has `bare = true`.

Every command calls this, through port resolution and status, so it must be cheap. It must also work when `git` isn't on PATH, for example when git itself comes from the project's nix profile. Branch lookup for `--from <branch>` reads `HEAD` in the common directory and in each `<common>/worktrees/*/HEAD` (`ref: refs/heads/<b>`). `<common>/worktrees/*/gitdir` gives each worktree's location.

### D5: Keep worktree ports in `.devy/worktree.yml`
Alternatives considered:
- **Write worktree ports into `devy.lock`.** Every worktree branch commits different ports, and merges conflict.
- **Deterministic offsets per worktree** (base + hash). Offsets collide with each other and with unrelated listeners, and losing the "free port from the OS" guarantee is worse than storing a file.
- **A root-level `devy.local.lock`.** That needs an entry in the user's own `.gitignore`.

`.devy/` is already per checkout and already devy-owned. `.devy/.gitignore` with `*` makes it safe without touching the user's ignore rules. The snapshot change's `.devy/snapshots/.gitignore` becomes redundant but harmless.

In code, `resolve_ports` takes a `PortSource` enum (`Lock(&LockFile)` / `Worktree(&WorktreePorts)`) instead of `Option<&LockFile>`. Callers get it from one helper that does the worktree detection, so `up`, `start`, `restart`, `check`, `status` and docker resolution all agree.

### D6: Preserve `devy.lock` ports in a worktree by copying them from the previous lock
`up.rs` builds the new lock exactly as today. In a worktree, it then replaces every entry's `assigned_port` with the previous lock's value for that name (or `None`). The existing "write only on change" comparison then keeps the lock untouched when only ports differ. The worktree's resolved ports go to `.devy/worktree.yml` through the same atomic temp-file-and-rename helper the lock uses.

### D7: `--from` treats the source as a full project
The source checkout may be on a different branch with a different `devy.yml`, ports or backend. devy loads the source's config and its port source (its lock, or its own `worktree.yml` if the source is itself a worktree). It then uses that context to stop and restart the source's services, so it never guesses a port or unit name from the target's config.

The copy runs in `up` after the lock write and package installs, and before the service start phase. That means the target's binaries are installed and the version guard can compare the source's locked version with the target's. Because extraction fills the data directory first, the one-time init (`initdb` and similar) sees an initialized directory and is skipped.

The archive is streamed to a temp file under the target's `.devy/`, so the final rename stays on one filesystem. It is then extracted with the snapshot code's safe extractor.

### D8: Bounded wait on the source's lock
The target already holds its own `.devy-lock`. If two worktrees run `--from` each other at the same time, unbounded waits would deadlock. devy polls `try_lock` on the source for up to 120 seconds, then fails with the busy message in the spec. That is long enough for a normal `devy up` in the source to finish starting services.

### D9: `prune` discovers resources from the user's unit directories and the container CLI
- **Nix:** devy scans `~/Library/LaunchAgents/sh.devy.*.plist` or `~/.config/systemd/user/devy-*.service` and reads `DEVY_PROJECT_ROOT`. A unit is stale when `<root>/devy.yml` doesn't exist.
- **Docker:** `<cli> ps -a --filter label=sh.devy.project --format …` lists containers, the label gives the root, and each container's volume has the same name as the container. The CLI is `docker`, falling back to `podman`, because there may be no `devy.yml` from which to read `container_cli`.

Units without `DEVY_PROJECT_ROOT` (legacy ones) are never pruned. Their owner is unknown, and lazy migration handles them.

## Risks / Trade-offs

- **Moving or renaming a checkout directory changes its slug, which orphans running units.** The data directory moves with the checkout, and the old unit's recorded root no longer has a `devy.yml`, so `devy prune` cleans it up. The next `devy up` starts fresh units. If the old unit still holds the locked port, the existing port-in-use failure points at the cause. → The `prune` hint is included in that failure message.
- **`--from` briefly stops the source's services.** For a main checkout with a running app, this interrupts the app for as long as the archive takes. → It's printed per service (`stopping <name> in <source root> to copy its data`), and the service is always restarted.
- **Large data directories make `--from` slow,** with no progress output. → The archive size is printed after each copy. Copy-on-write cloning (APFS `clonefile`, `cp --reflink`) is a possible later optimization.
- **Until the first start or stop, a legacy unit and the new name coexist in devy's view.** Read-only commands fall back to the owned legacy unit when reporting running state, so `devy status` stays accurate without migrating. `devy check` prints a non-failing note.
- **Explicit ports in `devy.yml` still collide between checkouts.** That's by design: devy only warns. Users who need both running remove the explicit port.
- **Overlap with in-flight changes.** `add-service-snapshots` and `add-agent-cli-support` also add MODIFIED deltas to `cli` "Built-in subcommands" and `shell-integration` "Tab completion". Whichever archives second must rebase its delta onto the merged requirement text.

## Migration Plan

1. Ship the naming change (D1–D3) together with port isolation. Neither is useful alone in a worktree, and lazy migration handles existing users the first time devy touches each service.
2. Downgrading: an older devy can't see per-project units. Run `devy down` with the new version before downgrading, or remove `sh.devy.<slug>.*` agents by hand. This is noted in the release notes.
3. `--from` lands after `add-service-snapshots`' archive helpers. If they're delayed, the rest of this change ships without `--from` and its completion entry.

## Open Questions

- Should `devy up` in a fresh worktree with empty data print a one-line tip about `--from`? It's cheap and doesn't change the approach. Decide during implementation based on how noisy it feels.
- Should `prune` also offer to delete stale `$TMPDIR/devy-*.log` files? They're harmless and the OS clears them, so the default is no.
