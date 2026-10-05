# dependency-modules Specification

## Purpose
How a dependency name in `devy.yml` maps to a module, and what the non-service modules (languages, runtimes and CLI tools) do: how they install, what extra config keys they accept, the idempotent project setup they run on every `devy up`, and the environment they contribute.

## Requirements

### Requirement: Module registry and generic fallback
The system SHALL resolve each dependency name to a module through a registry of canonical names. An unregistered name MUST fall back to a generic module. The generic module passes the name (and `version`/`tap`) directly to the active package manager and accepts any extra config key.

#### Scenario: Known module
- **WHEN** a dependency is named `python`
- **THEN** the python module handles its install, setup and environment

#### Scenario: Unknown name
- **WHEN** a dependency is named `jq`
- **THEN** the generic module installs package `jq` through the active package manager

### Requirement: Aliases resolve to canonical names
The system SHALL accept these aliases and treat them exactly like their canonical name, including as the key in `devy.lock`, for service lookup and for Nix export:

- `postgres`→`postgresql`, `mongo`→`mongodb`, `elastic`→`elasticsearch`, `meili`→`meilisearch`, `hashicorp-vault`→`vault`
- `rustup`→`rust`, `nodejs`/`javascript`/`js`→`node`, `ts`→`typescript`, `python3`→`python`, `golang`→`go`, `openjdk`→`java`, `dotnet-sdk`/`csharp`→`dotnet`
- `aws`/`aws-cli`→`awscli`, `github-cli`→`gh`, `google-cloud-sdk`→`gcloud`, `kubernetes-cli`→`kubectl`, `az`→`azure-cli`

Every alias MUST target a registered canonical name, and no alias may equal a canonical name.

#### Scenario: Alias used in devy.yml
- **WHEN** a dependency is listed as `nodejs`
- **THEN** it is handled by the node module and recorded in `devy.lock` under `node`

### Requirement: Extra config key allowlists
Each registered module SHALL declare which module-specific extra keys it accepts; a module that declares none accepts no extra keys. `devy check` MUST count every extra key outside a module's allowlist as an issue, with the message `<dep>: unrecognized config key `<key>` — known keys: …` or `… this module accepts no extra keys`. The generic module MUST NOT report unknown keys.

#### Scenario: Typo in a module option
- **WHEN** `rust` is configured with `toolchian: nightly`
- **THEN** `devy check` reports an unrecognized key `toolchian` and lists `toolchain, targets, components`

#### Scenario: Extra key on a generic dependency
- **WHEN** `jq` is configured with `foo: bar`
- **THEN** `devy check` does not report it

### Requirement: Package names per package manager
A module SHALL install under a package name chosen for the active package manager (apt, winget, nix, or a default used by brew), keeping `version` and `tap`. The simple tool modules MUST use these names (default/brew, apt, winget, nix):

- go: go, golang-go, GoLang.Go, go
- scala: scala, scala, EPFL.Scala, scala
- php: php, php, PHP.PHP, php
- awscli: awscli, awscli, Amazon.AWSCLI, awscli2
- gh: gh, gh, GitHub.cli, gh
- kubectl: kubectl, kubectl, Kubernetes.kubectl, kubectl
- helm: helm, helm, Helm.Helm, kubernetes-helm
- terraform: terraform, terraform, Hashicorp.Terraform, terraform
- azure-cli: azure-cli, azure-cli, Microsoft.AzureCLI, azure-cli
- swift: swift, swift, Swift.Toolchain, swift

#### Scenario: Go on apt
- **WHEN** `go` is a dependency and the package manager is apt
- **THEN** devy installs the apt package `golang-go`

### Requirement: Idempotent project setup via stamp files
Modules that install project dependencies (e.g. `npm install`, `bundle install`, `cargo fetch`) SHALL run that step during every `devy up`, even when the toolchain was already installed. They MUST skip it when a stamp file records the same modification time (in seconds) as the project's manifest or lockfile. After a successful run they MUST write the stamp. A failure to write a manifest-mtime stamp MUST only warn that dependencies will re-run on the next `devy up`. The node global-package stamp (`.devy_node_global_stamp`) and the gcloud component stamp (`.devy_gcloud_components_stamp`) ignore write failures silently. Ruby fails with `Failed to create .bundle directory for stamp` if it cannot create `.bundle/`.

#### Scenario: Manifest unchanged
- **WHEN** `devy up` runs twice and `Cargo.lock` has not changed in between
- **THEN** the second run skips `cargo fetch` because `.devy_rust_stamp` matches

