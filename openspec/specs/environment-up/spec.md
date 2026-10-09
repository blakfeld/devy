# environment-up Specification

## Purpose
Defines `devy up`, which brings a project's environment up from `devy.yml`. It installs dependencies, writes the lock and the shell environment, starts services, and runs the up hooks in a fixed order.

## Requirements

### Requirement: Serialized execution
`devy up` SHALL take an exclusive advisory lock on `<project root>/.devy-lock` (creating the file if needed) after loading `devy.yml` and selecting the package manager, and before printing the header or running any hook. A concurrent `devy up` in the same project SHALL block until the first one finishes. If the guard file cannot be opened or locked, the command SHALL fail with `Failed to open process guard file` or `Failed to acquire process lock (is another devy process running?)`.

#### Scenario: Concurrent runs queue
- **WHEN** two `devy up` processes start in the same project at the same time
- **THEN** the second one waits until the first releases the lock, then runs

### Requirement: Up phase ordering
`devy up` SHALL run these steps in order:
1. Print the header `devy up · <name>`, where `<name>` is the `name` from `devy.yml` with control characters stripped, or `project` when it is unset.
2. Check that devy-managed paths (`.devy/`, `.shadowenv.d/`, the virtualenv) are not symlinks or tracked by git, as defined in filesystem-safety.
3. Run the `before_up` hook.
4. Ensure the package manager is available.
5. Load `devy.lock`. This happens even with `--update`.
6. Validate each dependency's configuration.
7. Pin versions from the lock (unless `--update`), resolve service ports, and fail on port conflicts.
8. Install dependencies (phase 1).
9. Write `devy.lock`.
10. Write the shell environment.
11. Report orphaned lock entries.
12. Start services (phase 2).
13. Run the `after_up` hook.
14. Print `✓ <name> is ready`.

`devy up` SHALL NOT ask the user to allow the project or check any trust record: it runs the project's hooks and installs as `devy.yml` declares them.

A failure in any step SHALL abort the remaining steps. Each hook run SHALL be preceded by a `Hooks` header.

#### Scenario: Failing before_up hook
- **WHEN** the `before_up` hook exits non-zero
- **THEN** no dependency is installed and devy exits 1

#### Scenario: Successful run
- **WHEN** every step succeeds
- **THEN** the final line is `✓ <name> is ready`

#### Scenario: Untrusted project stops before hooks
- **WHEN** a cloned repository commits `.shadowenv.d/` and the user runs `devy up`
- **THEN** devy fails naming `.shadowenv.d` as tracked by git, the `before_up` hook does not run, and nothing is installed

#### Scenario: No trust prompt
- **WHEN** the user runs `devy up` for the first time in a freshly cloned project, with or without a terminal
- **THEN** devy prints no allow prompt and runs the `before_up` hook

### Requirement: Lock write
`devy up` SHALL build the lock from every dependency after phase 1, before the environment is written and before any service starts. It SHALL do this even when there are no dependencies, so the first `devy up` in a project with none creates a `devy.lock` with an empty `dependencies` map. When the new lock equals the existing one, devy SHALL leave the file untouched. Otherwise it SHALL write it and print `✓ Lock file written to devy.lock`.

#### Scenario: Service failure leaves lock current
- **WHEN** every dependency installs but a service fails to start
- **THEN** `devy.lock` already reflects the installed dependencies

### Requirement: Orphan reporting
After the environment step and before services start, `devy up` SHALL print `'<name>' was in devy.lock but is no longer in devy.yml — removed` for each entry in the previous lock that is no longer in `devy.yml`. This SHALL also happen with `--update`.

#### Scenario: Dependency removed from devy.yml
- **WHEN** `devy.lock` lists `redis` and `devy.yml` no longer does
- **THEN** after the environment step `devy up` prints `'redis' was in devy.lock but is no longer in devy.yml — removed` and does not uninstall it

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

### Requirement: Dependency configuration validation
`devy up` SHALL validate every dependency's configuration with the active package manager before installing anything. On failure it SHALL report `<dep>: config validation failed` along with the cause.

#### Scenario: Invalid tap
- **WHEN** a dependency has `tap: "not a tap"` and brew is the package manager
- **THEN** `devy up` fails with `<dep>: config validation failed` before any install

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

