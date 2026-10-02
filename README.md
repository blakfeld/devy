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

Create a starter config:

```sh
devy init
```

Then edit `devy.yml` to add your dependencies, and bring the environment up:

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

dependencies:
  # Simple form — installs the latest version
  - redis
  - jq

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

## Commands

### `devy up`

Installs dependencies, starts services, and configures the environment.

```sh
devy up               # Set up the environment
devy up --bootstrap   # Auto-install Nix if it is not already installed
devy up --update      # Re-resolve all versions and rewrite devy.lock
devy up --dry-run     # Check status without making any changes
```

### `devy down`

Stops all managed services.

```sh
devy down
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

### `devy init`

Creates an empty `devy.yml` in the current directory.

```sh
devy init          # Fails if devy.yml already exists
devy init --force  # Overwrite an existing devy.yml
```

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
2. **Tab completion** — registers completion for all built-in subcommands (`up`, `down`, `start`, `stop`, `restart`, `services`, `status`, `check`, `init`, `hook`, `pr`, `export`) and flags. Commands you define under `commands:` in `devy.yml` are completed **dynamically** — the completion function calls `devy _commands` at tab-press time so new commands appear without reloading your shell.

## Lock file

`devy up` writes `devy.lock` recording the exact version of every dependency that was installed, and the port assigned to every service whose port devy applies (see [Service environment variables](#service-environment-variables)). On subsequent runs without `--update`, devy pins each versionless dependency to its locked version and reuses its locked port, so the environment is reproducible across machines.

Commit `devy.lock` to version control. Run `devy up --update` when you want to upgrade.

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
