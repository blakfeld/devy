# Spec Delta

## MODIFIED Requirements

### Requirement: Lock file loading
A missing `devy.lock` SHALL be treated as having no lock, not as an error. A lock whose `version` is missing SHALL be treated as version 1. Any other version SHALL be rejected with `devy.lock has unsupported format version N. Only version 1 is supported.`. Invalid YAML, or a lock without a `dependencies` key, SHALL fail with `Failed to parse devy.lock`. A lock larger than 1 MiB SHALL be rejected, without being read further, with an error saying it is larger than the 1 MiB limit. A lock containing any YAML anchor (`&name`) or alias (`*name`) SHALL be rejected before it is deserialized, with `Failed to parse devy.lock: devy.lock: YAML anchors and aliases are not supported`, naming the line; devy never writes either, and `*` or `&` inside a quoted value or a comment is not an anchor or alias. In `devy up` these errors SHALL be prefixed with `Failed to read devy.lock: `. `devy up` SHALL load the lock after the `before_up` hook and the package manager availability check, and SHALL load it even with `--update`, so an invalid lock also makes `devy up --update` fail.

Each entry SHALL be validated on load, and an invalid entry SHALL fail with `Failed to parse devy.lock: <dep>: invalid <field> <value>`:
- `resolved_version` SHALL satisfy the project-config version rule.
- `image_digest`, when present, SHALL be `<repository>@sha256:<64 lowercase hex>`.
- `assigned_port`, when present, SHALL be between 1 and 65535.

A locked `assigned_port` below 1024 SHALL be rejected with the same error wherever devy would use it, that is when the backend applies ports for that dependency and `devy.yml` sets no explicit port for it. An explicit `devy.yml` port overrides the lock, and orphan entries are not used, so a privileged value there (for example one recorded from an earlier explicit `port:`) SHALL NOT be an error. devy SHALL NOT write a `resolved_version` or `image_digest` that fails these rules; it omits the value with a warning.

#### Scenario: No lock yet
- **WHEN** `devy up` runs in a project without `devy.lock`
- **THEN** it proceeds without pinned versions

#### Scenario: Unsupported version
- **WHEN** `devy.lock` declares `version: 2`
- **THEN** `devy up` fails with `error: Failed to read devy.lock: devy.lock has unsupported format version 2. Only version 1 is supported.`

#### Scenario: Invalid lock with --update
- **WHEN** `devy.lock` is not valid YAML and the user runs `devy up --update`
- **THEN** `devy up` fails with `Failed to read devy.lock: Failed to parse devy.lock`

#### Scenario: Alias amplification in the lock
- **WHEN** `devy.lock` contains one anchored list and thousands of aliases to it
- **THEN** devy fails promptly with the anchors-and-aliases error, without expanding them

#### Scenario: Injected version in the lock
- **WHEN** `devy.lock` has `resolved_version: "1.0;id"` for `deno`
- **THEN** `devy up` fails with an invalid `resolved_version` error before installing any dependency

#### Scenario: Privileged port in the lock
- **WHEN** `devy.lock` has `assigned_port: 22` for `redis`, the backend applies ports (Nix or a docker-managed service) and `devy.yml` sets no `port` for `redis`
- **THEN** `devy up` fails with an invalid `assigned_port` error before installing any dependency

#### Scenario: Privileged port from an earlier explicit port
- **WHEN** `devy.lock` has `assigned_port: 543` for `postgresql` and `devy.yml` now sets `port: 5433`
- **THEN** `devy up` uses port 5433 and does not fail

### Requirement: Version pinning from the lock
During `devy up` without `--update`, a dependency that has no explicit `version` in `devy.yml` SHALL be installed at the lock's `resolved_version` for its canonical name. An explicit `version` in `devy.yml` SHALL always take precedence over the lock. With `--update`, locked versions SHALL be ignored and `→ Ignoring devy.lock (--update)` SHALL be printed when a lock exists. Locked `assigned_port` values SHALL still be reused with `--update`. For docker-managed services, without `--update`, devy SHALL use the locked `image_digest` as the image reference when the dependency's image repository and tag are unchanged; when they changed, or with `--update`, devy SHALL resolve the tag again and record the newly pulled digest. A module's built-in digest pin (see docker-services) SHALL take precedence over the locked `image_digest`, with or without `--update`; it changes only with a devy release.

#### Scenario: Locked version applied
- **WHEN** `devy.lock` records node `20.11.0` and `devy.yml` lists `node` with no version
- **THEN** `devy up` asks the package manager for node version `20.11.0`

#### Scenario: Explicit version wins
- **WHEN** `devy.yml` pins node to `22` and the lock records `20.11.0`
- **THEN** `devy up` asks the package manager for node version `22`

#### Scenario: Update ignores locked versions
- **WHEN** the user runs `devy up --update`
- **THEN** locked versions are not applied, and the newly resolved versions are written to the lock

#### Scenario: Update keeps assigned ports
- **WHEN** `devy.lock` records `assigned_port: 51234` for `redis`, `devy.yml` sets no port, and the user runs `devy up --update`
- **THEN** redis keeps port 51234

#### Scenario: Teammate gets the same image
- **WHEN** `devy.lock` records `image_digest: redis@sha256:abc…` for docker-managed `redis:7` and a teammate runs `devy up`
- **THEN** devy pulls and runs `redis@sha256:abc…` even if the `redis:7` tag has moved

#### Scenario: Update re-resolves the tag
- **WHEN** the user runs `devy up --update` for docker-managed `redis:7`
- **THEN** devy pulls `redis:7` and records its current digest in `devy.lock`

#### Scenario: Update keeps a built-in digest pin
- **WHEN** docker-managed `minio` sets no `version` or `image`, `devy.lock` records an older `pgsty/silo` digest, and the user runs `devy up --update`
- **THEN** devy runs and records the built-in `pgsty/silo@sha256:…` pin, not a newly resolved digest

### Requirement: Atomic lock writes
devy SHALL write `devy.lock` atomically: it creates a new temporary file in the same directory exclusively and without following symlinks, then renames it over `devy.lock`. If `devy.lock` is a symlink, the write SHALL fail as defined in filesystem-safety. No temporary file SHALL remain after a successful write.

#### Scenario: Successful write
- **WHEN** `devy up` writes a new lock
- **THEN** `devy.lock` contains the new content and no `devy.lock.*.tmp` file remains

#### Scenario: Pre-planted temporary symlink
- **WHEN** a symlink exists at a temporary file name devy might choose
- **THEN** devy does not write through it and the lock write still succeeds
