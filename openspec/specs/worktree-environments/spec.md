# worktree-environments Specification

## Purpose
Lets several checkouts of one repository, the main checkout and any linked git worktrees, run their devy environments side by side without sharing ports or services.

## Requirements

### Requirement: Linked worktree detection
devy SHALL treat the project as being in a linked git worktree when the nearest `.git` at or above the project root is a file, its `gitdir:` line names a directory of the form `<common>/worktrees/<id>`, that directory exists, and its `gitdir` file points back to the project's `.git` file. The main checkout SHALL be the parent directory of `<common>`. When `<common>` is a bare repository, there is no main checkout. The main checkout's project root SHALL be the main checkout joined with the project root's path relative to the worktree's top level. A `.git` directory, a missing `.git`, or a `.git` file that points anywhere else (such as `.git/modules/` for submodules) SHALL mean the project is not in a linked worktree. On Windows, a `gitdir:` or `commondir` target on a network share SHALL be refused, and the project treated as not in a linked worktree, unless the project itself is on that same share. A device path target SHALL always be refused. Also on Windows, a `.git` that is itself a link, or a `gitdir:` or `commondir` target whose path passes through a link, SHALL mean the project is not in a linked worktree. Only reparse points that redirect a path (symlinks, junctions and mount points) count as links. Other reparse points, such as OneDrive or Cloud Files placeholders, deduplicated files and WOF-compressed files, SHALL be read like ordinary files and directories. Detection MUST NOT run `git`.

#### Scenario: Main checkout
- **WHEN** the project root `/src/app` contains a `.git` directory
- **THEN** devy treats the project as not being in a linked worktree

#### Scenario: Linked worktree
- **WHEN** the project root `/src/app-feat` contains a `.git` file reading `gitdir: /src/app/.git/worktrees/app-feat`, that directory exists, and its `gitdir` file names `/src/app-feat/.git`
- **THEN** devy treats the project as a linked worktree whose main checkout is `/src/app`

#### Scenario: Project in a monorepo subdirectory
- **WHEN** `devy.yml` is at `/src/app-feat/services/api` and `/src/app-feat` is a linked worktree of `/src/app`
- **THEN** the main checkout's project root is `/src/app/services/api`

#### Scenario: Submodule is not a worktree
- **WHEN** the project root's `.git` file reads `gitdir: ../.git/modules/lib`
- **THEN** devy treats the project as not being in a linked worktree

#### Scenario: Missing or mismatched back-link
- **WHEN** the project root `/src/copy` contains a `.git` file reading `gitdir: /src/app/.git/worktrees/app-feat`, and `/src/app/.git/worktrees/app-feat/gitdir` is missing or names `/src/app-feat/.git`
- **THEN** devy treats the project as not being in a linked worktree

#### Scenario: Network share target on Windows
- **WHEN** on Windows, the project root is `C:\src\app-feat`, and its `.git` file reads `gitdir: \\host\share\app\.git\worktrees\app-feat`
- **THEN** devy does not read that path and treats the project as not being in a linked worktree

#### Scenario: Junction in the gitdir path on Windows
- **WHEN** on Windows, the project's `.git` file reads `gitdir: C:\link\app\.git\worktrees\app-feat`, and `C:\link` is a junction
- **THEN** devy treats the project as not being in a linked worktree

#### Scenario: Repository in a OneDrive folder on Windows
- **WHEN** on Windows, a linked worktree and its main repository are in a OneDrive folder whose files are Cloud Files placeholders
- **THEN** devy detects the linked worktree and its main checkout as it would anywhere else

### Requirement: Worktree port file
In a linked worktree, devy SHALL keep the ports it records for the worktree in `.devy/worktree.yml`, a YAML file with `version: 1` and a `ports` map from canonical dependency name to port. Only `devy up` SHALL write the file, atomically, and only when its content changes. Every other command SHALL read it and MUST NOT create or modify it. A missing file SHALL count as having no recorded ports. An unreadable or unparseable file SHALL also count as empty, and devy SHALL warn `ignoring unreadable .devy/worktree.yml: <reason>`. Outside a linked worktree, devy MUST NOT read or write `.devy/worktree.yml`.

