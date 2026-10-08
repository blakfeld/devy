## ADDED Requirements

### Requirement: Setup tools use the installed toolchain
This requirement covers the setup commands that the node, typescript, python, elixir, erlang, dotnet, dart, zig, crystal, java and kotlin modules run during `devy up`. That includes typescript's `npm install -g` in its install step. The toolchain directories are the PATH entries the active package manager contributes to the project environment: `<project_root>/.devy/nix-profile/bin` under nix, once it passes the nix-profile check in filesystem-safety, and none under brew, apt and WinGet. For these commands:
- **Program lookup:** devy SHALL look for the program by name in the toolchain directories first, in order, and run it by absolute path when found. Otherwise it SHALL fall back to the program name resolved against devy's own PATH, as before.
- **Child PATH:** each command SHALL run with the toolchain directories prepended to devy's own PATH, so the programs and scripts it starts find the same toolchain.
- **Repository wrappers:** `./mvnw` and `./gradlew` stay repository scripts run from the project root, with that same PATH.

A `.devy/nix-profile` that fails the nix-profile check contributes no toolchain directory, and devy SHALL NOT run a program from it. devy's own process PATH SHALL stay unchanged. The rust, ruby, deno, bun and gcloud modules keep resolving their tools as their own requirements describe.

#### Scenario: npm from a fresh nix profile
- **WHEN** the backend is nix, `npm` is not on the user's PATH, the project has `package.json`, and `devy up` installs `nodejs` into `.devy/nix-profile`
- **THEN** the same `devy up` runs `<project_root>/.devy/nix-profile/bin/npm install` and succeeds

#### Scenario: Profile tool shadows a system copy
- **WHEN** the backend is nix, `/usr/local/bin/mix` exists, `.devy/nix-profile/bin/mix` exists, and the project has `mix.exs`
- **THEN** `devy up` runs `.devy/nix-profile/bin/mix deps.get`

#### Scenario: Child processes see the toolchain
- **WHEN** the backend is nix and `npm install` runs a lifecycle script that invokes `node`
- **THEN** that script's PATH starts with `<project_root>/.devy/nix-profile/bin`, so it runs the profile's `node`

#### Scenario: Backends without toolchain directories
- **WHEN** the backend is brew and the project has `mix.exs`
- **THEN** devy runs `mix deps.get` resolved from its own PATH, as before

#### Scenario: Fake nix profile
- **WHEN** `.devy/nix-profile` is a directory in the repository containing `bin/npm`, rather than a link into `/nix/store`
- **THEN** devy never runs that `npm` and does not put the directory on any command's PATH

## MODIFIED Requirements

### Requirement: Python module
The python module SHALL install Python through the package manager (apt `python3`, winget `Python.Python.3`, nix `python3`, otherwise `python`) and accept `venv_path` (default `.venv`) and `install_cmd`. `venv_path` MUST be a relative path without `..` components that resolves inside the project root; otherwise config validation SHALL fail with `python: invalid venv_path <value>`.

