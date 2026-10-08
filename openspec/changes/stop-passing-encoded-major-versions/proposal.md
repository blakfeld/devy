# Proposal

## Why

For dotnet, java and ruby, devy puts the version line into the backend package ID (`dotnet-sdk-8.0`, `Microsoft.DotNet.SDK.8`, `Microsoft.OpenJDK.17`, `RubyInstallerTeam.Ruby.<n>`), then passes the same `devy.yml` version to the backend as an exact pin. Neither apt nor WinGet has a package version as short as `8` or `17`: WinGet lists `Microsoft.OpenJDK.17` as `17.0.x.y` and `Microsoft.DotNet.SDK.8` as `8.0.xxx`, and apt versions look like `8.0.100-0ubuntu1`. So any pinned dotnet or java version fails to install on apt and WinGet, and on apt the exact-match installed check is never true, so the install is retried on every `devy up`. Java on apt installs `default-jdk=<version>`, which fails the same way. Ruby on WinGet is broken even without a pin: devy installs `RubyInstallerTeam.Ruby.3`, but the winget-pkgs catalogue only has major.minor IDs (`RubyInstallerTeam.Ruby.3.3`, `RubyInstallerTeam.Ruby.3.4`, ...). Its versions look like `3.3.10-1`, so a pinned `3.3.6` would not match either.

## What Changes

- dotnet on apt and WinGet, java on WinGet and ruby on WinGet: the version is used only to pick the package ID. It is no longer passed to the backend, so `apt-get install dotnet-sdk-8.0` and `winget install --id Microsoft.OpenJDK.17` carry no `=<version>` or `--version`. The installed check asks only whether that ID is installed. The backend installs the newest release of that line.
- java on apt: a pinned version installs `openjdk-<major>-jdk` with no version pin. Without a pin it still installs `default-jdk`.
- ruby on WinGet: the ID is `RubyInstallerTeam.Ruby.<major>.<minor>`, taken from the pinned version, or `3.3` (from the 3.3.6 default) without one. A version without a minor part, such as `3`, fails with `ruby: WinGet needs a major.minor Ruby version such as 3.3, got '3'` instead of guessing a line.
- brew and nix are unchanged: brew still uses the version as a formula pin (`dotnet@8`, `openjdk@17`), and nix already maps it to an attribute.
- **BREAKING** (minor): on apt and WinGet a full version such as `8.0.100` no longer pins an exact build. It now only selects the 8 line. That pin rarely worked (WinGet dotnet only), and the exact form differs by backend.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `dependency-modules`: "Package names per package manager" notes that modules may drop the version when their package name already carries it; "Ruby module" uses major.minor WinGet IDs and drops the version; "Java and Kotlin modules" uses `openjdk-<major>-jdk` on apt for a pinned version and drops the version on apt and WinGet; "Other language runtime modules" drops the dotnet version on apt and WinGet.

## Impact

- Code: `src/modules/dotnet.rs`, `src/modules/java.rs`, `src/modules/ruby.rs`, and possibly a small helper beside `pm_dep` in `src/modules/helpers.rs`. Backends (`src/package_manager/apt.rs`, `winget.rs`) are unchanged.
- Docs: README package-name table (java apt row, ruby WinGet `RubyInstallerTeam.Ruby.3.3`).
- Lock: apt and WinGet version lookups still use the `devy.yml` name and are unaffected.