#### Scenario: First up in a worktree
- **WHEN** `devy up` runs in a linked worktree with the nix backend, `redis` has no explicit port, and `.devy/worktree.yml` does not exist
- **THEN** devy assigns a free port and writes `.devy/worktree.yml` containing `redis` and that port

#### Scenario: Corrupt worktree file
- **WHEN** `.devy/worktree.yml` contains invalid YAML and the user runs `devy status` in the worktree
- **THEN** devy warns that it is ignoring the file and reports ports as not yet assigned

### Requirement: Explicit ports are shared across checkouts
When `devy up` or `devy check` runs in a linked worktree with a service dependency whose port is set explicitly in `devy.yml` and applicable to the active backend, devy SHALL warn `'<name>' has a fixed port <N> in devy.yml, so it can't run in this worktree and the main checkout at the same time`. The warning MUST NOT fail the command.

#### Scenario: Fixed redis port in a worktree
- **WHEN** `devy.yml` declares `redis` with `port: 6380`, the backend is nix, and the user runs `devy up` in a linked worktree
- **THEN** devy warns that redis's fixed port 6380 is shared with the main checkout and continues

### Requirement: Devy directory is ignored by git
Whenever devy creates or writes into `.devy/`, it SHALL ensure `.devy/.gitignore` exists. If the file is missing, devy SHALL create it with the single line `*`. devy MUST NOT modify an existing `.devy/.gitignore`.

#### Scenario: Ignore file created
- **WHEN** `devy up` creates `.devy/` for the first time
- **THEN** `.devy/.gitignore` exists and contains `*`

#### Scenario: Existing ignore file kept
- **WHEN** `.devy/.gitignore` already exists with custom content
- **THEN** devy leaves it unchanged

### Requirement: Pruning resources of removed checkouts
`devy prune` SHALL find devy-created service resources whose recorded project root has been removed:
- nix launchd agents (`sh.devy.<project>.<name>`) and systemd user units (`devy-<project>-<name>.service`), using the project root recorded in each unit
- docker-managed containers carrying a `sh.devy.project` label

A recorded root SHALL count as removed only when it is an absolute path that no longer exists, its parent directory exists, and, on Linux, that parent is not an `/etc/fstab` mount point that is currently unmounted. A root that exists SHALL be kept even when it has no `devy.yml`. A root that is not valid UTF-8 MUST NOT count as removed. devy SHALL check docker resources only against a local daemon. When the container CLI is configured for a remote daemon, devy SHALL skip docker resources with a note.

