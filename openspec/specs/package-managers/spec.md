# package-managers Specification

## Purpose
Defines how devy selects a package manager for the current platform and how each backend (Nix, Homebrew, apt, WinGet) checks, bootstraps, and installs dependencies.

## Requirements

### Requirement: Package manager selection
devy SHALL select the package manager from the `package_manager` key in `devy.yml` (`auto`, `nix`, `brew`, or `apt`; default `auto`) and MUST reject a backend that is unavailable on the current OS. `winget` is not a valid value; WinGet is only reachable through `auto` on Windows. Selection happens on every command that needs a package manager (for example `up`, `check`, `status`), so the `auto` warning is printed by each of them.

#### Scenario: Auto on macOS or Linux
- **WHEN** `package_manager` is unset or explicitly `auto` and devy runs on macOS or Linux
- **THEN** devy uses Nix and warns `No package_manager set in devy.yml — defaulting to nix. Add `package_manager: brew` (macOS) or `package_manager: apt` (Linux) to keep using your system package manager.` (the same warning appears even when `auto` is set explicitly)

#### Scenario: Auto on Windows
- **WHEN** `package_manager` is unset and devy runs on Windows
- **THEN** devy uses WinGet without a warning

#### Scenario: Brew off macOS
- **WHEN** `package_manager: brew` is set on Linux or Windows
- **THEN** devy fails with `package_manager: brew is only available on macOS`

#### Scenario: Apt off Linux
- **WHEN** `package_manager: apt` is set on macOS or Windows
- **THEN** devy fails with `package_manager: apt is only available on Linux`

#### Scenario: Nix on Windows
- **WHEN** `package_manager: nix` is set on Windows
- **THEN** devy fails with `package_manager: nix is not supported on Windows`

#### Scenario: Invalid value
- **WHEN** `package_manager: pacman` is set
- **THEN** loading `devy.yml` fails with a parse error

#### Scenario: winget named explicitly
- **WHEN** `package_manager: winget` is set
- **THEN** loading `devy.yml` fails with a parse error

### Requirement: Ensuring availability and bootstrap
Before installing anything, `devy up` SHALL verify the package manager is installed. If it is missing and `--bootstrap` was given, devy SHALL attempt to install it using verified installer downloads. Otherwise devy MUST fail with `<pm> is not installed. Re-run with --bootstrap to install automatically.`, followed by an `Install manually: <url>` line for nix (`https://nixos.org/download/`) and brew (`https://brew.sh`). apt and winget show no URL.

#### Scenario: Missing without bootstrap
- **WHEN** Nix is not installed and the user runs `devy up`
- **THEN** devy fails with a message that nix is not installed, suggesting re-running with `--bootstrap` or installing manually

#### Scenario: Nix bootstrap
- **WHEN** Nix is not installed and the user runs `devy up --bootstrap`
- **THEN** devy downloads the Determinate Systems installer for the release pinned in devy (`https://install.determinate.systems/nix/tag/v<pinned>`), verifies its SHA-256, and runs it with `install --no-confirm`

#### Scenario: Homebrew bootstrap
- **WHEN** Homebrew is not installed and the user runs `devy up --bootstrap` with `package_manager: brew`
- **THEN** devy downloads `install.sh` from the Homebrew/install commit pinned in devy, verifies its SHA-256, and runs it with `bash`, failing with `Homebrew installation failed` if it exits non-zero

#### Scenario: Apt or WinGet bootstrap
- **WHEN** apt or winget is missing and `--bootstrap` is given
- **THEN** devy fails, because those package managers cannot be bootstrapped: apt with `apt-get is not available; please ensure Ubuntu/Debian is properly installed`, winget with `winget is not available. Install App Installer from the Microsoft Store or update to a recent version of Windows 10/11.`

### Requirement: Project-local Nix profile
The Nix backend SHALL install packages into the project-local profile `<project_root>/.devy/nix-profile` using `nix profile install --profile <profile> nixpkgs#<attr>`. It SHALL fall back to `nix-env --profile <profile> -iA nixpkgs.<attr>` when the `nix profile` command is unavailable. `<attr>` SHALL be the module's versioned nixpkgs attribute when the dependency has a version that maps to one, and its unversioned attribute otherwise.

