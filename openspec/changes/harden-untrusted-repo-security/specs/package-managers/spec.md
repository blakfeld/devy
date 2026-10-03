# Spec Delta

## ADDED Requirements

### Requirement: Package arguments end option parsing
Every package-manager invocation that takes package names, versions, taps or IDs from configuration or the lock SHALL pass them after an end-of-options marker (`--`) where the tool supports one: `apt-get install`, `dpkg-query -W`, `brew install`, `brew tap`, `brew list`, `npm install -g`, `rustup target add`, `rustup component add`, `rbenv install`, `rbenv local`, and `gcloud components install`. For tools without such a marker (winget), values SHALL be passed only as the argument of a named option (`--id <id>`, `--version <version>`). Values SHALL have already passed project-config validation.

#### Scenario: apt receives a separator
- **WHEN** devy installs `redis-server` at version `7.0.15-1` with apt
- **THEN** it runs `/usr/bin/sudo /usr/bin/apt-get -y install -- redis-server=7.0.15-1`

### Requirement: Installer downloads are verified
Every script devy downloads and runs (the Nix and Homebrew bootstrap installers, and the deno, bun and gcloud installers) SHALL be fetched with HTTPS only (`--proto =https --tlsv1.2`, and no redirects to other schemes) from a URL that pins an exact release or commit. The script SHALL be saved to a private temporary file, and its SHA-256 compared against a digest built into devy before it runs. On mismatch devy SHALL delete the file and fail with `<installer> checksum mismatch: expected <hex>, got <hex>`, without running it. devy SHALL NOT pipe a download directly into a shell.

#### Scenario: Tampered installer
- **WHEN** the downloaded deno installer's SHA-256 differs from the pinned digest
- **THEN** devy fails with the checksum-mismatch error and the script is not executed

#### Scenario: Truncated download
- **WHEN** the connection drops partway through downloading the bun installer
- **THEN** no part of the script runs

## MODIFIED Requirements

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