devy SHALL remove only containers whose `sh.devy.host` label (docker-services) equals this machine's host id:
- Containers labelled with another id SHALL be left alone and not listed. When there are any, devy SHALL print `○ ignoring N containers labeled by another machine or user, whether or not their checkouts exist` (`1 container` for one).
- Containers of removed checkouts without a `sh.devy.host` label SHALL be listed in a note and not removed: `○ skipping N containers of removed checkouts without a sh.devy.host label (created by an older devy, or when devy couldn't identify the machine): they may be another machine's on a shared daemon`, followed by `○ if this machine created them, remove each with `<cli> rm -f <name>`, and its volume, if it has one, with `<cli> volume rm <name>`:` and one `<name> (<root>)` line per container. For one container the note reads `1 container of a removed checkout` and uses `it`.
- When devy can't identify this machine, it SHALL remove no containers and print `○ devy can't identify this machine, so it removes no containers (see `sh.devy.host` in the README)`.

devy SHALL keep each removed container's data volume by default. With `--volumes`, it SHALL also remove the volume named like the container, but only when that container mounts it. When volumes are kept and any listed container mounts one, devy SHALL print `○ container volumes are kept; use --volumes to remove them too`. Volumes whose container no longer exists are not found.

It SHALL list each resource with its kind, name and recorded project root, and then remove them only after confirmation:
- An interactive prompt `Remove N resources? [y/N]` that defaults to no, or `Remove 1 resource? [y/N]` for a single resource.
- `--yes` skips the prompt.
- When stdin is not a terminal and `--yes` was not given, devy SHALL print the list and fail with `refusing to prune without --yes when not interactive`.

Removing a nix unit SHALL stop it, unload or disable it, and delete its plist or unit file. Removing a container SHALL first check it again, and remove it by its ID only when it is still the listed container, still records the same removed root and still carries this machine's host id; otherwise devy SHALL leave it in place and report that it changed since it was listed. When a removal fails, devy SHALL warn and continue with the others, and then fail with `failed to remove X of N resources`. `devy prune` SHALL NOT need a `devy.yml`. It SHALL skip docker resources with a note when no container CLI is available. It MUST NOT touch units that have no recorded project root. When nothing is found, it SHALL print `○ nothing to prune` and exit 0.

#### Scenario: Removed worktree leaves a running service
- **WHEN** a worktree at `/src/app-feat` ran nix redis, the user ran `git worktree remove /src/app-feat`, and then runs `devy prune --yes`
- **THEN** devy stops the redis unit recorded for `/src/app-feat`, removes its plist or unit file, and prints it as removed

#### Scenario: Live projects untouched
- **WHEN** `/src/app/devy.yml` exists and its services are running, and the user runs `devy prune --yes`
- **THEN** none of `/src/app`'s units, containers or volumes are removed

#### Scenario: Checkout on a branch without devy.yml is kept
- **WHEN** a unit records `/src/app-feat`, that directory exists, and the branch checked out there has no `devy.yml`, and the user runs `devy prune --yes`
- **THEN** devy removes nothing for `/src/app-feat`

#### Scenario: Unmounted drive is kept
- **WHEN** on Linux, a unit records `/mnt/work/app`, `/etc/fstab` lists `/mnt/work` as a mount point, the drive is not mounted so `/mnt/work` is empty, and the user runs `devy prune --yes`
- **THEN** devy removes nothing for `/mnt/work/app`

#### Scenario: Non-interactive without yes
- **WHEN** stale resources exist, stdin is not a terminal, and the user runs `devy prune`
- **THEN** devy lists them, removes nothing, and fails with `refusing to prune without --yes when not interactive`

#### Scenario: Container volume kept by default
- **WHEN** container `devy-app-<hash>-redis`, labelled with this machine's host id, records the removed root `/src/app-feat` and mounts the volume `devy-app-<hash>-redis`, and the user runs `devy prune --yes`
- **THEN** devy removes the container, keeps the volume, and prints `○ container volumes are kept; use --volumes to remove them too`

#### Scenario: Volumes removed with --volumes
- **WHEN** the same container exists and the user runs `devy prune --yes --volumes`
- **THEN** devy lists it as `docker container and volume devy-app-<hash>-redis (/src/app-feat)` and removes both the container and the volume

#### Scenario: Another machine's container is ignored
- **WHEN** a container records the removed root `/src/app-feat` but its `sh.devy.host` label is another machine's id, and the user runs `devy prune --yes`
- **THEN** devy does not list or remove it, and prints `○ ignoring 1 container labeled by another machine or user, whether or not their checkouts exist`

#### Scenario: Unlabelled container is listed, not removed
- **WHEN** a container created by an older devy records the removed root `/src/app-feat` and has no `sh.devy.host` label, and the user runs `devy prune --yes`
- **THEN** devy leaves it in place and names it in the note about containers without a `sh.devy.host` label, with the `docker rm -f` and `docker volume rm` commands to remove it by hand

#### Scenario: Unidentified machine removes no containers
- **WHEN** devy can't identify this machine and a labelled container records a removed root
- **THEN** `devy prune` removes no containers and prints that devy can't identify this machine