#### Scenario: Install jq
- **WHEN** `devy up` installs `jq` with the nix backend
- **THEN** `jq` appears at `.devy/nix-profile/bin/jq` and nothing is installed into the user's global profile

#### Scenario: Versioned install
- **WHEN** `devy up` installs `node` with `version: "22"` using the nix backend
- **THEN** devy runs `nix profile install --profile <profile> nixpkgs#nodejs_22`

### Requirement: Nix installed check is project-scoped
The Nix backend SHALL treat a package as installed only if it appears in the project-local profile, matching the entry's attribute path against the exact attribute devy would install (versioned or not). When the profile does not exist yet, it MUST report nothing as installed. When the version was pinned from `devy.lock`, nix installs attributes rather than exact versions, so the check works at the attribute's granularity. An installed unversioned attribute SHALL also count as installed when its version maps to the same versioned attribute as the locked version. For a module without versioned attributes, the installed package satisfies any locked version. A later `devy up` therefore never reinstalls a package under its versioned name.

#### Scenario: Globally installed package
- **WHEN** `jq` is installed in the user's global Nix profile but `.devy/nix-profile` does not exist
- **THEN** devy reports `jq` as not installed and installs it into the project profile

#### Scenario: Versioned attribute recognized
- **WHEN** `nodejs_22` is in the project profile and `node` is declared with `version: "22"`
- **THEN** devy reports `node` as installed and does not reinstall it on the next `devy up`

#### Scenario: Mapped attribute recognized
- **WHEN** `mysql84` (package name `mysql`) is in the project profile and `mysql` is declared
- **THEN** devy reports `mysql` as installed

#### Scenario: Version changed
- **WHEN** `nodejs_22` is in the project profile and `node` is now declared with `version: "24"`
- **THEN** devy reports `node` as not installed and installs `nodejs_24`

#### Scenario: Locked patch version from a newer nixpkgs
- **WHEN** `nodejs` 24.20.0 is in the project profile and `devy.lock` records node `24.21.0`, written by a teammate whose nixpkgs is newer
- **THEN** devy reports `node` as installed and installs nothing

### Requirement: Nix profile on PATH first
When using the Nix backend, devy SHALL put `<project_root>/.devy/nix-profile/bin` ahead of every module-contributed PATH entry in the generated environment.

#### Scenario: PATH ordering with python
- **WHEN** the backend is nix and `python` contributes `.venv/bin`
- **THEN** `.devy/nix-profile/bin` precedes `.venv/bin` in the activated `PATH`

### Requirement: Locating the Nix binary
The Nix backend SHALL find `nix` on `PATH`, ignoring `PATH` entries inside the project root, then in `/nix/var/nix/profiles/default/bin`, `~/.nix-profile/bin`, and `/run/current-system/sw/bin`.

#### Scenario: Nix installed but not on PATH
- **WHEN** `nix` is absent from `PATH` but present at `/nix/var/nix/profiles/default/bin/nix`
- **THEN** devy uses that binary

#### Scenario: Project-local nix ignored
- **WHEN** the activated environment puts `<project_root>/.devy/nix-profile/bin` first on PATH and it contains `nix`
- **THEN** devy uses the system `nix` instead

### Requirement: Homebrew version pinning
The Homebrew backend SHALL install a versioned formula `<name>@<version>` when the version comes from `devy.yml`, has at most two dot-separated components and contains no `_`, and SHALL otherwise install the bare formula name. A version that came from `devy.lock` (`resolved_version`) SHALL NOT select a formula, whatever its shape, and SHALL NOT produce a warning: the lock is untrusted input, and a lock value such as `16` must not switch the install to `node@16`.

#### Scenario: Major version pin
- **WHEN** `node` is declared with `version: "20"`
- **THEN** devy installs `node@20`

#### Scenario: Full resolved version
- **WHEN** `node` resolves to `20.11.0` from `devy.lock`
- **THEN** devy installs the formula `node`

#### Scenario: Short lock version does not select a formula
- **WHEN** `node` has no `version` in `devy.yml` and `devy.lock` records `resolved_version: "16"`
- **THEN** devy installs the formula `node`, not `node@16`, without a warning

