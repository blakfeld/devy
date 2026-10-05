# filesystem-safety Specification

## Purpose
Ensures that files and directories committed to a repository, or planted in shared temporary locations by other local users, cannot redirect devy's writes, executables or sockets outside the places devy intends.

## Requirements

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
Before writing into or executing from `.devy/`, `.shadowenv.d/`, the Python virtualenv directory, or `.devy/nix-profile`, devy SHALL verify that each is either absent or a real directory owned by the current user, that it is not a git repository of its own (holds no `.git` entry), and that no file under it is tracked by git: by the project's repository, or, when the directory lies inside a nested repository (a submodule or nested checkout, whose files the project's own index does not list), by that nested repository. Otherwise devy SHALL fail with `<path> is <a symbolic link|a git repository of its own|tracked by git>; devy will not use it — remove it from the repository`. When git is found but cannot list the index (for example a corrupt index or a repository git refuses as unsafe), devy SHALL fail with an error saying it could not check which files git tracks (naming `git config --global --add safe.directory <path>`, with the repository path git reports, when git reports dubious ownership), rather than skip the check; a `.git` that git does not recognise as a repository at all (`fatal: not a git repository`) counts as no repository. The git call SHALL receive no `GIT_*` variable from devy's environment (only those devy sets itself), and the `git` binary SHALL be found outside the project root even when the check runs inside a nested repository. When git does not recognise a nested repository's `.git`, the project's own repository SHALL be asked instead. This check SHALL run wherever devy builds the project environment, so `devy exec` (and every other consumer of that environment) refuses the same directories as `devy up`. `.devy/nix-profile` SHALL additionally be a symlink whose resolved target lies under `/nix/store` before devy uses it or prepends its `bin` to PATH.

#### Scenario: Committed .venv binaries
- **WHEN** the repo commits `.venv/bin/git` and declares `python`
- **THEN** `devy up` fails naming `.venv` as tracked by git and writes no shadowenv file

#### Scenario: Committed venv tool under devy exec
- **WHEN** a repo commits `.venv/bin/sudo`, declares `python`, and the user runs `devy exec sudo`
- **THEN** devy fails naming `.venv` as tracked by git and runs nothing

#### Scenario: Fake nix profile
- **WHEN** `.devy/nix-profile` is a directory in the repo instead of a link into `/nix/store`
- **THEN** devy refuses to use it

#### Scenario: Committed .shadowenv.d symlink
- **WHEN** the repo commits `.shadowenv.d` as a symlink to another directory (for example another project's trusted `.shadowenv.d`)
- **THEN** `devy up` and `devy exec` fail naming `.shadowenv.d` as a symbolic link; writing the shadowenv file (`Shadowenv::setup`, which also refuses to list a symlinked directory for foreign entries) fails with `refusing to write <path>: it is a symbolic link`; the shadowenv trust removal that follows the refusal reads only a real `.shadowenv.d` (never following the link); and nothing in the target directory is listed, written, trusted or removed

### Requirement: Private temporary directories
Directories devy creates outside the project for its own use SHALL live under a per-user base (`$XDG_RUNTIME_DIR` when set, otherwise `$TMPDIR`, otherwise the system temp directory), SHALL be created with mode 0700 and exclusive creation, and SHALL be rejected when they already exist and are not a directory owned by the current user with mode 0700. This covers the Unix socket fallback directory, the AI working directory, and nix service log files. The AI working directory SHALL use a random name and be removed after use.

#### Scenario: Pre-created socket directory
- **WHEN** another local user has created the socket fallback directory devy would use
- **THEN** devy refuses to start the service and reports that the directory is not owned by the current user

### Requirement: Executables resolved outside the project
devy SHALL locate `shadowenv`, `nix`, `brew`, `sudo`, `apt-get`, `dpkg-query`, `claude`, `rbenv`, `rustup`, `rustc`, `winget`, `docker`, `podman`, `systemctl` and `journalctl` while ignoring any `PATH` entry inside the project root (falling back to standard system directories such as `/usr/bin` for the service tools, and never to a bare name resolved against the full `PATH`), SHALL run `sudo` and `apt-get` by absolute path (`/usr/bin/sudo`, `/usr/bin/apt-get`), the `systemctl` it runs under sudo as `/usr/bin/systemctl` (or `/bin/systemctl`), and `launchctl` as `/bin/launchctl`. The rust version probe SHALL also drop `RUSTUP_TOOLCHAIN`. `rbenv` (for status checks) and `rustc --version` SHALL run with `/` as their working directory, so a project's `.ruby-version` hooks or `rust-toolchain(.toml)` path toolchain can't substitute a binary. The only project-local location devy MAY use is `<project_root>/.devy/nix-profile/bin` after it passes the nix-profile check above (for example, to run a `shadowenv` installed into the project profile). devy SHALL NOT otherwise fall back to a binary inside the project directory.

#### Scenario: Planted docker
- **WHEN** an activated project environment puts `<project_root>/bin` first on PATH and it contains `docker`, `rbenv` or `systemctl`
- **THEN** `devy status`, `devy check` and `devy up` never run the planted binary

#### Scenario: Planted sudo
- **WHEN** an activated project environment puts `<project_root>/bin` first on PATH and it contains `sudo`
- **THEN** devy's apt backend runs `/usr/bin/sudo`
