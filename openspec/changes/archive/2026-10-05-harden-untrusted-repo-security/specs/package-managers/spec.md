# Spec Delta

## ADDED Requirements

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

## MODIFIED Requirements

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

### Requirement: Homebrew taps
When a dependency sets `tap`, the Homebrew backend SHALL validate it as `org/repo` (exactly one `/`, each part non-empty, starting with a letter or digit, limited to letters, digits, `-`, `_`, `.`, and not equal to `.` or `..`) and run `brew tap -- <tap>` before installing. Taps are listed in the project-trust summary. A dependency name can never select a tap, because dependency names cannot contain `/`.

#### Scenario: Invalid tap
- **WHEN** a dependency sets `tap: "evil; rm -rf /"`
- **THEN** config validation fails before anything is installed

#### Scenario: Valid tap
- **WHEN** `mongodb` sets `tap: mongodb/brew`
- **THEN** devy runs `brew tap -- mongodb/brew` and then installs `mongodb-community`

#### Scenario: Tap smuggled through the name
- **WHEN** a dependency is named `evilorg/tap/formula`
- **THEN** config loading fails with an invalid-name error and `brew` is never run

### Requirement: Apt backend
The apt backend SHALL count as available when `/usr/bin/apt-get` exists. It SHALL treat a package as installed when `dpkg-query -W -f=${Status}|${Version} -- <name>` reports `install ok installed` (and, when a version is pinned, the installed version matches exactly), and SHALL install with `/usr/bin/sudo /usr/bin/apt-get -y install -- <name>[=<version>]`. The version is passed through as is, so partial versions such as `20` may not resolve. devy never runs `apt-get update` before installing. A failed install reports `` `sudo apt-get <args>` failed — check the output above for details``.

#### Scenario: Pinned version mismatch
- **WHEN** `redis-server` is installed at a different version than the pinned one
- **THEN** devy reports it as not installed and installs `redis-server=<version>`

#### Scenario: Local package file rejected
- **WHEN** `devy.yml` lists `./evil.deb` and the backend is apt
- **THEN** config loading fails and `sudo` is never invoked

### Requirement: Locating the Nix binary
The Nix backend SHALL find `nix` on `PATH`, ignoring `PATH` entries inside the project root, then in `/nix/var/nix/profiles/default/bin`, `~/.nix-profile/bin`, and `/run/current-system/sw/bin`.

#### Scenario: Nix installed but not on PATH
- **WHEN** `nix` is absent from `PATH` but present at `/nix/var/nix/profiles/default/bin/nix`
- **THEN** devy uses that binary

#### Scenario: Project-local nix ignored
- **WHEN** the activated environment puts `<project_root>/.devy/nix-profile/bin` first on PATH and it contains `nix`
- **THEN** devy uses the system `nix` instead
