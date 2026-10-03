# Spec Delta

## Purpose

Lets several checkouts of one repository, the main checkout and any linked git worktrees, run their devy environments side by side without sharing ports or services. A worktree can start with a copy of another checkout's service data.

## ADDED Requirements

### Requirement: Linked worktree detection
devy SHALL treat the project as being in a linked git worktree when the nearest `.git` at or above the project root is a file, its `gitdir:` line names a directory of the form `<common>/worktrees/<id>`, and that directory exists. The main checkout SHALL be the parent directory of `<common>`. When `<common>` is a bare repository, there is no main checkout. The main checkout's project root SHALL be the main checkout joined with the project root's path relative to the worktree's top level. A `.git` directory, a missing `.git`, or a `.git` file that points anywhere else (such as `.git/modules/` for submodules) SHALL mean the project is not in a linked worktree. Detection MUST NOT run `git`.

#### Scenario: Main checkout
- **WHEN** the project root `/src/app` contains a `.git` directory
- **THEN** devy treats the project as not being in a linked worktree

#### Scenario: Linked worktree
- **WHEN** the project root `/src/app-feat` contains a `.git` file reading `gitdir: /src/app/.git/worktrees/app-feat`, and that directory exists
- **THEN** devy treats the project as a linked worktree whose main checkout is `/src/app`

#### Scenario: Project in a monorepo subdirectory
- **WHEN** `devy.yml` is at `/src/app-feat/services/api` and `/src/app-feat` is a linked worktree of `/src/app`
- **THEN** the main checkout's project root is `/src/app/services/api`

#### Scenario: Submodule is not a worktree
- **WHEN** the project root's `.git` file reads `gitdir: ../.git/modules/lib`
- **THEN** devy treats the project as not being in a linked worktree

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

### Requirement: Seeding service data from another checkout
`devy up --from [<source>]` SHALL copy service data from a source checkout of the same repository into this project before the service start phase starts any service. For each service dependency, the copy SHALL happen only when all of the following are true:
- the service is eligible for snapshots under the service-snapshots rules (today: run by the nix backend)
- the source project has data at `.devy/data/<canonical-name>/`
- this project's `.devy/data/<canonical-name>/` is missing or empty

To copy one service's data, devy SHALL:
1. Hold the source project's `.devy-lock`. If another devy command holds it, wait up to 120 seconds, then fail with `<source root> is busy: another devy command is running there`.
2. Stop the source's service if it is running, using the source project's own `devy.yml` and recorded ports.
3. Archive the source's data directory with the same rules as `devy snapshot save`.
4. Restart the source's service if it was running before.
5. Extract the archive into this project's data directory with the same safety rules as `devy snapshot restore`.

devy SHALL print `✓ <name> data copied from <source root>` for each copied service. It SHALL print `○ <name> already has data, skipped (remove .devy/data/<name> to copy it again)` when the target already has data, and `○ <name> has no data in <source root>, skipped` when the source has none. When the source's recorded major version for the service differs from this project's, devy SHALL skip that service with a warning naming both versions. If extraction fails, devy SHALL remove the partially extracted directory and fail `devy up`. The source's services MUST still be restarted whenever they were running before.

#### Scenario: Copy postgres from the main checkout
- **WHEN** the backend is nix, the main checkout's postgresql is running with data, the worktree has no `.devy/data/postgresql/`, and the user runs `devy up --from` in the worktree
- **THEN** devy stops the main checkout's postgresql, archives its data, restarts it, extracts the data into the worktree, and then starts the worktree's postgresql on the worktree's own port
- **AND** devy does not run `initdb` for the worktree, because the data directory is already initialized

#### Scenario: Target already has data
- **WHEN** the worktree's `.devy/data/postgresql/` is not empty and the user runs `devy up --from`
- **THEN** devy prints that postgresql already has data, skips it, and leaves both checkouts' data unchanged

#### Scenario: Ineligible backend
- **WHEN** the backend is brew and the user runs `devy up --from`
- **THEN** devy reports that no services are eligible for copying, explains that brew services share system data directories, and continues `devy up` without copying

#### Scenario: Source restarted after a failed copy
- **WHEN** the main checkout's redis is running and extraction into the worktree fails
- **THEN** devy restarts the main checkout's redis, removes the partial worktree data, and `devy up` fails

### Requirement: Resolving the copy source
devy SHALL resolve the `--from` source as follows:
- **No value:** the main checkout's project root.
- **A path to an existing directory:** that directory, which MUST be the project root (or contain it at the same relative path) of a checkout that shares this project's git common directory.
- **Anything else:** a branch name, matched against the branch checked out in the main checkout and in each linked worktree.

devy SHALL fail before installing anything with:
- `--from needs a source: this is not a linked worktree` when no value is given outside a linked worktree
- `--from needs a source: this repository has no main checkout` when no value is given in a worktree of a bare repository
- `no checkout of this repository has branch '<b>' checked out` when a branch matches no checkout
- `'<path>' is not a checkout of this repository` when a path shares a different common directory
- `--from source is this project` when the source resolves to this project's own root

`--from` combined with `--dry-run` SHALL be ignored with a warning, as `--update` and `--bootstrap` are.

#### Scenario: Branch name source
- **WHEN** a worktree at `/src/app-api` has branch `feat/api` checked out, and the user runs `devy up --from feat/api` in another worktree of the same repository
- **THEN** devy copies data from `/src/app-api`

#### Scenario: Unknown branch
- **WHEN** the user runs `devy up --from nope` and no checkout has branch `nope` checked out
- **THEN** devy fails with `no checkout of this repository has branch 'nope' checked out` and installs nothing

#### Scenario: From in the main checkout
- **WHEN** the user runs `devy up --from` with no value in the main checkout
- **THEN** devy fails with `--from needs a source: this is not a linked worktree`

### Requirement: Pruning resources of removed checkouts
`devy prune` SHALL find devy-created service resources whose project root no longer contains a `devy.yml`:
- nix launchd agents (`sh.devy.<project>.<name>`) and systemd user units (`devy-<project>-<name>.service`), using the project root recorded in each unit
- docker-managed containers carrying a `sh.devy.project` label, together with each container's named volume

It SHALL list each resource with its kind, name and recorded project root, and then remove them only after confirmation:
- An interactive prompt `Remove N resources? [y/N]` that defaults to no.
- `--yes` skips the prompt.
- When stdin is not a terminal and `--yes` was not given, devy SHALL print the list and fail with `refusing to prune without --yes when not interactive`.

Removing a nix unit SHALL stop it, unload or disable it, and delete its plist or unit file. Removing a docker resource SHALL remove the container and its volume. `devy prune` SHALL NOT need a `devy.yml`. It SHALL skip docker resources with a note when no container CLI is available. It MUST NOT touch units that have no recorded project root. When nothing is found, it SHALL print `○ nothing to prune` and exit 0.

#### Scenario: Removed worktree leaves a running service
- **WHEN** a worktree at `/src/app-feat` ran nix redis, the user ran `git worktree remove /src/app-feat`, and then runs `devy prune --yes`
- **THEN** devy stops the redis unit recorded for `/src/app-feat`, removes its plist or unit file, and prints it as removed

#### Scenario: Live projects untouched
- **WHEN** `/src/app/devy.yml` exists and its services are running, and the user runs `devy prune --yes`
- **THEN** none of `/src/app`'s units, containers or volumes are removed

#### Scenario: Non-interactive without yes
- **WHEN** stale resources exist, stdin is not a terminal, and the user runs `devy prune`
- **THEN** devy lists them, removes nothing, and fails with `refusing to prune without --yes when not interactive`