On setup it MUST pick the interpreter by name, following the setup-tool lookup order (toolchain directories first, then devy's own PATH). With a `version`, it tries `python<major>.<minor>` (for example `python3.12` for `3.12.4`) first. It then tries `python3`, and else `python`. If `<python> --version` fails, it MUST fail with `Python installation appears incomplete — `<python> --version` failed`.

When `<project_root>/<venv_path>/pyvenv.cfg` is missing, it MUST create the virtualenv with `<python> -m venv <path>`. When `pyvenv.cfg` exists and records a Python version (`version` or `version_info`) whose major.minor differs from the chosen interpreter's, it MUST print a warning naming both versions and recreate the virtualenv with `<python> -m venv --clear <path>`. This discards the dependency stamp, so dependencies are installed again. When `pyvenv.cfg` records no readable version, the virtualenv is left alone.

It then installs dependencies using the first that applies:
1. `install_cmd`, run through the default shell
2. `<venv>/bin/pip install -e .` when `pyproject.toml` exists
3. `<venv>/bin/pip install -r requirements.txt` when `requirements.txt` exists

On Windows `pip` is `<venv>/Scripts/pip`. Manifest-based installs are stamped in `<venv>/.devy_stamp`. `install_cmd` is never stamped, so it runs on every `devy up`. The module contributes `VIRTUAL_ENV=<absolute venv path>` and prepends `<venv>/bin` (`Scripts` on Windows) to PATH. The virtualenv directory is subject to the filesystem-safety checks for devy-managed directories.

#### Scenario: requirements.txt project
- **WHEN** the project has `requirements.txt` and no `.venv`
- **THEN** devy creates `.venv`, runs `.venv/bin/pip install -r requirements.txt`, and sets `VIRTUAL_ENV`

#### Scenario: Custom install command
- **WHEN** python sets `install_cmd: "poetry install"`
- **THEN** devy runs `poetry install` through the default shell instead of pip, on every `devy up`

#### Scenario: venv outside the project
- **WHEN** python sets `venv_path: /tmp/shared`
- **THEN** config validation fails and nothing is added to PATH

#### Scenario: Declared version under nix
- **WHEN** the backend is nix, python is declared with `version: "3.12"`, `/usr/bin/python3` is Python 3.9, and the project has no `.venv`
- **THEN** devy creates `.venv` with `<project_root>/.devy/nix-profile/bin/python3.12 -m venv`, and `.venv/pyvenv.cfg` records 3.12

#### Scenario: venv built by another Python
- **WHEN** `.venv/pyvenv.cfg` records `version = 3.9.6` and the chosen interpreter is Python 3.12
- **THEN** devy warns that it is recreating `.venv` (3.9 → 3.12), runs `<python> -m venv --clear .venv`, and reinstalls the project's dependencies

#### Scenario: venv already matches
- **WHEN** `.venv/pyvenv.cfg` records `version = 3.12.4` and the chosen interpreter is Python 3.12.7
- **THEN** devy keeps `.venv` and skips the dependency install when the stamp matches

### Requirement: Node module
The node module SHALL install Node through the package manager (apt `nodejs`, winget `OpenJS.NodeJS`, nix `nodejs`, otherwise `node`) and accept `global_packages`. If `package.json` exists in the project root, it MUST run `<tool> install`. The tool is picked by lockfile: `pnpm-lock.yaml`→pnpm, `yarn.lock`→yarn, otherwise npm. The run is stamped in `.devy_node_local_stamp` against the lockfile, or `package.json` when there is no lockfile. It MUST run `npm install -g <global_packages…>` unless the package list matches `.devy_node_global_stamp`. The stamp holds the list in config order (not sorted), so reordering `global_packages` re-runs the install.

Under the nix backend, the npm global prefix is `<project_root>/.devy/npm-global`, because the default prefix lies in the read-only `/nix/store`:
- **Install:** `npm install -g` MUST run with `--prefix <project_root>/.devy/npm-global`. Setup creates `<project_root>/.devy/npm-global/bin` on every run, whether or not `global_packages` is set.
- **Environment:** the module contributes `NPM_CONFIG_PREFIX=<project_root>/.devy/npm-global`.
- **PATH:** it prepends `<project_root>/.devy/npm-global/bin` to PATH whenever that directory exists.

The directory is subject to the filesystem-safety checks for `.devy/`. On other backends the default npm prefix is used and nothing extra is contributed.

#### Scenario: pnpm project
- **WHEN** the project root contains `package.json` and `pnpm-lock.yaml`
- **THEN** `devy up` runs `pnpm install` in the project root

#### Scenario: Global packages unchanged
- **WHEN** `global_packages: [typescript]` is unchanged since the last run
- **THEN** `npm install -g` is not run again

#### Scenario: Global packages reordered
- **WHEN** `global_packages` changes from `[a, b]` to `[b, a]`
- **THEN** the next `devy up` runs `npm install -g b a` again

#### Scenario: Global packages under nix
- **WHEN** the backend is nix and node declares `global_packages: [eslint]`
- **THEN** devy runs the profile's `npm install -g --prefix <project_root>/.devy/npm-global -- eslint`
- **AND** the written environment sets `NPM_CONFIG_PREFIX` and puts `<project_root>/.devy/npm-global/bin` on PATH

### Requirement: TypeScript module
The typescript module SHALL install Node as the node module does, then install the `typescript` npm package globally along with any packages listed in its `global_packages` extra key, which is part of its allowed extra keys. That global install follows the setup-tool lookup order. Under nix, it uses the node module's project-local npm prefix, `NPM_CONFIG_PREFIX` and PATH entry. On every `devy up` it MUST run the same lockfile-based project install as the node module, stamped in `.devy_ts_local_stamp`. Its nixpkgs attribute follows the node module's, including versioned attributes.

#### Scenario: Fresh install
- **WHEN** `typescript` is not installed
- **THEN** devy installs Node and runs `npm install -g typescript`

#### Scenario: Global packages
- **WHEN** `typescript` is declared with `global_packages: [eslint, prettier]` and is not installed
- **THEN** devy runs `npm install -g typescript eslint prettier`
- **AND** `devy check` does not report `global_packages` as unrecognized

#### Scenario: Node already installed
- **WHEN** Node is already installed through the package manager but the `typescript` npm package is not
- **THEN** devy reports typescript as already installed and does not run `npm install -g typescript`

#### Scenario: Fresh install under nix
- **WHEN** the backend is nix, `npm` is not on the user's PATH, and `typescript` is not installed
- **THEN** devy installs `nodejs` into the profile and runs `<project_root>/.devy/nix-profile/bin/npm install -g --prefix <project_root>/.devy/npm-global -- typescript`, which succeeds

## REMOVED Requirements

### Requirement: Setup tools run from devy's own PATH
**Reason**: This documented a bug. On a fresh nix profile, setup steps couldn't find the tools devy had just installed, or they ran a different system copy.
**Migration**: None needed. See "Setup tools use the installed toolchain". Projects that relied on a system tool shadowing the profile's copy should remove that dependency from `devy.yml`.