#### Scenario: Manifest changed
- **WHEN** `package-lock.json` is modified after a previous `devy up`
- **THEN** the next `devy up` runs `npm install` again and updates the stamp

### Requirement: Node module
The node module SHALL install Node through the package manager (apt `nodejs`, winget `OpenJS.NodeJS`, nix `nodejs`, otherwise `node`) and accept `global_packages`. If `package.json` exists in the project root, it MUST run `<tool> install`. The tool is picked by lockfile: `pnpm-lock.yaml`→pnpm, `yarn.lock`→yarn, otherwise npm. The run is stamped in `.devy_node_local_stamp` against the lockfile, or `package.json` when there is no lockfile. It MUST run `npm install -g <global_packages…>` unless the package list matches `.devy_node_global_stamp`. The stamp holds the list in config order (not sorted), so reordering `global_packages` re-runs the install.

#### Scenario: pnpm project
- **WHEN** the project root contains `package.json` and `pnpm-lock.yaml`
- **THEN** `devy up` runs `pnpm install` in the project root

#### Scenario: Global packages unchanged
- **WHEN** `global_packages: [typescript]` is unchanged since the last run
- **THEN** `npm install -g` is not run again

#### Scenario: Global packages reordered
- **WHEN** `global_packages` changes from `[a, b]` to `[b, a]`
- **THEN** the next `devy up` runs `npm install -g b a` again

### Requirement: TypeScript module
The typescript module SHALL install Node as the node module does, then install the `typescript` npm package globally along with any packages listed in its `global_packages` extra key, which is part of its allowed extra keys. On every `devy up` it MUST run the same lockfile-based project install as the node module, stamped in `.devy_ts_local_stamp`. Its nixpkgs attribute follows the node module's, including versioned attributes.

#### Scenario: Fresh install
- **WHEN** `typescript` is not installed
- **THEN** devy installs Node and runs `npm install -g typescript`

#### Scenario: Global packages
- **WHEN** `typescript` is declared with `global_packages: [eslint, prettier]` and is not installed
- **THEN** devy runs `npm install -g typescript eslint prettier`
- **AND** `devy check` does not report `global_packages` as unrecognized

#### Scenario: Node already installed
- **WHEN** Node is already installed through the package manager but the `typescript` npm package is not
- **THEN** devy reports typescript as already installed and does not run `npm install -g typescript`

### Requirement: Ruby module
The ruby module SHALL install Ruby through rbenv on brew, apt and nix: install `rbenv` (plus `ruby-build` on apt) if it is missing, then `rbenv install --skip-existing <version>`, defaulting to 3.3.6. On winget it MUST install `RubyInstallerTeam.Ruby.<major>` (major defaults to 3). On setup, when rbenv is available, it MUST run `rbenv local <version>`. Without a pinned version, an existing `.ruby-version` is left alone, and otherwise 3.3.6 is used. If a `Gemfile` exists it MUST run `bundle install`, preferring `$RBENV_ROOT/shims/bundle`, stamped in `.bundle/.devy_stamp` against `Gemfile.lock` or `Gemfile`. It contributes `RBENV_ROOT` (`$RBENV_ROOT`, or `~/.rbenv`) and prepends `<rbenv_root>/bin` and `<rbenv_root>/shims` to PATH. On brew, apt and nix, ruby counts as installed when `rbenv` is on PATH and either the pinned `version` is installed in rbenv or, with no pinned version, rbenv lists any Ruby version (`rbenv versions --bare` is non-empty); in that case 3.3.6 is not installed. On winget it counts as installed when the RubyInstaller package is installed. The lock version is the pinned `version`, or else the output of `rbenv local`; it is not read from the installed Ruby. On winget the version is looked up through the package manager under the name `ruby`, not the RubyInstaller id. Its lock source is always `rbenv`, including on winget.

#### Scenario: Gemfile present
- **WHEN** the project has a `Gemfile` and `Gemfile.lock` and no matching stamp
- **THEN** devy runs `bundle install` in the project root and writes `.bundle/.devy_stamp`

#### Scenario: Existing .ruby-version respected
- **WHEN** ruby has no `version` in `devy.yml` and the project has `.ruby-version`
- **THEN** devy does not run `rbenv local`

