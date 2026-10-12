# Spec Delta

## MODIFIED Requirements

### Requirement: Java and Kotlin modules
The java module SHALL install a JDK (apt `default-jdk`, winget `Microsoft.OpenJDK.<major>`, nix `jdk<major>`, otherwise `openjdk`; major defaults to 21). The `version` is also passed to the backend unchanged, so `version: 21` on apt installs `default-jdk=21`. On setup it MUST resolve dependencies:
- `pom.xml`: `./mvnw -B dependency:resolve` (or `mvn -B dependency:resolve` without the wrapper), stamped against `pom.xml`
- otherwise `build.gradle.kts` (preferred) or `build.gradle`: `./gradlew` (or `gradle`) `--no-daemon dependencies`, stamped against that file

Both are stamped in `.devy_java_stamp`. When devy finds a JDK home, Java MUST contribute `JAVA_HOME` and prepend `$JAVA_HOME/bin` to PATH; when it finds none it contributes neither. On brew and nix the real JDK home is the directory two levels above the real path (all symlinks resolved) of the installed `java` binary. devy exports it through a path that survives upgrades: the same location relative to `<brew prefix>/opt/<formula>` (brew) or `<project_root>/.devy/nix-profile` (nix) as the real home has in its keg or store object. The exported home MUST contain `bin/java`. Which JDK is used depends on the backend:
- brew: `<brew prefix>/opt/<formula>/bin/java`, where `<formula>` is the formula devy installs for the dependency (`openjdk`, or `openjdk@<version>` for a pinned `version` from devy.yml) and `<brew prefix>` is the prefix of the `brew` devy runs. A home inside the project is never used.
- nix: `<project_root>/.devy/nix-profile/bin/java`, used only when the profile passes devy's nix profile checks and the real home is inside `/nix/store` (and not `/nix/store` itself). When the profile does not contain the home's relative path, the real `/nix/store` home is exported.
- apt: `/usr/lib/jvm/default-java` when it exists, otherwise a home derived from the `java` on PATH.
- winget: an existing `$JAVA_HOME`, used as is.

On brew and nix devy MUST NOT fall back to `/usr/libexec/java_home`, `/usr/lib/jvm/default-java` or the `java` on PATH, so a JDK other than the one installed for the dependency is never exported. The kotlin module SHALL install `kotlin` (winget `JetBrains.Kotlin`) and resolve Gradle dependencies, stamped in `.devy_kotlin_stamp`.

#### Scenario: Maven project with wrapper
- **WHEN** the project has `pom.xml` and `./mvnw`
- **THEN** devy resolves dependencies with `./mvnw`

#### Scenario: Homebrew keg-only JDK on a Mac with no other JDK
- **WHEN** the backend is brew, `java` is declared without a version, `openjdk` is installed under `/opt/homebrew/opt/openjdk`, and `/usr/libexec/java_home` finds no JDK
- **THEN** devy sets `JAVA_HOME` to `/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home` (the home of `/opt/homebrew/Cellar/openjdk/<version>`, reached through `opt`) and prepends that directory's `bin` to PATH

#### Scenario: Pinned version wins over another installed JDK
- **WHEN** the backend is brew, `java` is declared with `version: "17"`, `openjdk@17` is installed, and `/usr/libexec/java_home` reports a JDK 21 elsewhere
- **THEN** `JAVA_HOME` is `<brew prefix>/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home`, not the JDK 21

#### Scenario: JDK from the nix profile
- **WHEN** the backend is nix, `.devy/nix-profile/bin/java` resolves to `/nix/store/<hash>-openjdk-21/lib/openjdk/bin/java`, and the profile contains `lib/openjdk`
- **THEN** devy sets `JAVA_HOME` to `<project_root>/.devy/nix-profile/lib/openjdk`, even if `/usr/lib/jvm/default-java` exists

#### Scenario: JDK from the nix store when the profile lacks its home
- **WHEN** the backend is nix, `.devy/nix-profile/bin/java` resolves to `/nix/store/<hash>-openjdk-21/lib/openjdk/bin/java`, and the profile has no `lib/openjdk`
- **THEN** devy sets `JAVA_HOME` to `/nix/store/<hash>-openjdk-21/lib/openjdk`

#### Scenario: Requested JDK not installed
- **WHEN** the backend is brew and `<brew prefix>/opt/openjdk/bin/java` does not exist (for example `devy check` before the first `devy up`), while another JDK is installed on the system
- **THEN** devy contributes neither `JAVA_HOME` nor a Java PATH entry
