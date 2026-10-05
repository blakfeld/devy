# Spec Delta

## MODIFIED Requirements

### Requirement: Shadowenv installation and trust
When there is environment content to write and `shadowenv` is not found outside the project (as defined in filesystem-safety), `devy up` SHALL install `shadowenv` through the active package manager. After writing the file, devy SHALL run `shadowenv trust` in the project root only when both of these hold, and MUST fail if trusting fails:
- the project is trusted under project-trust
- `.shadowenv.d/` is a real directory, not tracked by git, that contains no entry shadowenv could evaluate other than `500_devy.lisp`: every other entry is a regular file whose name does not end in `.lisp` (compared case-insensitively), such as the `.gitignore` and `.trust-<fingerprint>` files `shadowenv trust` writes itself and the `.error-<n>-<shell pid>` files shadowenv's hook writes while the directory is untrusted

devy SHALL locate the binary outside the project, or in the verified project nix profile. devy SHALL check the directory before writing `500_devy.lisp`. When `.shadowenv.d/` contains a directory, a symlink or another `*.lisp` file, devy SHALL fail with `.shadowenv.d contains files devy did not write (<names>); devy removed shadowenv's trust for this project; review and remove them, then run devy allow and devy up`. Before failing on such an entry, or whenever the managed-path check of filesystem-safety refuses a managed directory (for example a `.shadowenv.d/` tracked by git), whichever command ran it (`devy up`, `devy exec`, or a service started under nix by `devy start`/`restart`), devy SHALL remove shadowenv's trust files (`.shadowenv.d/.trust-*`, only when `.shadowenv.d/` is a real directory) so a signature from an earlier `devy up` stops loading the directory.

The first line of every `500_devy.lisp` devy writes SHALL be the Lisp comment `; devy-env <nonce>`, where `<nonce>` is 32 lowercase hex digits drawn at random for each write, and the file SHALL end with a newline. Before putting the file in place, devy SHALL write an exact copy of it to `<nonce>.lisp` in the per-user directory `$XDG_STATE_HOME/devy/shadowenv/` (default `~/.local/state/devy/shadowenv/`; `%LOCALAPPDATA%\devy\shadowenv\` on Windows), created with mode 0700 beside the trust store and refused, like it, inside the project; failing to write the copy SHALL fail the write. After replacing the file, devy SHALL remove the copy the replaced file's first line named, but only when that copy is byte for byte the replaced file, so a `500_devy.lisp` copied from another project (or naming its nonce) never deletes that project's copy. `devy allow --revoke` SHALL likewise remove the copy of the project's `500_devy.lisp` (only when it is that file's copy), so the shell hook stops accepting the file until the next `devy up`. The shell hook (shell-integration) and the trust check (project-trust) treat a `500_devy.lisp` as devy's only when it is a regular file identical to the copy its first line names: repository content cannot write that copy, nor learn a nonce generated on the user's machine, so a pulled or replaced `500_devy.lisp` never matches.

The errors SHALL be:
- `Failed to configure environment variables: shadowenv trust failed` when `shadowenv trust` exits non-zero
- `Failed to configure environment variables: Failed to run shadowenv trust` when the binary cannot be started
- `Failed to install shadowenv` when installing shadowenv fails; `devy up` SHALL fail with this error

#### Scenario: Shadowenv installed via nix
- **WHEN** shadowenv is not found outside the project and the backend is nix
- **THEN** devy installs `shadowenv` into the project nix profile, verifies the profile, and runs `<project_root>/.devy/nix-profile/bin/shadowenv trust`

#### Scenario: Committed lisp file
- **WHEN** the repo commits `.shadowenv.d/000_evil.lisp` and the user runs `devy up`
- **THEN** devy does not run `shadowenv trust`, writes no environment, and exits 1

#### Scenario: Environment file and its copy
- **WHEN** `devy up` writes `.shadowenv.d/500_devy.lisp`
- **THEN** its first line is `; devy-env <nonce>` and `~/.local/state/devy/shadowenv/<nonce>.lisp` holds the same bytes
- **AND** the next write uses a new nonce and removes the old copy

#### Scenario: Another project's copy is kept
- **WHEN** a project's `500_devy.lisp` names the nonce of another project's copy but differs from it, and `devy up` rewrites it
- **THEN** the other project's copy is left in place

#### Scenario: Managed-path refusal outside up
- **WHEN** an earlier `devy up` trusted `.shadowenv.d`, a pull makes `.devy` a symlink, and the user runs `devy exec make`
- **THEN** devy refuses, and `.shadowenv.d/.trust-*` is removed
