## MODIFIED Requirements

### Requirement: Package manager availability
Before installing anything, `devy up` SHALL check that the selected package manager is available, but only when at least one dependency is installed through it or shadowenv must be installed through it.
- If the package manager is missing, `devy up --bootstrap` SHALL install Nix or Homebrew.
- For apt and WinGet, bootstrap cannot install anything and SHALL fail with a message asking the user to install it manually.
- Without `--bootstrap`, devy SHALL fail with `Failed to ensure <pm> is available: <pm> is not installed. Re-run with --bootstrap to install automatically.` For nix and brew, the message also includes a manual install URL.

When any dependency is docker-managed, `devy up` SHALL also check container runtime availability as defined in the docker-services spec.

#### Scenario: Missing package manager without bootstrap
- **WHEN** Nix is the selected package manager, it is not installed, a dependency needs it, and the user runs `devy up`
- **THEN** devy fails with a message suggesting `--bootstrap`, and nothing is installed

#### Scenario: Bootstrap installs the package manager
- **WHEN** Nix is not installed and the user runs `devy up --bootstrap`
- **THEN** devy installs Nix, then continues with the dependencies

#### Scenario: Bootstrap cannot install apt
- **WHEN** apt is the selected package manager, `apt-get` is not on PATH, and the user runs `devy up --bootstrap`
- **THEN** devy fails with `apt-get is not available; please ensure Ubuntu/Debian is properly installed`

#### Scenario: Docker-only project needs no package manager
- **WHEN** `devy.yml` sets `service_manager: docker`, declares only `redis` and `postgresql`, shadowenv is already on PATH, and Nix is not installed
- **THEN** `devy up` completes without checking for or installing Nix

### Requirement: Dependency installation phase
For each dependency, in declaration order, `devy up` SHALL do the following. In these messages, `<dep>` is shown as `<name>@<version>` when a version is set in `devy.yml` or pinned from `devy.lock`, and as `<name>` otherwise.
- If the dependency is already installed, print `○ <dep> already installed (via <pm>)`. For docker-managed services it SHALL print `○ <name> image present (docker)`.
- Otherwise, install it, printing `→ Installing <dep>` and then `✓ Installed <dep>`. Docker-managed services are installed by pulling their image, printing `→ Pulling <reference>` and then `✓ Pulled <reference>`.
- Only when the dependency was freshly installed, run its `after_install` command (if any) in the project root, using the dependency's `shell` or the default shell. Before running it, print the warning `<name>: running after_install: <cmd>`. A non-zero exit SHALL abort the run.
- Run the module's post-setup step on every run, whether or not anything was installed. A failure SHALL abort with `post_setup failed for <name>`. Post-setup steps that write package-manager service config SHALL be skipped for docker-managed services.

#### Scenario: Already installed
- **WHEN** a dependency is already installed
- **THEN** devy skips installing it and does not run its `after_install`, but still runs its post-setup step

#### Scenario: after_install runs in the project root
- **WHEN** a freshly installed dependency has `after_install: "pwd > where.txt"` and `devy up` is run from a subdirectory
- **THEN** `where.txt` is created in the project root

#### Scenario: Install failure
- **WHEN** the package manager fails to install a dependency
- **THEN** devy fails with `Failed to install <dep>` (e.g. `Failed to install node@20.11.0` when the version is pinned from the lock) and no services are started

#### Scenario: Docker service is pulled, not installed
- **WHEN** `redis` is docker-managed and its image is not present locally
- **THEN** devy pulls the image and does not ask the package manager to install `redis`
