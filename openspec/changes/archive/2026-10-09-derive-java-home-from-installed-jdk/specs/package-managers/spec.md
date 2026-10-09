# Spec Delta

## ADDED Requirements

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
