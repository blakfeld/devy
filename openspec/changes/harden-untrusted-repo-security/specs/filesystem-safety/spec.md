# Spec Delta

## Purpose

Ensures that files and directories committed to a repository, or planted in shared temporary locations by other local users, cannot redirect devy's writes, executables or sockets outside the places devy intends.

## ADDED Requirements

### Requirement: Writes do not follow symlinks
Every file devy creates or replaces inside the project, under `.devy/`, under `.shadowenv.d/`, or in a temporary directory SHALL be written by creating a new temporary file in the destination directory with exclusive creation and without following symlinks, then renaming it over the destination. If the destination path itself, or any directory devy creates on the way to it, is a symlink, devy SHALL fail with `refusing to write <path>: it is a symbolic link` and leave the link target untouched. This covers at least:
- `devy.lock` and `devy.yml`
- `.devy-lock`
- the failure record
- stamp files
- `.shadowenv.d/500_devy.lisp`
- generated service configs (`my.cnf`, `vault.hcl`, `nginx.conf`, `server.properties`, RabbitMQ files)
- export output

#### Scenario: Committed symlink stamp
- **WHEN** the repo commits `.devy_bun_stamp` as a symlink to `~/.zshrc` and `devy up` runs
- **THEN** devy fails with the refusing-to-write error and `~/.zshrc` is unchanged

#### Scenario: Pre-planted temporary file names
- **WHEN** the repo commits symlinks named `devy.lock.<n>.tmp` for many values of `<n>`
- **THEN** `devy up` writes `devy.lock` normally and no symlink target is modified

### Requirement: devy-managed directories must be real and untracked
Before writing into or executing from `.devy/`, `.shadowenv.d/`, the Python virtualenv directory, or `.devy/nix-profile`, devy SHALL verify that each is either absent or a real directory owned by the current user, and, when the project is a git repository, that no file under it is tracked by git. Otherwise devy SHALL fail with `<path> is <a symbolic link|tracked by git>; devy will not use it — remove it from the repository`. `.devy/nix-profile` SHALL additionally be a symlink whose resolved target lies under `/nix/store` before devy uses it or prepends its `bin` to PATH.

#### Scenario: Committed .venv binaries
- **WHEN** the repo commits `.venv/bin/git` and declares `python`
- **THEN** `devy up` fails naming `.venv` as tracked by git and writes no shadowenv file

#### Scenario: Fake nix profile
- **WHEN** `.devy/nix-profile` is a directory in the repo instead of a link into `/nix/store`
- **THEN** devy refuses to use it

### Requirement: Private temporary directories
Directories devy creates outside the project for its own use SHALL live under a per-user base (`$XDG_RUNTIME_DIR` when set, otherwise `$TMPDIR`, otherwise the system temp directory), SHALL be created with mode 0700 and exclusive creation, and SHALL be rejected when they already exist and are not a directory owned by the current user with mode 0700. This covers the Unix socket fallback directory, the AI working directory, and nix service log files. The AI working directory SHALL use a random name and be removed after use.

#### Scenario: Pre-created socket directory
- **WHEN** another local user has created the socket fallback directory devy would use
- **THEN** devy refuses to start the service and reports that the directory is not owned by the current user

### Requirement: Executables resolved outside the project
devy SHALL locate `shadowenv`, `nix`, `brew`, `sudo`, `apt-get`, `dpkg-query` and `claude` while ignoring any `PATH` entry inside the project root, and SHALL run `sudo` and `apt-get` by absolute path (`/usr/bin/sudo`, `/usr/bin/apt-get`). The only project-local location devy MAY use is `<project_root>/.devy/nix-profile/bin` after it passes the nix-profile check above (for example, to run a `shadowenv` installed into the project profile). devy SHALL NOT otherwise fall back to a binary inside the project directory.

#### Scenario: Planted sudo
- **WHEN** an activated project environment puts `<project_root>/bin` first on PATH and it contains `sudo`
- **THEN** devy's apt backend runs `/usr/bin/sudo`
