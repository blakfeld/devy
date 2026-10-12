# Design

## Context

`pm_dep` in `src/modules/helpers.rs` renames a dependency for the backend and copies `version` across. dotnet (`package_name`), java (`pkg_name`, `winget_package_id`) and ruby (`winget_package_id`) build a package ID from the version's major part and then hand both the ID and the raw version to `is_package_installed` and `install_package`. The backends then:

- apt `install_args` emits `<name>=<version>`, and `is_package_installed` requires `installed_version_matches` (exact string equality).
- WinGet `install_package` adds `--version <v>`. WinGet needs an exact catalogue version: the winget-pkgs manifests list `Microsoft.OpenJDK.17` as `17.0.x.y`, `Microsoft.DotNet.SDK.8` as `8.0.1xx`–`8.0.4xx`, and `RubyInstallerTeam.Ruby.3.3` as `3.3.8-1`…`3.3.12-1`.
- The winget-pkgs catalogue has `manifests/r/RubyInstallerTeam/Ruby/3/{0..4}`. Those are IDs `RubyInstallerTeam.Ruby.3.0`…`3.4`, and there is no `RubyInstallerTeam.Ruby.3` (verified 2026-10-08).

brew is different. Its `formula_name` turns a short user version into a formula pin (`dotnet@8`, `openjdk@17`), which is correct. nix has its own versioned-attribute path (`nix_install_attr`) and never reaches the code paths changed here.

## Goals / Non-Goals

**Goals:**
- A short pinned version (`8`, `17`, `3.4`) installs and is detected as installed on apt and WinGet for dotnet, java and ruby.
- Ruby on WinGet uses an ID that exists in the catalogue, with and without a pinned version.
- Reproduce each failure in a unit test before fixing it.

**Non-Goals:**
- Exact-build pinning for these modules on apt or WinGet.
- Fixing the general "lock lookup uses the `devy.yml` name" behaviour (spec: Default lookup), which already means these modules record no apt or WinGet lock version.
- Changing brew or nix behaviour, or any other module.

## Decisions

### D1: Drop the version whenever the ID carries it (apt and WinGet only)
Add a helper next to `pm_dep`, for example `pm_dep_unversioned(dep, name)`, that builds the same dependency with `version: None` and `version_from_lock: false`. dotnet uses it on `apt` and `winget`, java on `apt` and `winget`, and ruby on `winget`. brew and the default branch keep `pm_dep`.

*Alternative considered:* pass the version only when it has more parts than the ID encodes (so `8.0.100` would still pin on WinGet). This was rejected. Only dotnet on WinGet would gain from it. Java (`17.0.12` vs `17.0.12.7`) and Ruby (`3.3.6` vs `3.3.6-1`) would still fail, and apt never accepts these short forms. One rule that holds for every backend is easier to document and test.

*Alternative considered:* clear the version inside the apt and WinGet backends. This was rejected because the backend can't tell a version that is already in the name from a real pin (for example `golang-go=1.22.2-1`).

### D2: Lock-sourced versions are dropped too
The rule applies whatever the version's source. For these modules, apt and WinGet lock entries come from looking up the `devy.yml` name (`dotnet`, `java`), which isn't the installed package. So any lock value isn't a valid pin for the ID anyway.

### D3: Java on apt picks `openjdk-<major>-jdk` when pinned
`default-jdk=<v>` can never work, and silently ignoring the pin would install the distro default (for example 21 when 17 was asked for). Debian and Ubuntu ship `openjdk-<N>-jdk` for supported majors. Without a pin, `default-jdk` stays the package so existing setups don't change.

### D4: Ruby WinGet ID is major.minor; a major-only version is an error
The ID comes from the first two dot-separated parts of the pinned version, or `3.3` from `DEFAULT_RUBY_VERSION`. A version like `3` fails with a clear message, because guessing a minor could install an unintended line, and rbenv rejects `3` on the other backends anyway. `winget_package_id` returns `Result`, and `is_installed` and `install` propagate the error. The parts are already validated by project config (the Ruby version charset), so the ID needs no extra escaping. It still goes through `checked_winget_args`.

## Risks / Trade-offs

- [`version: 8.0.100` no longer pins an exact dotnet build on WinGet] → Document it as a minor breaking change in the proposal and the README. The ID still selects the 8 line.
- [`openjdk-<N>-jdk` may not exist on an older distro, such as `openjdk-25-jdk` on Ubuntu 22.04] → apt fails with its own "Unable to locate package" error, as it does today for other missing packages. No allowlist is added.
- [On apt, `openjdk-17-jdk` without `default-jdk` may leave no `/usr/lib/jvm/default-java`] → JAVA_HOME detection already falls back to the `java` on PATH (Linux branch of `detect_java_home`). No change is needed, but the java tests cover it.
- [An installed Ruby on WinGet with a different minor (3.3 installed, `version: 3.4` pinned) isn't detected] → This is the intended behaviour: a different line is installed side by side, as RubyInstaller supports.

## Migration Plan

No data migration. Users with `version: 3` for ruby on WinGet must change it to `3.3` (or similar). The error message says so.

## Open Questions

- Should devy warn when a user gives a version more precise than the ID carries (for example `8.0.100`), since only the line is honoured? The default for this change is no warning. Adding one later wouldn't change the install behaviour.