#### Scenario: Another Ruby already in rbenv
- **WHEN** ruby has no `version`, the project has no `.ruby-version`, and rbenv has only Ruby 3.2.2 installed
- **THEN** devy treats ruby as installed, skips `rbenv install 3.3.6`, and setup fails with "`rbenv local 3.3.6` failed — run `rbenv install 3.3.6` first"

### Requirement: Python module
The python module SHALL install Python through the package manager (apt `python3`, winget `Python.Python.3`, nix `python3`, otherwise `python`) and accept `venv_path` (default `.venv`) and `install_cmd`. `venv_path` MUST be a relative path without `..` components that resolves inside the project root; otherwise config validation SHALL fail with `python: invalid venv_path <value>`. On setup, only when `<project_root>/<venv_path>/pyvenv.cfg` is missing, it MUST pick `python3` from PATH (else `python`), fail with `Python installation appears incomplete — `<python> --version` failed` if `<python> --version` fails, and create the virtualenv with `<python> -m venv <path>`. It then installs dependencies using the first that applies:
1. `install_cmd`, run through the default shell
2. `<venv>/bin/pip install -e .` when `pyproject.toml` exists
3. `<venv>/bin/pip install -r requirements.txt` when `requirements.txt` exists

On Windows `pip` is `<venv>/Scripts/pip`. Manifest-based installs are stamped in `<venv>/.devy_stamp`. `install_cmd` is never stamped, so it runs on every `devy up`. The module contributes `VIRTUAL_ENV=<absolute venv path>` and prepends `<venv>/bin` (`Scripts` on Windows) to PATH. The virtualenv directory is subject to the filesystem-safety checks for devy-managed directories.

#### Scenario: requirements.txt project
- **WHEN** the project has `requirements.txt` and no `.venv`
- **THEN** devy creates `.venv`, runs `.venv/bin/pip install -r requirements.txt`, and sets `VIRTUAL_ENV`

#### Scenario: Custom install command
- **WHEN** python sets `install_cmd: "poetry install"`
- **THEN** devy runs `poetry install` through the default shell instead of pip, on every `devy up`

#### Scenario: venv outside the project
- **WHEN** python sets `venv_path: /tmp/shared`
- **THEN** config validation fails and nothing is added to PATH

### Requirement: Rust module
The rust module SHALL install through rustup, regardless of the package manager, and accept `toolchain` (default `stable`), `targets` and `components`. Rust counts as installed whenever `rustup` is on PATH. Only when it is not does devy run the official `sh.rustup.rs` installer with `-y --no-modify-path`, then (using `~/.cargo/bin/rustup` when rustup is still not on PATH) `rustup toolchain install <tc>`, `rustup default <tc>`, `rustup target add --toolchain <tc> <target>` and `rustup component add --toolchain <tc> <component>` for each listed entry. While rustup is present, changing `toolchain`, `targets` or `components` has no effect. If `Cargo.toml` exists it MUST run `cargo fetch`, stamped in `.devy_rust_stamp` against `Cargo.lock` or `Cargo.toml`. Its lock source is `rustup` and its version comes from `rustc --version`.

#### Scenario: Extra target
- **WHEN** rust is configured with `targets: [wasm32-unknown-unknown]` and rustup is not yet installed
- **THEN** devy runs `rustup target add --toolchain stable wasm32-unknown-unknown`

#### Scenario: Target added after rustup exists
- **WHEN** `rustup` is already on PATH and `targets: [wasm32-unknown-unknown]` is added to devy.yml
- **THEN** devy reports rust as already installed and does not add the target

### Requirement: Java and Kotlin modules
The java module SHALL install a JDK (apt `default-jdk`, winget `Microsoft.OpenJDK.<major>`, nix `jdk<major>`, otherwise `openjdk`; major defaults to 21). The `version` is also passed to the backend unchanged, so `version: 21` on apt installs `default-jdk=21`. On setup it MUST resolve dependencies:
- `pom.xml`: `./mvnw -B dependency:resolve` (or `mvn -B dependency:resolve` without the wrapper), stamped against `pom.xml`
- otherwise `build.gradle.kts` (preferred) or `build.gradle`: `./gradlew` (or `gradle`) `--no-daemon dependencies`, stamped against that file

Both are stamped in `.devy_java_stamp`. When devy can detect a JDK home, Java MUST contribute `JAVA_HOME` and prepend `$JAVA_HOME/bin` to PATH. Detection uses `/usr/libexec/java_home` on macOS; `/usr/lib/jvm/default-java`, or a path derived from `java` on PATH, on Linux; and an existing `$JAVA_HOME` on Windows. When nothing is detected it contributes neither. The kotlin module SHALL install `kotlin` (winget `JetBrains.Kotlin`) and resolve Gradle dependencies, stamped in `.devy_kotlin_stamp`.

