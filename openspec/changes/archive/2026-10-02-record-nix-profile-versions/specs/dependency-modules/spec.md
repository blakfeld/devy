## MODIFIED Requirements

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
