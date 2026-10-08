# Spec Delta

## ADDED Requirements

### Requirement: Setup tools resolved to a full path
Before running a module setup command by name (for example `npm`, `pnpm`, `yarn`, `bundle`, `mvn`, `gradle`, `mix`, `rebar3`, `gcloud`, `rustup`), devy SHALL resolve the name to a full path. It searches the PATH defined by "Setup tools run from devy's own PATH", skipping relative entries and entries inside the project root, and then runs that path. On Windows the lookup MUST try each extension in `PATHEXT`, so a `.cmd` or `.bat` shim such as `npm.cmd` is found and run. When no match exists, the step MUST fail with `` `<tool>` not found on PATH `` and name the setup step that needed it. Arguments passed to a `.cmd` or `.bat` shim MUST reach it unchanged. An argument that cannot be passed safely to a batch file MUST fail the step instead of being reinterpreted by `cmd.exe`.

#### Scenario: npm shim on Windows
- **WHEN** on Windows the project has `package.json`, the only npm on PATH is `C:\Program Files\nodejs\npm.cmd`, and there is no matching stamp
- **THEN** `devy up` runs `npm.cmd install` in the project root and writes `.devy_node_local_stamp`

#### Scenario: Global packages through a shim
- **WHEN** on Windows node is declared with `global_packages: ["@angular/cli@^17.0.0", "pkg@>=1.0"]`
- **THEN** devy runs `npm.cmd install -g -- @angular/cli@^17.0.0 pkg@>=1.0` with every package argument passed through unchanged

#### Scenario: Elixir on Windows
- **WHEN** on Windows the project has `mix.exs`, `mix` is installed only as `mix.bat`, and there is no matching stamp
- **THEN** `devy up` runs `mix.bat deps.get`

#### Scenario: Tool missing
- **WHEN** the project has `Gemfile` and no `bundle` is found on PATH
- **THEN** `devy up` fails with `` `bundle` not found on PATH `` instead of an operating-system "program not found" error

#### Scenario: Project-local tool ignored
- **WHEN** PATH contains `<project_root>/bin`, which holds an `npm` executable, ahead of the system npm
- **THEN** devy runs the system npm, not `<project_root>/bin/npm`

## MODIFIED Requirements

### Requirement: Java and Kotlin modules
The java module SHALL install a JDK (apt `default-jdk`, winget `Microsoft.OpenJDK.<major>`, nix `jdk<major>`, otherwise `openjdk`; major defaults to 21). The `version` is also passed to the backend unchanged, so `version: 21` on apt installs `default-jdk=21`. On setup it MUST resolve dependencies:
- `pom.xml`: `<wrapper> -B dependency:resolve` (or `mvn -B dependency:resolve` without the wrapper), stamped against `pom.xml`
- otherwise `build.gradle.kts` (preferred) or `build.gradle`: `<wrapper>` (or `gradle`) `--no-daemon dependencies`, stamped against that file

The wrapper is the project root's `mvnw` / `gradlew` on macOS and Linux. On Windows it is `mvnw.cmd` / `gradlew.bat`, and a POSIX-only `mvnw` / `gradlew` is ignored there. The wrapper is run by its absolute path in the project root. Without a usable wrapper, `mvn` / `gradle` is resolved as described in "Setup tools resolved to a full path". Both are stamped in `.devy_java_stamp`. When devy can detect a JDK home, Java MUST contribute `JAVA_HOME` and prepend `$JAVA_HOME/bin` to PATH. Detection uses `/usr/libexec/java_home` on macOS; `/usr/lib/jvm/default-java`, or a path derived from `java` on PATH, on Linux; and an existing `$JAVA_HOME` on Windows. When nothing is detected it contributes neither. The kotlin module SHALL install `kotlin` (winget `JetBrains.Kotlin`) and resolve Gradle dependencies with the same wrapper selection, stamped in `.devy_kotlin_stamp`.

#### Scenario: Maven project with wrapper
- **WHEN** on macOS or Linux the project has `pom.xml` and `./mvnw`
- **THEN** devy resolves dependencies with `<project_root>/mvnw`

#### Scenario: Gradle wrapper on Windows
- **WHEN** on Windows the project has `build.gradle.kts`, `gradlew` and `gradlew.bat`
- **THEN** devy runs `<project_root>\gradlew.bat --no-daemon dependencies`

#### Scenario: Maven wrapper on Windows
- **WHEN** on Windows the project has `pom.xml`, `mvnw` and `mvnw.cmd`
- **THEN** devy runs `<project_root>\mvnw.cmd -B dependency:resolve`

#### Scenario: POSIX-only wrapper on Windows
- **WHEN** on Windows the project has `build.gradle` and `gradlew` but no `gradlew.bat`, and `gradle.bat` is on PATH
- **THEN** devy runs `gradle.bat --no-daemon dependencies` and does not try to run `gradlew`
