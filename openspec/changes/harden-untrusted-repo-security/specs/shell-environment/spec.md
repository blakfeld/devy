# Spec Delta

## MODIFIED Requirements

### Requirement: Shadowenv installation and trust
When there is environment content to write and `shadowenv` is not found outside the project (as defined in filesystem-safety), `devy up` SHALL install `shadowenv` through the active package manager. After writing the file, devy SHALL run `shadowenv trust` in the project root only when both of these hold, and MUST fail if trusting fails:
- the project is trusted under project-trust
- `.shadowenv.d/` is a real directory, not tracked by git, that contains no entry other than `500_devy.lisp`

devy SHALL locate the binary outside the project, or in the verified project nix profile. When `.shadowenv.d/` contains other entries, devy SHALL fail with `.shadowenv.d contains files devy did not write (<names>); remove them or trust the directory yourself with shadowenv trust`.

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