#### Scenario: Maven project with wrapper
- **WHEN** the project has `pom.xml` and `./mvnw`
- **THEN** devy resolves dependencies with `./mvnw`

### Requirement: Other language runtime modules
These modules SHALL install through the package manager (winget id in parentheses) and run their project dependency step with a stamp file, only when the listed manifest exists:
- elixir (`Erlang-Solutions.Elixir`): installs erlang first if missing; runs `mix deps.get` against `mix.lock`/`mix.exs`, stamp `.devy_elixir_stamp`
- erlang (`Erlang-Solutions.Erlang`): runs `rebar3 get-deps` against `rebar.lock`/`rebar.config`, stamp `.devy_erlang_stamp`
- dotnet (brew/default `dotnet`, apt `dotnet-sdk-<M>.0`, nix `dotnet-sdk_<M>`, winget `Microsoft.DotNet.SDK.<M>`, M defaults to 8): runs `dotnet restore` for a `.sln` (preferred) or `.csproj`, stamp `.devy_dotnet_stamp`
- dart (`Dart.Dart`): runs `dart pub get` against `pubspec.lock`/`pubspec.yaml`, stamp `.devy_dart_stamp`
- zig (`zig-lang.zig`): runs `zig build` when `build.zig.zon` exists, stamp `.devy_zig_stamp`
- crystal (`Manas.Crystal`): runs `shards install` against `shard.lock`/`shard.yml`, stamp `.devy_crystal_stamp`

#### Scenario: Elixir project
- **WHEN** the project has `mix.exs` and no matching stamp
- **THEN** `devy up` runs `mix deps.get`