### Requirement: Environment composition
`devy up` SHALL build the project environment from three sources: the variables and PATH entries each module contributes, `<NAME>_HOST`/`<NAME>_PORT` variables for each service, and the `environment` map in `devy.yml`. A user-defined `environment` value SHALL override a module-provided variable with the same name. The package manager's own PATH entries SHALL come before module PATH entries, and per-package directories (brew's `<brew prefix>/opt/<formula>/bin`) SHALL come after module PATH entries; a directory listed twice is kept only at its first position. The result SHALL be written through the shell environment manager. When there is anything to write and shadowenv is not available, devy SHALL first install it through the package manager, printing `→ Installing shadowenv` and failing with `Failed to install shadowenv` if that fails. After writing, devy SHALL print `✓ Environment configured (<N> variables)`, where PATH entries are not counted, followed by the hint `Activate with: <command>`, which loads devy's shell integration: `eval "$(devy hook <shell>)"` when `<shell>` is zsh or bash, and `devy hook fish | source` for fish. `<shell>` is the basename of `$SHELL` if it is zsh, bash or fish, and otherwise `zsh`.

#### Scenario: User value wins
- **WHEN** the postgresql module sets `DATABASE_URL` and `devy.yml` `environment` also sets `DATABASE_URL`
- **THEN** the written environment uses the value from `devy.yml`

#### Scenario: Environment cleared when nothing remains
- **WHEN** no PATH entries are contributed (apt or WinGet, or brew with no installed formula's `opt/<formula>/bin` directory), a previous run wrote environment variables, and the dependencies and `environment` that produced them have since been removed
- **THEN** `devy up` clears the devy environment file and prints `✓ Environment configuration cleared`

#### Scenario: Brew formula directories are written
- **WHEN** brew is the package manager, the project declares `jq` with no `environment`, and `<brew prefix>/opt/jq/bin` exists
- **THEN** `devy up` writes the environment file with that PATH entry and prints `✓ Environment configured (0 variables)`

#### Scenario: Nix always writes the environment
- **WHEN** nix is the package manager and the project has no dependencies and no `environment`
- **THEN** `devy up` still writes the environment file with the `.devy/nix-profile/bin` PATH entry, prints `✓ Environment configured (0 variables)`, and never clears the file

### Requirement: Service start phase
After the lock is written, `devy up` SHALL start each service dependency in declaration order and then wait for it to become ready:
- If the service is already running, it SHALL print `○ <dep> service already running`.
- If starting fails, it SHALL fail with `Failed to start <dep> service`.
- If the health check does not pass in time, it SHALL print the warning `<dep> is not yet responding to health checks — verify manually: <cause>`, and SHALL NOT fail the run.

Because a start failure aborts the run, any later services and the `after_up` hook do not run.

#### Scenario: Health check times out
- **WHEN** a service starts but never passes its health check
- **THEN** `devy up` warns that the service is not yet responding, then continues and runs `after_up`

#### Scenario: Service already running
- **WHEN** redis is already running
- **THEN** `devy up` does not start it again

### Requirement: Failure record
When `devy up` (without `--dry-run`) fails after `devy.yml` has been located, devy SHALL write a failure record to `<project root>/.devy/last-up-failure.json`, creating `.devy/` if needed. The record SHALL contain:
- the full error chain, as printed on the `error:` line
- the `devy up` step that failed, when known
- the dependency being processed, when the failure is tied to one
- the platform, the selected backend and the devy version
- a UTC timestamp

Each failure SHALL replace any previous record. A `devy up` that completes successfully SHALL delete an existing record. Failing to write or delete the record SHALL NOT change `devy up`'s output or exit code beyond a single warning. Writing the record SHALL NOT make any network request. On Unix the file SHALL be readable and writable only by its owner.

#### Scenario: Failed service readiness is recorded
- **WHEN** `devy up` fails because `postgres` does not become ready
- **THEN** `.devy/last-up-failure.json` exists and names `postgres`, the service start step and the error chain

#### Scenario: Success clears the record
- **WHEN** a failure record exists and the next `devy up` prints `✓ <name> is ready`
- **THEN** `.devy/last-up-failure.json` no longer exists

#### Scenario: No project, no record
- **WHEN** `devy up` fails because no `devy.yml` is found
- **THEN** no failure record is written anywhere

#### Scenario: Dry run never records
- **WHEN** `devy up --dry-run` finds issues and exits 1
- **THEN** no failure record is written and an existing record is left unchanged

### Requirement: Doctor hint on failure
When `devy up` fails and a failure record was written, devy SHALL print `  · run devy doctor to diagnose this failure` to stderr after the `error:` line. The exit code SHALL remain 1. Printing the hint SHALL NOT make any network request or load AI configuration.

#### Scenario: Hint follows the error
- **WHEN** `devy up` fails with a hook exiting non-zero
- **THEN** stderr shows the `error:` line followed by the `devy doctor` hint, and devy exits 1

#### Scenario: Offline failure stays offline
- **WHEN** `devy up` fails on a machine with no network access and no AI configuration
- **THEN** the hint is printed and no network request is attempted
