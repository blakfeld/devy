# Spec Delta

## Purpose

Lets developers save, list, restore and delete named point-in-time copies of the service data devy owns for a project, so a local database can be returned to a known-good state.

## ADDED Requirements

### Requirement: Snapshot eligibility
A service SHALL be eligible for snapshots only when devy owns its data. That means the nix backend runs it and its data lives in `<project_root>/.devy/data/<canonical-name>/`. Services run by brew, apt or WinGet MUST NOT be snapshotted, because their data lives in shared system directories.
- When `--service` names an ineligible service, the command SHALL fail with `<dep>: snapshots need devy-managed service data, but <dep> runs under <pm>, which keeps its data outside the project`.
- When no `--service` is given, the command SHALL act on every eligible service dependency in declaration order. If there are none, it SHALL fail with `no services eligible for snapshots (snapshots need the nix backend)`.

#### Scenario: Ineligible service named explicitly
- **WHEN** `package_manager: brew` and the user runs `devy snapshot save base --service postgres`
- **THEN** devy fails with the eligibility message for `postgres` and exits 1
- **AND** nothing is written under `.devy/snapshots/`

#### Scenario: No eligible services
- **WHEN** `package_manager: apt` and `devy.yml` declares only `redis`, and the user runs `devy snapshot save base`
- **THEN** devy fails with `no services eligible for snapshots (snapshots need the nix backend)`

#### Scenario: Unknown service
- **WHEN** the user runs `devy snapshot save base --service nope` and `nope` is not a service dependency
- **THEN** devy fails with the same message that `devy start nope` gives

### Requirement: Snapshot names
A snapshot name SHALL match `[A-Za-z0-9][A-Za-z0-9._-]*` and be at most 64 characters long. Otherwise, devy SHALL fail with `invalid snapshot name '<name>' — use letters, digits, '.', '_' and '-'`.

#### Scenario: Path traversal rejected
- **WHEN** the user runs `devy snapshot save ../escape`
- **THEN** devy fails with the invalid-name message and writes nothing

### Requirement: Saving a snapshot
`devy snapshot save <name> [--service <svc>]... [--force]` SHALL do the following for each selected service, one at a time:
1. Stop the service if it is running, waiting as `devy stop` does.
2. Archive the contents of its data directory to `.devy/snapshots/<name>/<canonical-name>.tar.gz`, skipping Unix sockets and pid files.
3. Start the service again only if it was running before, and wait for readiness as `devy start` does.

The command SHALL write `.devy/snapshots/<name>/manifest.json`, recording:
- the creation time
- the devy version
- the package manager
- for each service, its canonical name and its resolved version from `devy.lock`, when present

On success, it SHALL print `✓ snapshot '<name>' saved (<services>)`.

If `<name>` already exists, the command SHALL fail with `snapshot '<name>' already exists — use --force to overwrite` unless `--force` is given. With `--force`, devy SHALL replace the existing snapshot only after the new one is complete.

If archiving fails, devy SHALL remove the partial snapshot, still restart any service it stopped, and fail with `Failed to snapshot <dep>`.

#### Scenario: Save all running nix services
- **WHEN** postgres and redis are running under nix and the user runs `devy snapshot save base`
- **THEN** `.devy/snapshots/base/` contains `postgresql.tar.gz`, `redis.tar.gz` and `manifest.json`
- **AND** postgres and redis are running again when the command exits

#### Scenario: Stopped service stays stopped
- **WHEN** postgres is stopped and the user runs `devy snapshot save base --service postgres`
- **THEN** the snapshot is written and postgres is still stopped afterwards

#### Scenario: Existing name without force
- **WHEN** a snapshot named `base` exists and the user runs `devy snapshot save base`
- **THEN** devy fails with the already-exists message and the existing snapshot is unchanged

#### Scenario: Service with no data yet
- **WHEN** a selected service's data directory does not exist yet
- **THEN** devy warns `<dep> has no data yet — skipped` and leaves it out of the manifest

### Requirement: Snapshot storage is never committed
Before writing its first snapshot, devy SHALL make sure `.devy/snapshots/.gitignore` exists and contains `*`.

#### Scenario: Gitignore written on first save
- **WHEN** `.devy/snapshots/` does not exist and the user saves a snapshot
- **THEN** `.devy/snapshots/.gitignore` exists afterwards and contains `*`

