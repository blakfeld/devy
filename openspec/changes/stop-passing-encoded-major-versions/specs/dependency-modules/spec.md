# Spec Delta

## MODIFIED Requirements

### Requirement: Package names per package manager
A module SHALL install under a package name chosen for the active package manager (apt, winget, nix, or a default used by brew), keeping `version` and `tap`. The exception is a module whose own requirement says the package name already carries the version, which then does not pass it to the backend. The simple tool modules MUST use these names (default/brew, apt, winget, nix):

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

#### Scenario: Simple module keeps its version
- **WHEN** `go` has `version: 1.22.2-1` and the package manager is apt
- **THEN** devy installs `golang-go=1.22.2-1`

### Requirement: Ruby module
The ruby module SHALL install Ruby through rbenv on brew, apt and nix: install `rbenv` (plus `ruby-build` on apt) if it is missing, then `rbenv install --skip-existing <version>`, defaulting to 3.3.6. On winget it MUST install `RubyInstallerTeam.Ruby.<major>.<minor>`, with major and minor from the pinned `version`, or `3.3` (from the 3.3.6 default) when no version is pinned. A pinned version with no minor part (for example `3`) MUST fail on winget with `ruby: WinGet needs a major.minor Ruby version such as 3.3, got '<version>'` before winget runs. On winget the version MUST NOT be passed to the backend: devy adds no `--version`, and ruby counts as installed when that RubyInstaller ID is installed at any release. On setup, when rbenv is available, it MUST run `rbenv local <version>`. Without a pinned version, an existing `.ruby-version` is left alone, and otherwise 3.3.6 is used. If a `Gemfile` exists it MUST run `bundle install`, preferring `$RBENV_ROOT/shims/bundle`, stamped in `.bundle/.devy_stamp` against `Gemfile.lock` or `Gemfile`. It contributes `RBENV_ROOT` (`$RBENV_ROOT`, or `~/.rbenv`) and prepends `<rbenv_root>/bin` and `<rbenv_root>/shims` to PATH. On brew, apt and nix, ruby counts as installed when `rbenv` is on PATH and either the pinned `version` is installed in rbenv or, with no pinned version, rbenv lists any Ruby version (`rbenv versions --bare` is non-empty); in that case 3.3.6 is not installed. The lock version is the pinned `version`, or else the output of `rbenv local`; it is not read from the installed Ruby. On winget the version is looked up through the package manager under the name `ruby`, not the RubyInstaller id. Its lock source is always `rbenv`, including on winget.

#### Scenario: Gemfile present
- **WHEN** the project has a `Gemfile` and `Gemfile.lock` and no matching stamp
- **THEN** devy runs `bundle install` in the project root and writes `.bundle/.devy_stamp`

#### Scenario: Existing .ruby-version respected
- **WHEN** ruby has no `version` in `devy.yml` and the project has `.ruby-version`
- **THEN** devy does not run `rbenv local`

#### Scenario: Another Ruby already in rbenv
- **WHEN** ruby has no `version`, the project has no `.ruby-version`, and rbenv has only Ruby 3.2.2 installed
- **THEN** devy treats ruby as installed, skips `rbenv install 3.3.6`, and setup fails with "`rbenv local 3.3.6` failed — run `rbenv install 3.3.6` first"

#### Scenario: Ruby on winget without a version
- **WHEN** ruby has no `version` and the package manager is winget
- **THEN** devy runs `winget install --id RubyInstallerTeam.Ruby.3.3 --exact --accept-source-agreements --accept-package-agreements` with no `--version`

#### Scenario: Ruby on winget with a patch version
- **WHEN** ruby has `version: 3.4.1` and the package manager is winget
- **THEN** devy installs `RubyInstallerTeam.Ruby.3.4` with no `--version`, and treats ruby as installed when `winget list --id RubyInstallerTeam.Ruby.3.4 --exact` lists it

#### Scenario: Ruby on winget with only a major version
- **WHEN** ruby has `version: 3` and the package manager is winget
- **THEN** devy fails with `ruby: WinGet needs a major.minor Ruby version such as 3.3, got '3'` and does not run winget

