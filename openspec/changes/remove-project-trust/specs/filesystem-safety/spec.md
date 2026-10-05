# filesystem-safety delta

## MODIFIED Requirements

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