### Requirement: Homebrew taps
When a dependency sets `tap`, the Homebrew backend SHALL validate it as `org/repo` (exactly one `/`, each part non-empty, starting with a letter or digit, limited to letters, digits, `-`, `_`, `.`, and not equal to `.` or `..`) and run `brew tap -- <tap>` before installing. Taps are listed in the executable entry listing (project-config). A dependency name can never select a tap, because dependency names cannot contain `/`.

#### Scenario: Invalid tap
- **WHEN** a dependency sets `tap: "evil; rm -rf /"`
- **THEN** config validation fails before anything is installed

#### Scenario: Valid tap
- **WHEN** `mongodb` sets `tap: mongodb/brew`
- **THEN** devy runs `brew tap -- mongodb/brew` and then installs `mongodb-community`

#### Scenario: Tap smuggled through the name
- **WHEN** a dependency is named `evilorg/tap/formula`
- **THEN** config loading fails with an invalid-name error and `brew` is never run

### Requirement: Homebrew installed check
The Homebrew backend SHALL treat a formula as installed when `brew list --versions <formula>` produces output, and SHALL report the second token of that output as the resolved version.

#### Scenario: Installed formula
- **WHEN** `brew list --versions jq` prints `jq 1.7.1`
- **THEN** devy reports `jq` as installed with version `1.7.1`

### Requirement: Apt backend
The apt backend SHALL count as available when `/usr/bin/apt-get` exists. It SHALL treat a package as installed when `dpkg-query -W -f=${Status}|${Version} -- <name>` reports `install ok installed` (and, when a version is pinned, the installed version matches exactly), and SHALL install with `/usr/bin/sudo /usr/bin/apt-get -y install -- <name>[=<version>]`. The version is passed through as is, so partial versions such as `20` may not resolve. devy never runs `apt-get update` before installing. A failed install reports `` `sudo apt-get <args>` failed — check the output above for details``.

#### Scenario: Pinned version mismatch
- **WHEN** `redis-server` is installed at a different version than the pinned one
- **THEN** devy reports it as not installed and installs `redis-server=<version>`

#### Scenario: Local package file rejected
- **WHEN** `devy.yml` lists `./evil.deb` and the backend is apt
- **THEN** config loading fails and `sudo` is never invoked

### Requirement: WinGet backend
The WinGet backend SHALL check installation with `winget list --id <id> --exact` and install with `winget install --id <id> --exact --accept-source-agreements --accept-package-agreements`, adding `--version <v>` when a version is set.

#### Scenario: Install with version
- **WHEN** `node` is declared with `version: "20.11.0"` on Windows
- **THEN** devy runs `winget install --id OpenJS.NodeJS --exact ... --version 20.11.0`

### Requirement: Service config directories
Package managers SHALL expose a per-service config directory where one exists: brew uses `$(brew --prefix <svc>)/etc`; apt uses `/etc/mysql/conf.d` for mysql and mariadb and `/etc/postgresql/<highest version>/main/conf.d` for postgresql; Nix and WinGet provide none.

#### Scenario: Apt postgres version directory
- **WHEN** `/etc/postgresql/14` and `/etc/postgresql/16` exist
- **THEN** the apt config directory for postgresql is `/etc/postgresql/16/main/conf.d`

### Requirement: Unhonored nix versions are reported
When the nix backend cannot map a dependency's `version` from `devy.yml` to a versioned nixpkgs attribute, `devy up` SHALL install the unversioned attribute and warn `<dep>: version <v> is not supported by the nix backend — installing the nixpkgs default`. A version that came only from `devy.lock` pinning MUST NOT produce this warning.

#### Scenario: Explicit unmapped version
- **WHEN** `jq` is declared with `version: "1.6"` under nix
- **THEN** `devy up` installs `nixpkgs#jq` and prints the version-not-supported warning

#### Scenario: Lock-pinned version
- **WHEN** `jq` has no `version` in `devy.yml` and `devy.lock` records `resolved_version: 1.7.1`
- **THEN** `devy up` prints no version warning for `jq`

