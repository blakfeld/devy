# devy

[![crates.io](https://img.shields.io/crates/v/devy.svg)](https://crates.io/crates/devy)
[![CI](https://github.com/blakfeld/envy/actions/workflows/ci.yml/badge.svg)](https://github.com/blakfeld/envy/actions/workflows/ci.yml)

A declarative developer environment manager. Define your project's dependencies, services, environment variables, and runnable commands in a single `devy.yml` file — then run `devy up` to get everything running.

## What it does

- **Installs dependencies** via Nix by default — packages land in `.devy/nix-profile` inside your project, not your global environment
- **Starts services** like MySQL and Redis, and waits for them to be healthy
- **Sets environment variables** persistently in your shell session via [shadowenv](https://shopify.github.io/shadowenv/)
- **Locks versions** in `devy.lock` so teammates get the same setup
- **Runs project commands** defined in `devy.yml` (like `npm run dev`, `make test`, etc.)

## Platform support

| Platform | Default package manager | Service management |
|---|---|---|
| macOS | [Nix](https://nixos.org) (project-local profile) | launchd (`launchctl`) |
| Ubuntu / Debian | [Nix](https://nixos.org) (project-local profile) | systemd user units (`systemctl --user`) |
| Windows 10/11 | [WinGet](https://learn.microsoft.com/en-us/windows/package-manager/winget/) | `net start` / `sc` |

On macOS and Linux you can opt into your system package manager instead by setting `package_manager: brew` or `package_manager: apt` in `devy.yml`. See [Choosing a package manager](#choosing-a-package-manager).

## Requirements

- A supported platform (see above)
- **macOS / Linux:** Nix is required. Run `devy up --bootstrap` to install it automatically via the [Determinate Installer](https://install.determinate.systems), or install Nix manually first.
- **`devy init`, `devy doctor`, `devy ask` and `devy logs --explain`:** the [Claude Code](https://claude.com/claude-code) CLI (`claude`), signed in. Only these commands use it; `devy init --detect` works without it, and `devy doctor` falls back to devy's own checks.

## Installation

`devy` is published on [crates.io](https://crates.io/crates/devy). Install with:

```sh
cargo install devy
```

Add the shell hook to your rc file. It activates the environment after `devy up` **and** enables tab-completion for all built-in subcommands and your project's custom commands:

```sh
# ~/.zshrc
eval "$(devy hook zsh)"

# ~/.bashrc
eval "$(devy hook bash)"

# ~/.config/fish/config.fish
devy hook fish | source
```

## Quick start

Draft a config from your project:

```sh
devy init           # Claude drafts devy.yml from your project's files
devy init --detect  # or draft it offline from fixed rules, without Claude
```

Then review `devy.yml`, and bring the environment up:

```sh
devy up --bootstrap   # installs Nix automatically if not already installed
devy up               # if Nix is already installed
```

## devy.yml reference

```yaml
name: my-project

# Package manager to use. Defaults to "nix" on macOS and Linux, "winget" on Windows.
# Options: nix, brew (macOS only), apt (Linux only)
package_manager: nix

# What runs service dependencies (redis, postgresql, …). Defaults to "package":
# the package manager's own service backend. "docker" runs each service as a
# per-project container instead. See "Running services with Docker or Podman".
# Options: package, docker
service_manager: package

# The Docker-compatible CLI used for docker-managed services. Defaults to "docker".
# Options: docker, podman
container_cli: docker

dependencies:
  # Simple form — installs the latest version
  - redis
  - jq

  # Run just this service in a container, from a registry mirror.
  # service_manager (package or docker) overrides the top-level setting for one
  # service; image replaces the default image repository. Both apply only to
  # built-in services.
  - postgres:
      service_manager: docker
      image: registry.corp.example/mirror/postgres

  # Pinned version (under Nix, see "Versions under Nix" below)
  - node:
      version: "22"

  # MySQL with a custom port and extra server flags
  - mysql:
      port: 3307
      cli_args: "--innodb-buffer-pool-size=256M"

  # Node with global npm packages
  - node:
      version: "22"
      global_packages:
        - typescript
        - eslint

  # Rust with a specific toolchain, targets, and components
  - rust:
      toolchain: stable
      targets:
        - wasm32-unknown-unknown
      components:
        - rust-analyzer

environment:
  # devy injects MYSQL_HOST, MYSQL_PORT, REDIS_HOST, REDIS_PORT automatically.
  # Build connection strings from those instead of hardcoding ports.
  DATABASE_URL: "mysql://root@${MYSQL_HOST}:${MYSQL_PORT}/myapp_dev"
  REDIS_URL: "redis://${REDIS_HOST}:${REDIS_PORT}"
  LOG_LEVEL: "debug"

commands:
  # Simple form — runs via `sh -c`
  dev: "npm run dev"

  # Configured form — custom shell and working directory
  migrate:
    cmd: "bundle exec rails db:migrate"
    cwd: ./api
    shell: bash

hooks:
  # Single command
  before_up: "echo 'Starting up…'"

  # Configured command
  after_up:
    cmd: "bundle install"
    shell: bash

  # List of commands — run in order, stops on first failure
  before_down:
    - "echo 'Stopping…'"
    - cmd: "make teardown"
      shell: bash

  after_down: ~
```

## Choosing a package manager

devy defaults to Nix on macOS and Linux. Nix installs packages into a project-local profile at `.devy/nix-profile`, so nothing leaks into your global environment and each project is fully isolated.

To use your system package manager instead, set `package_manager:` in `devy.yml`:

```yaml
package_manager: brew   # macOS only — uses Homebrew
package_manager: apt    # Linux only — uses apt-get (requires sudo)
```

When `package_manager` is omitted or set to `auto`, devy prints a warning and falls back to Nix.

### Without Nix

If Nix isn't allowed where you work, combine your system package manager with docker-managed services. Tools come from Homebrew or apt, and every service runs in a container, so nothing goes through Nix:

```yaml
name: my-project
package_manager: brew      # or apt on Linux
service_manager: docker    # services run as containers
container_cli: docker      # or podman

dependencies:
  - node
  - postgresql
  - redis
```

A project whose only dependencies are docker-managed services needs no package manager at all, as long as shadowenv is already installed. See [Running services with Docker or Podman](#running-services-with-docker-or-podman).

### Versions under Nix

Nix installs one nixpkgs attribute per dependency. A `version:` is honored when it maps to a versioned attribute that nixpkgs carries. devy matches on the major version (major.minor for Python, MySQL and Go), so `22` and `22.11.0` both install `nodejs_22`:

| Module | Supported versions | Attribute | Without a version |
|---|---|---|---|
| `node`, `typescript` | 22, 24 | `nodejs_22` | `nodejs` |
| `python` | 3.11–3.14 | `python312` | `python3` |
| `postgresql` | 14–18 | `postgresql_16` | `postgresql` |
| `mysql` | 8.4 | `mysql84` | `mysql84` |
| `java` | 8, 11, 17, 21, 25 | `jdk21` | `jdk21` |
| `dotnet` | 6–10 | `dotnet-sdk_8` | `dotnet-sdk_8` |
| `go` | 1.26 | `go_1_26` | `go` |

Every other module, and any version outside these lists, installs the unversioned attribute. When that happens for a `version:` written in `devy.yml`, `devy up` and `devy check` warn `<dep>: version <v> is not supported by the nix backend — installing the nixpkgs default`. Versions pinned from `devy.lock` never warn. Modules that install through their own tooling (`rust` via rustup, `ruby` via rbenv, `deno`, `bun`, `gcloud`) honor `version:` themselves. `devy export` uses the same attributes.

**The lock under Nix.** Nix installs attributes, not exact versions, so `devy.lock` pins the attribute: a locked `24.20.0` means `nodejs_24`, and is satisfied by any installed `nodejs` 24.x. The lock records the version first resolved (read from the profile, e.g. `redis@8.6.3`) and keeps it on later runs, even when a teammate on a newer nixpkgs has `24.21.0` installed, so locks don't flap between machines. `devy up --update` re-resolves and records the versions currently installed.

**Unfree packages.** Four modules install nixpkgs packages with unfree licenses: `mongodb` (`mongodb-ce`), `elasticsearch`, `vault` and `terraform`. devy allows unfree packages for those installs only, with `NIXPKGS_ALLOW_UNFREE=1` and `--impure` on that one `nix profile install`, and prints `<attr>: nixpkgs#<attr> is unfree — allowing unfree packages for this install`. No configuration is needed, and your global Nix configuration is untouched. Every other install stays free-only, so a generic dependency that turns out to be unfree fails with nix's own error. `devy export` likewise adds an `allowUnfreePredicate` naming exactly those packages.

**Insecure packages.** nixpkgs marks `elasticsearch` (7.x, end-of-life) as insecure. devy allows insecure packages for that install only, with `NIXPKGS_ALLOW_INSECURE=1` and `--impure`, and warns `elasticsearch: nixpkgs#elasticsearch is marked insecure by nixpkgs — allowing insecure packages for this install`. The service only listens on `127.0.0.1`, but don't expose it beyond your machine. `devy export` adds a matching `allowInsecurePredicate`.

## Running services with Docker or Podman

Set `service_manager: docker` to run every service dependency as a per-project container instead of through the package manager. Languages and tools (`node`, `jq`, …) still come from the package manager. To containerize only some services, set `service_manager: docker` on those dependencies instead; to keep one service on the package manager while the rest run in containers, set `service_manager: package` on it.

```yaml
package_manager: brew
service_manager: docker
container_cli: docker      # or podman

dependencies:
  - node                   # installed with brew
  - postgresql             # runs in a container
  - redis:
      service_manager: package   # stays on brew services
```

devy drives any Docker-compatible CLI: Docker Desktop, OrbStack, Colima, Linux `dockerd`, or Podman (rootful or rootless) with `container_cli: podman`. `devy up` checks the CLI is on `PATH` and its daemon answers before it pulls or starts anything, and fails with `docker is not available — install it or start its daemon, or set service_manager: package` otherwise. devy never installs Docker or Podman, even with `--bootstrap`.

**Names and data.** Each service runs in a container named `devy-<project>-<service>`, where `<project>` is the project `name` plus a short hash of the project directory, so two checkouts of the same project never collide. Data lives in a named volume of the same name. Containers are labeled `sh.devy.project` and `sh.devy.service`.

**Ports.** devy assigns and locks a port for every docker-managed service, exactly as under Nix, and publishes it on `127.0.0.1` only. `REDIS_PORT`, `DATABASE_URL` and the other exported variables use that host port. Changing a port, image or setting recreates the container on the next `devy up`; the data volume is kept.

**Credentials.** Containers are set up for passwordless local use, matching the URLs devy exports: PostgreSQL trusts local connections (your OS user is the superuser) and MySQL/MariaDB allow an empty root password.

**Stopping and cleaning up.** `devy down` and `devy stop` stop containers and keep their data. `devy down --volumes` also removes each docker-managed service's container and volume, printing `✓ <service> container and volume removed`; package-managed services are unaffected.

**Pinned images.** Each service has a default image and tag (e.g. `redis:7`, `postgres:16`); `version:` selects a different tag. After pulling, `devy.lock` records `source: docker`, the tag as `resolved_version`, and the image's `image_digest` (`redis@sha256:…`). Teammates then pull and run exactly that digest, even if the tag has moved since. Changing `version` or `image` resolves the tag again; `devy up --update` re-pulls every tag and records the new digests.

**Registry mirrors.** `image:` replaces a service's image repository, e.g. for a corporate mirror. A tag in `image` overrides `version` (with a warning):

```yaml
dependencies:
  - redis:
      service_manager: docker
      image: registry.corp.example/mirror/redis   # runs registry.corp.example/mirror/redis:7
```

`service_manager` and `image` apply only to built-in services; on anything else devy fails with `<dep>: service_manager and image apply only to built-in services`.

**Kafka.** Kafka always runs as a single KRaft node in its container. Without `kraft: true`, `devy up` and `devy check` warn that zookeeper mode is not supported with docker; no ZooKeeper container is started.

**Apple Silicon.** The default `mailhog/mailhog` image is amd64-only, so it runs under emulation: slow, but it works. Point `image:` at a multi-arch build if you prefer.

**Rootless Podman** can't publish ports below 1024. devy's assigned ports are always above that; if you set an explicit low port (e.g. nginx `port: 80`), devy warns.

**Switching an existing project.** Moving a service from the package manager to docker starts it with a fresh, empty volume. Its old data stays with the package-managed service; dump and restore it if you need it.

## Commands

### `devy up`

Installs dependencies, starts services, and configures the environment.

```sh
devy up               # Set up the environment
devy up --bootstrap   # Auto-install Nix if it is not already installed
devy up --update      # Re-resolve all versions and rewrite devy.lock
devy up --dry-run     # Check status without making any changes
```

When `devy up` fails, it records what it was doing in `.devy/last-up-failure.json` and suggests `devy doctor`. The record holds the error, the step that failed (for example `install` or `start services`), the dependency involved, the platform, the package manager, the devy version and a UTC timestamp. Each failure replaces the previous record, and a successful `devy up` deletes it. The file is owner-readable only and never leaves your machine unless you run `devy doctor` with AI. `devy up --dry-run` never writes it.

### `devy down`

Stops all managed services.

```sh
devy down             # Stop services; docker-managed services keep their containers and data
devy down --volumes   # Also remove docker-managed containers and data volumes
```

### `devy services`, `devy start`, `devy stop`, `devy restart`

Manage individual services without touching the rest of the environment. Use these when you want to control a single service — restart a database after a config change, stop something you don't need right now, or bring a service back up without re-running `devy up` for everything.

```sh
devy services        # List all services and their current running status
devy start redis     # Start a service (skips if already running)
devy stop redis      # Stop a service (skips if already stopped)
devy restart mysql   # Stop then start a service, waiting for it to be healthy
```

Service names match what's defined under `dependencies:` in `devy.yml`.

When a service starts but its health check times out, or doesn't stop in time, devy points you at `devy logs <name>`.

### `devy logs`

Shows a service's recent log output, wherever its backend keeps it. Reading logs never starts, stops or changes anything.

```sh
devy logs redis              # The last 100 lines of redis's log
devy logs redis -n 20        # The last 20 lines (--lines)
devy logs redis -f           # Then keep streaming new lines until Ctrl-C (--follow)
devy logs                    # Every service, each under its own heading
devy logs -f                 # Every service, streamed together as "<name> | <line>"
devy logs postgres --explain # Ask Claude what's wrong (see "devy ask" below)
```

Names resolve like `devy start`: `devy logs postgres` finds a `postgresql` dependency. A name that isn't declared, or isn't a service, is an error. Following a service that isn't running is allowed, so you can watch it start from another terminal. Ctrl-C ends `--follow` with exit status 0.

Where devy reads from:

| Backend | Log source |
|---|---|
| nix on macOS | the launchd agent's log file, `$TMPDIR/devy-<name>.log` (shared by every project with a service of that name) |
| nix on Linux | the user journal: `journalctl --user -u devy-<name>.service` |
| brew | the files `brew services info --json <name>` reports; both stdout and stderr files when it reports two |
| apt | the system journal: `journalctl -u <name>` |
| docker / podman | `docker logs` (or `podman logs`) for the container `devy-<project>-<service>` |
| winget | not supported — devy says to check Windows Event Viewer or the service's own log directory |

`<name>` is the backend's service name, e.g. `postgresql` for a `postgres` dependency.

When a service hasn't logged anything yet, devy prints `· No logs yet for <name>` (with the expected file under nix on macOS) and exits 0. Under nix, services that also write their own log files under `.devy/data/<name>/` (nginx's `error.log` and `access.log`, Elasticsearch's and OpenSearch's `logs/`, Kafka's `app-logs/`, RabbitMQ's `log/`) get one `· also see <path>` line per file that exists.

On apt systems your user may not be allowed to read the system journal. devy never uses `sudo` for logs; it says the journal couldn't be read and suggests `sudo journalctl -u <name>` or adding yourself to the `systemd-journal` group (`sudo usermod -aG systemd-journal $USER`, then log in again).

### `devy ask`

Asks Claude a question about this project's environment, using a snapshot devy collects for it (see [What `devy ask` and `devy logs --explain` send](#ai-features)).

```sh
devy ask "why can't my app reach redis?"
devy ask "is postgres ok?" > answer.md                   # Only the answer goes to the file
devy ask --show-context "is postgres ok?"                 # Print what would be sent, then exit
devy logs postgres --explain                              # Diagnose one service from its logs
devy logs postgres --explain -n 300                       # …from its last 300 lines
devy logs postgres --explain --show-context               # Print that request, then exit
```

`devy ask` needs a `devy.yml` and doesn't install, start or change anything. `devy logs <name> --explain` sends that service's last `--lines` lines (default 100) with its configuration, and prints the likely cause and steps to fix it; it needs a service name and can't be combined with `--follow`. When the service has no logs yet, devy says so and doesn't contact Claude. `--show-context` works without `claude` installed.

If `claude` isn't installed or the request fails, devy prints `error: …` and exits 1.

### `devy status`

Shows what is installed, what services are running, and what environment variables are set.

```sh
devy status
```

### `devy check`

Validates that everything matches `devy.yml` and exits non-zero if any issues are found. Suitable for CI.

```sh
devy check
```

### `devy doctor`

Diagnoses the environment and the most recent failed `devy up`.

```sh
devy doctor                  # Checks, the last failure, and a diagnosis from Claude
devy doctor --no-ai          # Only devy's own checks; no network access
devy doctor --yes            # Apply a suggested devy.yml fix without asking
devy doctor --show-context   # Print what would be sent to Claude, then exit
```

It always runs the same checks as `devy check`, under a `Checks` header. Problems that make `devy check` stop, such as invalid YAML or a port conflict, are reported as findings instead. If `.devy/last-up-failure.json` exists, it prints that failure under `Last devy up failure`. When there is nothing wrong, it prints `✓ no problems found` and stops without contacting Claude.

Otherwise, if `claude` is available (see [AI features](#ai-features)), devy asks Claude for a diagnosis and prints a summary, the likely cause and numbered steps, labelled with the model that wrote them. Commands in the steps are for you to run; devy never runs them. Without `claude`, or with `--no-ai`, devy prints its findings and `· AI diagnosis unavailable — <reason>`. If the AI request fails, devy warns `AI diagnosis failed: <cause>`. Both cases still exit 0.

**Suggested fixes.** When Claude proposes a change to `devy.yml`, devy validates it first. The proposal must parse and pass the checks that `devy check` treats as errors: dependency entries, backend validation, known keys, shells and port conflicts. An invalid proposal is dropped with a warning. A valid one is shown as a diff under `Suggested fix`, and then:

- with `--yes`, devy writes it
- at an interactive terminal, devy asks `Apply this change to devy.yml? [y/N]`, defaulting to no
- otherwise (for example in CI), devy prints `· not applied — re-run with --yes to apply` and changes nothing

An accepted fix replaces `devy.yml` atomically, keeping its permissions, and devy prints `✓ updated devy.yml — run devy up to apply it`. devy never runs `devy up` itself, and `devy.yml` is the only file doctor writes. Since `devy.yml` is committed, use git to undo a fix.

`devy doctor` exits 0 whenever it completes, even when it finds problems. It exits 1 only when it can't run at all: outside a devy project, or when writing an accepted fix fails.

### `devy init`

Drafts a `devy.yml` for the current directory: devy scans the project's files, then Claude, through your `claude` CLI, turns that into a complete config.

```sh
devy init                 # Draft devy.yml with Claude. Fails if devy.yml already exists
devy init --force         # Overwrite an existing devy.yml
devy init --detect        # Skip Claude: draft from project files only, without network access
devy init --show-context  # Print what would be sent to Claude, without sending it
```

`--detect` and `--show-context` cannot be combined. `--force` applies to every mode. `init` only looks at the current directory, never its parents or subdirectories.

#### Project scan (`devy init --detect`)

Both modes start with the same offline scan. `devy init --detect` writes its result directly, as a draft config that starts with `# Generated by devy init --detect — review before committing`:

| Source | Becomes |
|---|---|
| `.nvmrc`, `.node-version`, `package.json` `engines.node` | `node` with its version |
| `.tool-versions` | one dependency per tool devy has a module for |
| `.ruby-version`, `.python-version` | `ruby` / `python` with its version |
| `rust-toolchain.toml`, `rust-toolchain` | `rust` (with a version when the channel is one) |
| `go.mod` `go` directive | `go` with its version |
| `compose.yaml`, `docker-compose.yml` images | service dependencies run as containers (`postgres:16` → `postgresql` version `16`, `service_manager: docker`) |
| `package.json` `scripts` | `commands`, as `<npm\|yarn\|pnpm\|bun> run <script>` depending on the lockfile |
| `.env.example`, `.env.sample`, `.env.template` | `environment` |
| `package.json` `name` | `name` (otherwise the directory name) |

Services found in a compose file get `service_manager: docker`, so they keep running as containers the way the project already runs them (see [Running services with Docker or Podman](#running-services-with-docker-or-podman)). They stay on the package manager instead when:

- the project already manages tools natively, shown by `flake.nix`, `shell.nix`, `default.nix`, `devbox.json`, `Brewfile`, or `use nix` / `use flake` in `.envrc`. A `# TODO:` then points out that `service_manager: docker` is available.
- the same service was already detected from a version file such as `.tool-versions`.

In `environment`, a value pointing at a detected service on `localhost` / `127.0.0.1` and its default port is rewritten to devy's injected variables, so `DATABASE_URL=postgres://localhost:5432/app` becomes `postgres://${POSTGRESQL_HOST}:${POSTGRESQL_PORT}/app`. Secret-looking values (see [AI features](#ai-features)) are left empty with a `# TODO: set` comment. `.env` and other real dotenv files are never read.

Anything devy found but could not map — an unknown compose image, a tool without a module, a script named like a devy subcommand — is listed as a `# TODO:` comment at the top of the file. With nothing to detect, `--detect` writes a starter config (`name: my-project`, `dependencies: []`) after the header.

#### With Claude (`devy init`)

Runs the scan above, then asks Claude for a complete `devy.yml`. It sends the detected draft, devy's module catalog (names, aliases, services, default ports, injected variables and accepted keys), and the redacted contents of these files when present: `package.json`, the runtime version files above, `Gemfile`, `pyproject.toml`, `requirements.txt`, `Cargo.toml`, compose files, `.env.example` / `.env.sample` / `.env.template`, `Makefile`, `Procfile`, `README.md` and `.github/workflows/*.yml`. See [AI features](#ai-features) for configuration and redaction.

devy validates the reply before writing anything: it must parse, use only known keys for each module, use valid shells, and have no conflicting explicit ports. If it fails, devy sends one follow-up request with the errors. If that also fails, devy prints the errors, writes nothing and exits 1. A written file starts with `# Generated by devy init (<model>) — AI-generated, review before committing`.

If `claude` is not installed, or it reports an error such as not being signed in, `devy init` fails without writing anything and suggests `devy init --detect`.

Run `devy init --show-context` to print the exact request (model, system prompt and the prompt piped to `claude`) and exit. It does not need `claude` installed and sends nothing.

### `devy export`

Exports the environment as a Nix file so you can use it with `nix-shell` or Nix flakes outside of devy.

```sh
devy export                    # Writes flake.nix (default)
devy export --format=shell     # Writes shell.nix
devy export --format=flake     # Writes flake.nix
```

### `devy pr`

Opens a GitHub pull request for the current branch in your browser.

```sh
devy pr
```

### `devy <command>`

Runs a command defined under `commands:` in `devy.yml`.

```sh
devy dev      # Runs the "dev" command
devy migrate  # Runs the "migrate" command
```

### `devy hook <shell>`

Prints a shell integration snippet. Pipe it to `eval` in your rc file (see [Installation](#installation)).

```sh
devy hook zsh
devy hook bash
devy hook fish
```

The snippet does two things:

1. **Shadowenv activation** — wraps `devy up` so the new environment is activated in your current shell session immediately after installation.
2. **Tab completion** — registers completion for all built-in subcommands (`up`, `down`, `start`, `stop`, `restart`, `services`, `status`, `check`, `doctor`, `logs`, `ask`, `init`, `hook`, `pr`, `export`) and flags. Commands you define under `commands:` in `devy.yml` are completed **dynamically** — the completion function calls `devy _commands` at tab-press time so new commands appear without reloading your shell. `devy logs <TAB>` completes service names the same way, from `devy _services`.

## Lock file

`devy up` writes `devy.lock` recording the exact version of every dependency that was installed, and the port assigned to every service whose port devy applies (see [Service environment variables](#service-environment-variables)). On subsequent runs without `--update`, devy pins each versionless dependency to its locked version and reuses its locked port, so the environment is reproducible across machines.

Commit `devy.lock` to version control. Run `devy up --update` when you want to upgrade.

Don't commit `.devy/`. It holds machine-local state: the Nix profile, service data and the last `devy up` failure record. Add it to `.gitignore`:

```gitignore
.devy/
```

Docker-managed services record `source: docker`, their image tag and `image_digest`, the digest of the pulled image, so every machine runs the same image (see [Running services with Docker or Podman](#running-services-with-docker-or-podman)).

## Supported dependency modules

### Services

| Name(s) | Default port | Notes |
|---|---|---|
| `mysql` | 3306 | Managed service; supports `port`, `cli_args` |
| `postgresql`, `postgres` | 5432 | Managed service; supports `port` |
| `redis` | 6379 | Managed service; health-checks via PING |
| `mongodb`, `mongo` | 27017 | Managed service |
| `nginx` | 80 | Managed service; supports `port` |
| `mariadb` | 3306 | Managed service |
| `rabbitmq` | — | Managed service |
| `memcached` | — | Managed service |
| `minio` | — | Managed service |
| `vault` | — | Managed service |
| `elasticsearch` | — | Managed service |
| `kafka` | — | Managed service |
| `meilisearch` | — | Managed service |
| `mailhog` | — | Managed service |
| `opensearch` | — | Managed service |

### Service environment variables

For every service dependency, `devy up` automatically injects two environment variables into your shell session:

| Variable | Value |
|---|---|
| `<SERVICE>_HOST` | Always `127.0.0.1` |
| `<SERVICE>_PORT` | The service's effective port |

The prefix is the canonical service name, uppercased, with hyphens replaced by underscores. For example, `redis` → `REDIS_HOST` / `REDIS_PORT`, and `postgresql` → `POSTGRESQL_HOST` / `POSTGRESQL_PORT`.

Which ports devy can choose depends on whether the backend can make the service actually listen on them:

| Backend | Services whose port devy applies |
|---|---|
| Nix (macOS, Linux) | Every built-in service. devy launches the process itself, with the port, a `127.0.0.1` bind and a data directory under `.devy/data/<service>/`. |
| Homebrew, apt | `postgresql`, `mysql` and `mariadb` only, via a devy-managed file in the service's `conf.d` directory. |
| WinGet | None. |

Port assignment follows this priority order:

1. **Explicit port in `devy.yml`** — e.g. `port: 3307` — always wins.
2. **Port saved in `devy.lock`** — when the backend applies the port, it's reused on every later `devy up`, including after `--update`, so the port stays stable across machines and teammates.
3. **Random available port** — when the backend applies the port, assigned on the first `devy up` if no port is configured and there's no lock entry.
4. **The service's default port** — everywhere else. For example, `redis` under Homebrew always uses 6379, so `REDIS_PORT` and `REDIS_URL` point where Redis actually listens.

If you set an explicit, non-default port that the backend can't apply (e.g. `redis` with `port: 6380` under Homebrew), devy still exports that port but warns that you have to configure the service to listen on it yourself.

`devy start`, `devy restart`, `devy check` and `devy status` resolve ports the same way using `devy.lock`, but never assign new ports or write the lock. devy errors if two services resolve to the same port. Ports that `devy up` hasn't assigned yet are excluded, so `mysql` and `mariadb` under Nix don't conflict, but `elasticsearch` and `opensearch` under Homebrew (both 9200) do.

Values set under `environment:` in `devy.yml` take precedence over the auto-injected `_HOST` / `_PORT` variables, so you can override them if needed.

### Languages and runtimes

| Name(s) | Notes |
|---|---|
| `node`, `nodejs`, `javascript`, `js` | Supports `global_packages` |
| `typescript`, `ts` | Installs Node + TypeScript globally; supports `global_packages` |
| `ruby` | Runs `bundle install` when a `Gemfile` is present |
| `rust`, `rustup` | Installs via rustup (all platforms); supports `toolchain`, `targets`, `components` |
| `python`, `python3` | |
| `go`, `golang` | |
| `java`, `openjdk` | |
| `kotlin` | |
| `elixir` | |
| `erlang` | |
| `dart` | |
| `crystal` | |
| `zig` | |
| `bun` | |
| `deno` | |
| `dotnet` | |
| `swift` | |
| anything else | Falls back to a generic package manager install |

### Package name mapping

Each module knows the correct package name for each package manager — you always use the same name in `devy.yml` regardless of which backend is active:

| Module | Nix (`nixpkgs`) | Homebrew | apt | WinGet |
|---|---|---|---|---|
| `mysql` | `mysql84` (version-matched) | `mysql` | `mysql-server` | `Oracle.MySQL` |
| `postgresql` | `postgresql` (version-matched) | `postgresql` | `postgresql` | `PostgreSQL.PostgreSQL` |
| `redis` | `redis` | `redis` | `redis-server` | `Redis.Redis` |
| `mongodb` | `mongodb-ce` (unfree) | `mongodb-community` | `mongodb-org` | `MongoDB.Server` |
| `nginx` | `nginx` | `nginx` | `nginx` | `Nginx.Nginx` |
| `node` | `nodejs` (version-matched) | `node` | `nodejs` | `OpenJS.NodeJS` |
| `python` | `python3` (version-matched) | `python` | `python3` | `Python.Python.3` |
| `go` | `go` (version-matched) | `go` | `golang-go` | `GoLang.Go` |
| `java` | `jdk21` (version-matched) | `openjdk` | `default-jdk` | `Microsoft.OpenJDK.21` |
| `kotlin` | `kotlin` | `kotlin` | `kotlin` | `JetBrains.Kotlin` |
| `ruby` | `ruby` | `ruby` | `ruby` | `RubyInstallerTeam.Ruby.3` |

### Platform notes

**macOS (Nix default):** Services are managed via launchd. devy writes a `LaunchAgent` plist to `~/Library/LaunchAgents/sh.devy.<name>.plist` and uses `launchctl` to start and stop them.

**Linux (Nix default):** Services are managed via systemd user units. devy writes a unit file to `~/.config/systemd/user/devy-<name>.service` and uses `systemctl --user` to start and stop them — no `sudo` required.

**Services under Nix:** devy launches each service from `.devy/nix-profile/bin` with its resolved port, a `127.0.0.1` bind, and its data, sockets and generated config under `.devy/data/<service>/`. The plist or unit is rewritten on every start, so port changes take effect. Databases are initialized on first start (`initdb`, `mysqld --initialize-insecure`, `mariadb-install-db`), and Kafka's storage is formatted once. Kafka always runs in KRaft mode, because nixpkgs ships Kafka 4, which has no ZooKeeper. Vault without `dev_mode` gets a file-storage config and starts sealed, so initialize and unseal it yourself. `mongodb` installs nixpkgs' unfree `mongodb-ce` (see "Versions under Nix" for unfree handling). `mysql` and `mariadb` can be used together: MariaDB's client tools (`mysql`, `mysqldump`, …) take precedence in `.devy/nix-profile/bin`, while the `mysql` service still runs MySQL's own server. Unit names aren't per-project, so two projects can't run the same service under Nix at the same time.

**Search servers under Nix:** `elasticsearch` and `opensearch` write into their config directory, which in the Nix store is read-only. On first start devy copies the package's `config/` to `.devy/data/<service>/config/`, makes it writable, and points the server at it (`ES_PATH_CONF` / `OPENSEARCH_PATH_CONF`). In that copy, the relative GC-log, error-file and heap-dump paths in `jvm.options` are rewritten to point under `.devy/data/<service>/`, because the package's start script runs from the read-only store. Later starts reuse it, so edits such as JVM heap in `jvm.options` persist. To reseed it, for example after a major-version upgrade, stop the service, delete `.devy/data/<service>/config/` and start it again. Elasticsearch runs with `ES_HOME` set to its package directory and machine learning disabled (`xpack.ml.enabled=false`), and OpenSearch with its security plugin disabled, so it serves plain HTTP.

**macOS (Homebrew):** Set `package_manager: brew` in `devy.yml`. Service management uses `brew services`.

**Ubuntu/Debian (apt):** Set `package_manager: apt` in `devy.yml`. Install operations use `sudo apt-get`. Version pinning with the `version:` field uses apt's exact-version syntax (`pkg=version`) — for most languages, omit the version field and rely on `devy.lock` to pin the installed version across machines.

**Windows:** Service management uses `net start`/`sc`. Custom MySQL/PostgreSQL config options (`port`, `cli_args`) are not applied on Windows. Nix is not supported on Windows.

## AI features

Only `devy init` (not with `--detect`), `devy doctor` (not with `--no-ai`), `devy ask` and `devy logs --explain` use AI, and none of them with `--show-context`. No other command looks for or runs `claude`. A failed `devy up` only suggests `devy doctor`; it never contacts Claude.

They run through your own [Claude Code](https://claude.com/claude-code) CLI, so devy needs no API key of its own: install `claude`, sign in once, and devy uses whatever account and default model `claude` is set up with. devy runs `claude -p` with all tools, MCP servers and slash commands disabled, without saving the session, and from an empty temporary directory, so the project's own `.claude/` settings, hooks and `CLAUDE.md` are not loaded.

| Variable | Purpose |
|---|---|
| `DEVY_AI_MODEL` | Optional. Model passed to `claude --model` (for example `opus` or `claude-opus-5-5`). Unset uses `claude`'s default. |

```sh
devy init
DEVY_AI_MODEL=opus devy init
```

The header of a generated file names the model `claude` reports having used.

**What `devy doctor` sends.** Only these, after redaction:

- the last `devy up` failure record
- the findings from devy's checks
- `devy.yml` and `devy.lock`
- the platform, package manager and devy version
- up to the last 50 log lines of each service that is named in the failure record or reported stopped, read the same way as [`devy logs`](#devy-logs)

Services with no logs available are listed as `no logs available`. devy never sends the environment file (`.shadowenv.d/`), dotenv files or any other file. The request also includes devy's `devy.yml` reference and module catalog.

**What `devy ask` sends.** Your question and, after redaction:

- `devy.yml`, and `devy.lock` if it exists
- the platform and the package manager
- for each dependency, whether it is installed and, for services, whether it is running
- for each service, its last 50 log lines, read the same way as `devy logs` (each log command gets 5 seconds)

A service whose logs can't be read (winget, an unreadable journal, a stopped container runtime) gets a one-line note instead, and the question is still sent. When the whole snapshot would exceed 60 KiB, the oldest log lines are dropped first.

**What `devy logs <name> --explain` sends.** After redaction: that service's `devy.yml` entry, its `devy.lock` entry, whether it is running, its port, and its last `--lines` log lines.

Both requests also include a short note on how devy's backends work. Neither ever includes your shell's environment variables, the shadowenv file or any other file.

**What is redacted.** Before anything is sent, devy replaces with `<redacted>`:

- the value of any `KEY=value` or `key: value` entry whose key contains `KEY`, `SECRET`, `TOKEN`, `PASSWORD`, `PASSWD`, `CREDENTIAL` or `PRIVATE` (case-insensitive)
- passwords in URLs (`postgres://app:hunter2@localhost/app` → `postgres://app:<redacted>@localhost/app`)
- PEM blocks (`-----BEGIN … -----END …-----`)
- tokens starting with `sk-`, `ghp_`, `github_pat_`, `xox` (Slack) or `AKIA` (AWS)

devy never reads `.env`, `.env.local` or other real dotenv files, `.git`, SSH keys, or anything outside the file list above. Each file is capped at 8 KiB and the whole request at 100 KiB; longer content ends with `… [truncated]`.

Redaction is pattern-based. A secret stored under an innocuous name — for example a password pasted into `README.md` — would be sent. Use `--show-context` to check before sending.

**Failures.** If `claude` is not on `PATH`, `devy init` says so and suggests `devy init --detect`; `devy doctor` prints its own findings and exits 0 (see [`devy doctor`](#devy-doctor)); `devy ask` and `devy logs --explain` print `error: …` and exit 1. A request is stopped after 5 minutes. When `claude` fails or reports an error (for example, not signed in), devy prints `AI request failed: <reason>` and exits 1 without writing files. Retries on rate limits are left to `claude`.

## Security

### `after_install`

`devy.yml` supports an `after_install` field that runs an arbitrary shell command immediately after a dependency is freshly installed:

```yaml
dependencies:
  - mysql:
      after_install: "mysql_secure_installation"
```

**This executes arbitrary code on your machine.** devy prints a warning to the terminal before executing each `after_install` command so you can see what is about to run. Before running `devy up` on a project you did not author — especially one shared via a template or onboarding flow — review all `after_install` values in `devy.yml`.

This is the same attack surface as `npm install` lifecycle scripts or `pip install` running `setup.py`.

### Nix auto-install

If Nix is not installed on macOS or Linux, `devy up --bootstrap` will install it by fetching and executing the [Determinate Installer](https://install.determinate.systems) over HTTPS. Without `--bootstrap`, devy exits with an error and instructions to install Nix manually:

```sh
devy up --bootstrap   # allows automatic Nix installation
devy up               # exits with an error if Nix is not installed
```

This is recommended in CI environments where unexpected system-level changes should be blocked — omit `--bootstrap` and pre-install Nix in your CI image instead.

### Homebrew `tap:` field

When using `package_manager: brew`, `devy.yml` accepts a `tap:` field to pull packages from a custom Homebrew tap:

```yaml
package_manager: brew
dependencies:
  - my-tool:
      tap: myorg/homebrew-tools
```

Only allow taps from sources you trust — tapping a malicious repository can execute code during the tap install. This field has no effect when using the Nix backend.

### `devy.yml` scope

devy walks up the directory tree to find `devy.yml` but stops at the nearest `.git` root (or `$HOME`). It will not read `devy.yml` files from parent directories outside the current git repository, preventing a malicious config in a parent directory from being executed.