### Requirement: Restoring a snapshot
`devy snapshot restore <name> [--service <svc>]... [--yes] [--force]` SHALL replace each selected service's data directory with that service's archive from the snapshot. When no `--service` is given, the selection SHALL be every service in the manifest that is still a service dependency in `devy.yml`.

Before changing anything, devy SHALL print the services that will be overwritten and ask `Overwrite current data for <services>? [y/N]`. Any answer other than `y` or `yes` SHALL abort with exit 1 and change nothing. With `--yes`, devy SHALL skip the prompt. When stdin is not a terminal and `--yes` is absent, devy SHALL fail with `restore would overwrite data — pass --yes to confirm`.

For each selected service, devy SHALL:
1. Stop the service if it is running.
2. Move the current data directory aside.
3. Extract the archive in its place.
4. Remove the set-aside copy, but only after extraction succeeds.

If extraction fails, devy SHALL put the original data directory back, fail with `Failed to restore <dep>`, and leave the services not yet processed untouched. After restoring, devy SHALL restart each service that was running before and wait for readiness.

#### Scenario: Restore with confirmation
- **WHEN** the user runs `devy snapshot restore base` in a terminal and answers `y`
- **THEN** postgres's data directory holds the snapshot's contents and postgres is running again

#### Scenario: Declined confirmation
- **WHEN** the user answers `n` to the restore prompt
- **THEN** devy exits 1 and no data directory is changed

#### Scenario: Non-interactive without --yes
- **WHEN** `devy snapshot restore base` runs with stdin not a terminal and no `--yes`
- **THEN** devy fails with `restore would overwrite data — pass --yes to confirm`

#### Scenario: Corrupt archive rolls back
- **WHEN** `postgresql.tar.gz` in the snapshot is truncated and the user runs `devy snapshot restore base --yes`
- **THEN** devy fails with `Failed to restore postgresql`
- **AND** `.devy/data/postgresql/` holds the same data it held before the command

#### Scenario: Unknown snapshot
- **WHEN** the user runs `devy snapshot restore nope`
- **THEN** devy fails with `no snapshot named 'nope' — see devy snapshot list`

### Requirement: Restore version guard
When the manifest records a version for a service and devy can determine the service's currently resolved version, and their major versions (the text before the first `.`) differ, restore SHALL refuse that service. It SHALL fail with `<dep> snapshot is from version <old>, but <new> is installed; data formats may be incompatible — pass --force to restore anyway`. With `--force`, devy SHALL warn and continue. When either version is unknown, devy SHALL restore without checking.

#### Scenario: Major version mismatch
- **WHEN** the snapshot recorded postgresql `15.6` and the project now resolves postgresql `16.2`
- **THEN** `devy snapshot restore base --yes` fails with the version-guard message and changes nothing

#### Scenario: Minor version difference
- **WHEN** the snapshot recorded redis `7.2.4` and the project now resolves redis `7.2.5`
- **THEN** the restore proceeds without a version warning

### Requirement: Listing and deleting snapshots
`devy snapshot list` SHALL print one line per snapshot in `.devy/snapshots/`, newest first. Each line SHALL show the name, creation time, services and total size. When there are none, it SHALL print `○ no snapshots`. Directories without a readable manifest SHALL be listed with `(invalid)` instead of their details.

`devy snapshot delete <name>` SHALL remove `.devy/snapshots/<name>/` and print `✓ snapshot '<name>' deleted`. It SHALL fail with the unknown-snapshot message when `<name>` doesn't exist.

#### Scenario: List snapshots
- **WHEN** snapshots `base` and `after-migrate` exist
- **THEN** `devy snapshot list` prints both, with `after-migrate` first if it is newer

#### Scenario: Delete snapshot
- **WHEN** the user runs `devy snapshot delete base`
- **THEN** `.devy/snapshots/base/` no longer exists

### Requirement: Snapshot commands are serialized with up
`devy snapshot save`, `restore` and `delete` SHALL take the same exclusive advisory lock on `<project root>/.devy-lock` that `devy up` takes. Saving or restoring therefore cannot interleave with `devy up`, and two snapshot commands in the same project cannot interleave with each other.

#### Scenario: Save waits for up
- **WHEN** `devy up` is running and the user runs `devy snapshot save base` in the same project
- **THEN** the save starts only after `devy up` finishes