### Requirement: Java and Kotlin modules
The java module SHALL install a JDK (apt `default-jdk` with no pinned version and `openjdk-<major>-jdk` with one, winget `Microsoft.OpenJDK.<major>`, nix `jdk<major>`, otherwise `openjdk`; major defaults to 21). On apt and winget the version only selects the package and MUST NOT be passed to the backend: devy installs with no `=<version>` or `--version`, and java counts as installed when that package is installed at any release. On brew the `version` is passed through unchanged. On setup it MUST resolve dependencies:
- `pom.xml`: `./mvnw -B dependency:resolve` (or `mvn -B dependency:resolve` without the wrapper), stamped against `pom.xml`
- otherwise `build.gradle.kts` (preferred) or `build.gradle`: `./gradlew` (or `gradle`) `--no-daemon dependencies`, stamped against that file

Both are stamped in `.devy_java_stamp`. When devy can detect a JDK home, Java MUST contribute `JAVA_HOME` and prepend `$JAVA_HOME/bin` to PATH. Detection uses `/usr/libexec/java_home` on macOS; `/usr/lib/jvm/default-java`, or a path derived from `java` on PATH, on Linux; and an existing `$JAVA_HOME` on Windows. When nothing is detected it contributes neither. The kotlin module SHALL install `kotlin` (winget `JetBrains.Kotlin`) and resolve Gradle dependencies, stamped in `.devy_kotlin_stamp`.

#### Scenario: Maven project with wrapper
- **WHEN** the project has `pom.xml` and `./mvnw`
- **THEN** devy resolves dependencies with `./mvnw`

#### Scenario: Pinned Java on apt
- **WHEN** java has `version: 17` and the package manager is apt
- **THEN** devy runs `apt-get -y install -- openjdk-17-jdk` with no `=17`, and treats java as installed when `openjdk-17-jdk` is installed at any version

#### Scenario: Unpinned Java on apt
- **WHEN** java has no `version` and the package manager is apt
- **THEN** devy installs `default-jdk`

#### Scenario: Pinned Java on winget
- **WHEN** java has `version: 17` and the package manager is winget
- **THEN** devy installs `Microsoft.OpenJDK.17` with no `--version`

### Requirement: Other language runtime modules
These modules SHALL install through the package manager (winget id in parentheses) and run their project dependency step with a stamp file, only when the listed manifest exists:
- elixir (`Erlang-Solutions.Elixir`): installs erlang first if missing; runs `mix deps.get` against `mix.lock`/`mix.exs`, stamp `.devy_elixir_stamp`
- erlang (`Erlang-Solutions.Erlang`): runs `rebar3 get-deps` against `rebar.lock`/`rebar.config`, stamp `.devy_erlang_stamp`
- dotnet (brew/default `dotnet`, apt `dotnet-sdk-<M>.0`, nix `dotnet-sdk_<M>`, winget `Microsoft.DotNet.SDK.<M>`, M defaults to 8): runs `dotnet restore` for a `.sln` (preferred) or `.csproj`, stamp `.devy_dotnet_stamp`. On apt and winget the version only selects the package and MUST NOT be passed to the backend, and dotnet counts as installed when that package is installed at any release. On brew it is passed through unchanged.
- dart (`Dart.Dart`): runs `dart pub get` against `pubspec.lock`/`pubspec.yaml`, stamp `.devy_dart_stamp`
- zig (`zig-lang.zig`): runs `zig build` when `build.zig.zon` exists, stamp `.devy_zig_stamp`
- crystal (`Manas.Crystal`): runs `shards install` against `shard.lock`/`shard.yml`, stamp `.devy_crystal_stamp`

#### Scenario: Elixir project
- **WHEN** the project has `mix.exs` and no matching stamp
- **THEN** `devy up` runs `mix deps.get`

#### Scenario: Pinned dotnet on apt
- **WHEN** dotnet has `version: 8` and the package manager is apt
- **THEN** devy runs `apt-get -y install -- dotnet-sdk-8.0` with no `=8`, and treats dotnet as installed when `dotnet-sdk-8.0` is installed at any version

#### Scenario: Pinned dotnet on winget
- **WHEN** dotnet has `version: 8.0.100` and the package manager is winget
- **THEN** devy installs `Microsoft.DotNet.SDK.8` with no `--version`

#### Scenario: Pinned dotnet on brew
- **WHEN** dotnet has `version: 8` and the package manager is brew
- **THEN** devy still passes the version to brew, which installs the `dotnet@8` formula