### Requirement: MariaDB wins nix profile conflicts with MySQL
`mysql` and `mariadb` ship many of the same file names (`mysql`, `mysqldump`, `mysqld` and others). The nix backend SHALL install `mariadb` with profile priority 4, so that in the project profile those names resolve to MariaDB whichever package is installed first. The `mysql` service SHALL still run MySQL's own `mysqld` from the `mysql` package, not from the merged profile.

#### Scenario: Both installed
- **WHEN** `devy.yml` lists `mysql` and `mariadb` and `devy up` runs under nix
- **THEN** devy runs `nix profile install nixpkgs#mariadb --priority 4`, both packages are installed, `.devy/nix-profile/bin/mysqld` is MariaDB's, and the `mysql` service reports a MySQL server version

### Requirement: Unfree nix packages
Modules whose nixpkgs package is unfree SHALL declare it: `mongodb` (`mongodb-ce`), `elasticsearch`, `vault` and `terraform`. When the nix backend installs a declared-unfree package, it SHALL allow unfree packages for that install only, and print `<dep>: nixpkgs#<attr> is unfree — allowing unfree packages for this install`. It MUST NOT allow unfree packages for any other install. It MUST NOT change the user's global Nix configuration.

#### Scenario: MongoDB installs under nix
- **WHEN** `devy up` installs `mongodb` with the nix backend
- **THEN** devy prints that `nixpkgs#mongodb-ce` is unfree, installs it into `.devy/nix-profile`, and the install succeeds without any configuration in `devy.yml`

#### Scenario: Free packages stay free-only
- **WHEN** `devy up` installs `redis` and `vault` with the nix backend
- **THEN** only the `vault` install allows unfree packages, and no unfree notice is printed for `redis`

#### Scenario: Unfree dependency of a free package
- **WHEN** a module that does not declare an unfree package resolves to a nixpkgs attribute that nixpkgs refuses as unfree
- **THEN** the install fails with nix's own unfree error, and devy does not retry with unfree allowed

### Requirement: Insecure nix packages
Modules whose nixpkgs package nixpkgs marks insecure SHALL declare it: `elasticsearch`.
- **Install:** when the nix backend installs a declared-insecure package, it SHALL allow insecure packages for that install only. It SHALL print the warning `<dep>: nixpkgs#<attr> is marked insecure by nixpkgs — allowing insecure packages for this install`.
- **Scope:** it MUST NOT allow insecure packages for any other install, and MUST NOT change the user's global Nix configuration.
- **Unfree:** a package that is both unfree and insecure SHALL have both allowed for that install.

#### Scenario: Elasticsearch installs under nix
- **WHEN** `devy up` installs `elasticsearch` with the nix backend
- **THEN** devy prints the unfree notice and the insecure warning for `nixpkgs#elasticsearch`, and the install into `.devy/nix-profile` succeeds without any configuration in `devy.yml`

#### Scenario: Other packages stay secure-only
- **WHEN** `devy up` installs `redis` and `mongodb` with the nix backend
- **THEN** neither install allows insecure packages, and no insecure warning is printed

### Requirement: Nix reports installed versions
The Nix backend SHALL report a package's installed version from its profile entry. When the entry records no version, as with Nix ≥ 2.20, the backend SHALL derive the version from the name of the entry's main output store path. It splits at the first `-` that is followed by a digit, as Nix itself does: `redis-8.6.3` gives `8.6.3`, and `apache-kafka-2.13-4.3.1` gives `2.13-4.3.1`. The main output is the store path whose name is a prefix of the entry's other outputs' names (so `mysql-8.4.11` rather than `mysql-8.4.11-man`). When no version can be derived, the reported version SHALL be absent. It MUST NOT be the string `unknown`.

#### Scenario: Version from a v3 profile entry
- **WHEN** the project profile contains `redis` with store path `/nix/store/<hash>-redis-8.6.3` and no version field
- **THEN** devy reports redis as installed at `8.6.3`, and `devy up` prints `redis@8.6.3 already installed`

#### Scenario: Multiple outputs
- **WHEN** the profile entry for `mysql84` lists `<hash>-mysql-8.4.11-man` before `<hash>-mysql-8.4.11`
- **THEN** devy reports version `8.4.11`

