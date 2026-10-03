# Spec Delta

## MODIFIED Requirements

### Requirement: Lock file loading
A missing `devy.lock` SHALL be treated as having no lock, not as an error. A lock whose `version` is missing SHALL be treated as version 1. Any other version SHALL be rejected with `devy.lock has unsupported format version N. Only version 1 is supported.`. Invalid YAML, or a lock without a `dependencies` key, SHALL fail with `Failed to parse devy.lock`. In `devy up` these errors SHALL be prefixed with `Failed to read devy.lock: `. `devy up` SHALL load the lock after the `before_up` hook and the package manager availability check, and SHALL load it even with `--update`, so an invalid lock also makes `devy up --update` fail.

Each entry SHALL be validated on load, and an invalid entry SHALL fail with `Failed to parse devy.lock: <dep>: invalid <field> <value>`:
- `resolved_version` SHALL satisfy the project-config version rule.
- `image_digest`, when present, SHALL be `<repository>@sha256:<64 lowercase hex>`.
- `assigned_port`, when present, SHALL be between 1024 and 65535, unless it equals the port set explicitly for that dependency in `devy.yml`.

#### Scenario: No lock yet
- **WHEN** `devy up` runs in a project without `devy.lock`
- **THEN** it proceeds without pinned versions

#### Scenario: Unsupported version
- **WHEN** `devy.lock` declares `version: 2`
- **THEN** `devy up` fails with `error: Failed to read devy.lock: devy.lock has unsupported format version 2. Only version 1 is supported.`

#### Scenario: Invalid lock with --update
- **WHEN** `devy.lock` is not valid YAML and the user runs `devy up --update`
- **THEN** `devy up` fails with `Failed to read devy.lock: Failed to parse devy.lock`

#### Scenario: Injected version in the lock
- **WHEN** `devy.lock` has `resolved_version: "1.0;id"` for `deno`
- **THEN** `devy up` fails with an invalid `resolved_version` error before installing anything

#### Scenario: Privileged port in the lock
- **WHEN** `devy.lock` has `assigned_port: 22` for `redis`
- **THEN** `devy up` fails with an invalid `assigned_port` error

### Requirement: Atomic lock writes
devy SHALL write `devy.lock` atomically: it creates a new temporary file in the same directory exclusively and without following symlinks, then renames it over `devy.lock`. If `devy.lock` is a symlink, the write SHALL fail as defined in filesystem-safety. No temporary file SHALL remain after a successful write.

#### Scenario: Successful write
- **WHEN** `devy up` writes a new lock
- **THEN** `devy.lock` contains the new content and no `devy.lock.*.tmp` file remains

#### Scenario: Pre-planted temporary symlink
- **WHEN** a symlink exists at a temporary file name devy might choose
- **THEN** devy does not write through it and the lock write still succeeds