### Requirement: Script-installed runtimes
The deno, bun and gcloud modules SHALL use their official install scripts instead of the package manager. Each script SHALL be downloaded and checked as defined by the package-managers requirement on verified installer downloads, and SHALL then be run from the downloaded file with the version passed as a separate argument or environment variable, never interpolated into a shell command string.
- deno: the deno installer run with `sh <file>`, with `-s v<version>` when pinned (a version already starting with `v` is passed as is). Counted as installed when `deno` is on PATH or at `~/.deno/bin/deno`. Lock source `deno-installer`. Runs `deno install` when `deno.json` (preferred) or `deno.jsonc` exists, stamped in `.devy_deno_stamp` against that file.
- bun: the bun installer run with `bash <file>`, with the pinned version passed as the installer's version argument (`bun-v<version>`) when pinned. Counted as installed when `bun` is on PATH or at `~/.bun/bin/bun`. Lock source `bun-installer`. Runs `bun install` when `package.json` exists, stamped in `.devy_bun_stamp` against `bun.lockb`, or `package.json` when there is no lockfile.
- gcloud: uses the package manager on brew (`google-cloud-sdk`) and winget (`Google.CloudSDK`), where installed means the package is installed. On nix and apt it counts as installed when `gcloud` is on PATH or at `$HOME/google-cloud-sdk/bin/gcloud` (which is then used for `components` and the version), and otherwise downloads the verified Google Cloud CLI archive pinned in devy for the current OS and architecture, unpacks it with `tar` in a private staging directory under `$HOME`, moves it to `$HOME/google-cloud-sdk` (failing if that already exists) and runs its bundled installer with `bash $HOME/google-cloud-sdk/install.sh --quiet --usage-reporting=false --path-update=false --command-completion=false`, removing `$HOME/google-cloud-sdk` again if that fails. (Google's `install_google_cloud_sdk.bash` downloads an unversioned, unverified archive, so devy does not use it.) Lock source `gcloud-installer` on every backend. Accepts `components`, installed one per command with `gcloud components install --quiet -- <component>` unless the sorted list matches `.devy_gcloud_components_stamp`.

For deno and bun the installed check ignores `version`, so a pinned version is used only on first install and changing it never reinstalls. None of these three modules adds anything to PATH (`~/.deno/bin`, `~/.bun/bin` and the gcloud SDK `bin` are not added).

#### Scenario: Pinned deno version
- **WHEN** deno is configured with `version: "1.40.0"` and is not installed
- **THEN** devy runs the verified deno installer with `-s v1.40.0` as separate arguments

#### Scenario: Pinned version changed
- **WHEN** deno is already installed and its `version` is changed in devy.yml
- **THEN** devy reports deno as already installed and does not reinstall it

#### Scenario: gcloud components unchanged
- **WHEN** gcloud `components` are unchanged since the last run
- **THEN** `gcloud components install` is not run again

#### Scenario: Pinned bun version honored
- **WHEN** bun is configured with `version: "1.1.0"` and is not installed
- **THEN** devy runs the verified bun installer with `bun-v1.1.0` as its version argument

### Requirement: Resolved versions for the lock file
Each module SHALL report the installed version recorded in `devy.lock` and the install source.
- **Default lookup:** the active package manager is queried using the dependency's `devy.yml` name, not its per-manager package name. Modules whose package name differs (for example go → `golang-go` on apt, node → `nodejs` on apt) therefore usually record no version.
- **Under nix:** modules SHALL instead query the nixpkgs attribute they install, versioned or not. If that attribute isn't installed, they query the unversioned attribute, so node, python, java, dotnet, go, mysql and mongodb (`mongodb-ce`) record their versions.
- **Own installers:** modules with their own installer report their own source (`rbenv`, `rustup`, `deno-installer`, `bun-installer`, `gcloud-installer`). rust, deno, bun and gcloud read the version from the tool itself. ruby records its pinned `version`, or else the output of `rbenv local`. gcloud's source is `gcloud-installer` even when brew or winget installed it.

#### Scenario: Rust version in lock
- **WHEN** `devy up` installs rust
- **THEN** `devy.lock` records `rust` with `source: rustup` and the version reported by `rustc --version`

#### Scenario: Package name differs from dependency name
- **WHEN** `go` is installed with the apt backend (package `golang-go`)
- **THEN** devy looks up the version of the `go` package, finds none, and records no version for `go` in `devy.lock`

#### Scenario: Node version under nix
- **WHEN** `devy up` installs `node` without a version under nix and `nodejs` 24.20.0 lands in the profile
- **THEN** `devy.lock` records `node` with `resolved_version: 24.20.0` and `source: nix`

### Requirement: Setup tools run from devy's own PATH
Module setup commands (for example `npm`, `pnpm`, `cargo`, `mix`, `rbenv`, `bundle`, `gcloud`) and ruby's `rbenv install` SHALL run with the PATH of the `devy` process itself. During `devy up`, devy does not add `<project_root>/.devy/nix-profile/bin` or module PATH entries to its own PATH; those entries only take effect once shadowenv activates them. On a fresh nix-backend run, tools installed into the project profile are therefore not found by later setup steps.

#### Scenario: Ruby on a fresh nix profile
- **WHEN** the backend is nix, `rbenv` is not on the user's PATH, and `devy up` installs `rbenv` into `.devy/nix-profile`
- **THEN** the following `rbenv install` fails because `rbenv` cannot be started

### Requirement: Versioned nix attributes
Modules with versioned nixpkgs packages SHALL map a dependency `version` to a versioned attribute, using the major (and, where nixpkgs names include it, minor) component of the version. This means full versions applied from `devy.lock` map to the same attribute as their short form. Each module SHALL map only an allowlist of versions that current nixpkgs carries:

| Module | Supported versions | Example | Without a version |
|---|---|---|---|
| node, typescript | 22, 24 | `22` or `22.11.0` → `nodejs_22` | `nodejs` |
| python | 3.11–3.14 | `3.12` or `3.12.4` → `python312` | `python3` |
| postgresql | 14–18 | `16` → `postgresql_16` | `postgresql` |
| mysql | 8.4 | `8.4` → `mysql84` | `mysql84` |
| java | 8, 11, 17, 21, 25 | `21` → `jdk21` | `jdk21` |
| dotnet | 6–10 | `8` → `dotnet-sdk_8` | `dotnet-sdk_8` |
| go | 1.26 | `1.26` → `go_1_26` | `go` |

Modules without a mapping, and versions outside the supported set, SHALL fall back to the unversioned attribute.

#### Scenario: Full locked version maps to major
- **WHEN** `node` has `version` `22.11.0` applied from `devy.lock` under nix
- **THEN** devy installs `nodejs_22`

#### Scenario: Version nixpkgs no longer carries
- **WHEN** `node` is declared with `version: "20"` under nix
- **THEN** devy installs `nodejs` and warns that the version is not supported by the nix backend

#### Scenario: Python minor version
- **WHEN** `python` is declared with `version: "3.12"` under nix
- **THEN** devy installs `python312`