### Requirement: Legacy nix-env style tracks installed attributes
When the Nix backend uses the legacy `nix-env` style, devy SHALL record each nixpkgs attribute it installs, together with the exact derivation name that install produced, in `<project_root>/.devy/nix-env-attrs.json`.
- **Installed:** an attribute is installed when the file records it and the project profile still contains a package with that derivation name. Its version SHALL be that package's version.
- **No record:** for an attribute with no record, which happens in profiles created before this file existed, devy SHALL fall back to matching a profile package whose name equals the attribute.
- **Missing or unreadable file:** devy SHALL treat it as empty and MUST NOT fail.

#### Scenario: Renamed attribute recognized
- **WHEN** devy installed `mysql84` with `nix-env` (package name `mysql`) and `devy up` runs again
- **THEN** devy reports `mysql` as installed and does not run `nix-env -iA` again

#### Scenario: Versioned attribute does not satisfy the unversioned one
- **WHEN** devy installed `nodejs_22` with `nix-env`, and `node` is now declared without a version
- **THEN** devy reports `node` as not installed and installs `nodejs`

#### Scenario: Profile from before the manifest
- **WHEN** a `nix-env` project profile contains `jq` and `.devy/nix-env-attrs.json` does not exist
- **THEN** devy reports `jq` as installed by its package name

#### Scenario: Package removed outside devy
- **WHEN** the manifest records `redis` but the user removed it with `nix-env -e redis`
- **THEN** devy reports `redis` as not installed and reinstalls it

### Requirement: Package arguments end option parsing
Every package-manager invocation that takes package names, versions, taps or IDs from configuration or the lock SHALL pass them after an end-of-options marker (`--`) where the tool supports one: `apt-get install`, `dpkg-query -W`, `brew install`, `brew tap`, `brew list`, `npm install -g`, `rustup target add`, `rustup component add`, and `gcloud components install`. For tools without such a marker (winget), values SHALL be passed only as the argument of a named option (`--id <id>`, `--version <version>`). `rbenv install` and `rbenv local` have no usable marker (`rbenv install` treats arguments after `--` as configure options, and `rbenv local` would record `--` itself), so a Ruby version passed to them SHALL be a plain name: an ASCII letter or digit followed only by letters, digits, `.`, `_` or `-`; anything else fails with `invalid Ruby version '<v>'` before rbenv runs. `rbenv install` and `rbenv prefix` SHALL run with `/` as their working directory, so a version name never resolves to a ruby-build definition file in the project or another writable directory. Values SHALL have already passed project-config validation.

#### Scenario: apt receives a separator
- **WHEN** devy installs `redis-server` at version `7.0.15-1` with apt
- **THEN** it runs `/usr/bin/sudo /usr/bin/apt-get -y install -- redis-server=7.0.15-1`

#### Scenario: Path-like Ruby version
- **WHEN** ruby is pinned to `./evil` (or `../x`, `-x`)
- **THEN** devy fails with `invalid Ruby version` and never runs `rbenv install`

#### Scenario: rbenv runs outside the project
- **WHEN** devy runs `rbenv install --skip-existing 3.3.6` or `rbenv prefix 3.3.6`
- **THEN** the command's working directory is `/`, so a `3.3.6` file in the project is never read as a ruby-build definition

### Requirement: Installer downloads are verified
Every script or archive devy downloads and runs (the Nix and Homebrew bootstrap installers, the deno, bun and rustup installers, and the gcloud CLI archive) SHALL be fetched with HTTPS only (TLS 1.2 or later, and no redirect to another scheme) from a URL that pins an exact release or commit, with a size cap. The download SHALL be saved to a private temporary file, and its SHA-256 compared against a digest built into devy before it runs. On mismatch devy SHALL delete the file and fail with `<installer> checksum mismatch: expected <hex>, got <hex>`, without running it. devy SHALL NOT pipe a download directly into a shell. Installers SHALL run from a private directory with a scrubbed environment (an allowlist of locale, terminal, home and proxy variables) and a PATH without relative or project-local entries, so repository-controlled variables such as `NIX_INSTALLER_BINARY_ROOT`, `RUSTUP_UPDATE_ROOT` or `BASH_ENV` cannot redirect them. `HOME` SHALL be `$HOME` only when it is an existing absolute directory outside the project owned by the current user, and otherwise the user database entry (devy looks for installed tools under the same home), and `SHELL`, `SUDO_ASKPASS` and CA-bundle variables (`SSL_CERT_FILE`, `CURL_CA_BUNDLE`) SHALL NOT be passed; the rustup installer additionally receives `CARGO_HOME`/`RUSTUP_HOME` when they are absolute and outside the project. `CI` and `NONINTERACTIVE` SHALL be passed through, since they only suppress prompts. The CA-bundle variables are dropped because the installers' second-stage downloads carry no digest devy knows, so a repository-supplied trust root could intercept them; users behind a TLS-inspecting proxy install its root in the system trust store instead. Because bash fills an unset `SHELL` from the user database, the bun installer SHALL be run with `SHELL=/bin/sh`, so it prints PATH instructions instead of editing a shell rc file.

#### Scenario: Tampered installer
- **WHEN** the downloaded deno installer's SHA-256 differs from the pinned digest
- **THEN** devy fails with the checksum-mismatch error and the script is not executed

#### Scenario: Truncated download
- **WHEN** the connection drops partway through downloading the bun installer
- **THEN** no part of the script runs

### Requirement: Homebrew never installs local files
Every `brew` invocation SHALL set `HOMEBREW_FORBID_PACKAGES_FROM_PATHS=1` and run with `/` as its working directory, and the Homebrew backend SHALL refuse a formula name that ends in `.rb`, `.json`, `.tar.gz` or `.tgz` or contains `.bottle.` (config validation rejects such dependency names too), so neither a formula or cask file nor a bottle archive in the project can be installed by a bare name.

#### Scenario: Local bottle name
- **WHEN** `devy.yml` lists a dependency named `x.arm64_sonoma.bottle.tar.gz`
- **THEN** config loading fails with an invalid-name error and `brew` is never run

### Requirement: Homebrew formula bin directories on PATH
For each dependency that devy installs through the Homebrew backend, devy SHALL contribute `<brew prefix>/opt/<formula>/bin` to the project PATH when that directory exists, where `<formula>` is the formula devy installs for the dependency (after module name mapping and `name@version` pinning, as in Homebrew version pinning) and `<brew prefix>` is the prefix of the `brew` devy runs (or, when that has no `opt` directory because `brew` is a link from elsewhere, the prefix of its real path), and neither the prefix nor the directory is ever inside the project. These entries come after the backend's own entries and after module entries, in dependency order, so a module's entry (such as a Python virtualenv's `bin`) wins over the formula's. Dependencies that are not installed through Homebrew (for example rust via rustup, or deno and bun via their installers) contribute none. Finding these directories SHALL NOT run `brew`. This makes keg-only formulae, which Homebrew does not link into `<brew prefix>/bin` (versioned formulae such as `node@22`, `postgresql@16` and `mysql@8.4`, and `openjdk`), reachable once the environment is active.

#### Scenario: Versioned keg-only formula
- **WHEN** the backend is brew, `postgres` is declared with `version: "16"`, and `/opt/homebrew/opt/postgresql@16/bin` exists
- **THEN** the project PATH entries include `/opt/homebrew/opt/postgresql@16/bin`, so `psql` resolves to PostgreSQL 16 in the activated shell

#### Scenario: Pinned formula ahead of a linked version
- **WHEN** the backend is brew, `node` is declared with `version: "22"`, `node@22` is installed, and an unversioned `node` 26 is linked into `/opt/homebrew/bin`
- **THEN** `/opt/homebrew/opt/node@22/bin` is on the project PATH ahead of `/opt/homebrew/bin`, so `node --version` reports 22

#### Scenario: Virtualenv ahead of Homebrew Python
- **WHEN** the backend is brew, `python` is declared, and `/opt/homebrew/opt/python/bin` exists
- **THEN** the project's `.venv/bin` comes before `/opt/homebrew/opt/python/bin` on the project PATH, so `pip` installs into the virtualenv

#### Scenario: Formula not installed yet
- **WHEN** the backend is brew and `/opt/homebrew/opt/node@22/bin` does not exist
- **THEN** devy contributes no PATH entry for it

#### Scenario: Not installed through Homebrew
- **WHEN** the backend is brew and the only dependency is `rust`
- **THEN** devy contributes no Homebrew PATH entry
