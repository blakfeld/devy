# Spec Delta

## MODIFIED Requirements

### Requirement: Script-installed runtimes
The deno, bun and gcloud modules SHALL use their official install scripts instead of the package manager. Each script SHALL be downloaded and checked as defined by the package-managers requirement on verified installer downloads, and SHALL then be run from the downloaded file with the version passed as a separate argument or environment variable, never interpolated into a shell command string.
- deno: the deno installer run with `sh <file>`, with `-s v<version>` when pinned (a version already starting with `v` is passed as is). Counted as installed when `deno` is on PATH or at `~/.deno/bin/deno`. Lock source `deno-installer`. Runs `deno install` when `deno.json` (preferred) or `deno.jsonc` exists, stamped in `.devy_deno_stamp` against that file.
- bun: the bun installer run with `bash <file>`, with the pinned version passed as the installer's version argument (`bun-v<version>`) when pinned. Counted as installed when `bun` is on PATH or at `~/.bun/bin/bun`. Lock source `bun-installer`. Runs `bun install` when `package.json` exists, stamped in `.devy_bun_stamp` against `bun.lockb`, or `package.json` when there is no lockfile.
- gcloud: uses the package manager on brew (`google-cloud-sdk`) and winget (`Google.CloudSDK`), where installed means the package is installed. On nix and apt it counts as installed when `gcloud` is on PATH or at `$HOME/google-cloud-sdk/bin/gcloud` (which is then used for `components` and the version), and otherwise downloads the verified Google Cloud CLI archive pinned in devy for the current OS and architecture, unpacks it with `tar` in a private staging directory under `$HOME`, moves it to `$HOME/google-cloud-sdk` (failing if that already exists) and runs its bundled installer with `bash $HOME/google-cloud-sdk/install.sh --quiet --usage-reporting=false --path-update=false --command-completion=false`, removing `$HOME/google-cloud-sdk` again if that fails. (Google's `install_google_cloud_sdk.bash` downloads an unversioned, unverified archive, so devy does not use it.) Lock source `gcloud-installer` on every backend. Accepts `components`, installed one per command with `gcloud components install --quiet -- <component>` unless the sorted list matches `.devy_gcloud_components_stamp`.

For deno and bun the installed check ignores `version`, so a pinned version is used only on first install and changing it never reinstalls. None of these three modules adds anything to PATH (`~/.deno/bin`, `~/.bun/bin` and the gcloud SDK `bin` are not added).

#### Scenario: Pinned deno version
- **WHEN** deno is configured with `version: "1.40.0"` and is not installed
- **THEN** devy runs the verified deno installer with `-s v1.40.0` as separate arguments

#### Scenario: Pinned version changed
- **WHEN** deno is already installed and its `version` is changed in devy.yml
- **THEN** devy reports deno as already installed and does not reinstall it

#### Scenario: gcloud components unchanged
- **WHEN** gcloud `components` are unchanged since the last run
- **THEN** `gcloud components install` is not run again

#### Scenario: Pinned bun version honored
- **WHEN** bun is configured with `version: "1.1.0"` and is not installed
- **THEN** devy runs the verified bun installer with `bun-v1.1.0` as its version argument

### Requirement: Python module
The python module SHALL install Python through the package manager (apt `python3`, winget `Python.Python.3`, nix `python3`, otherwise `python`) and accept `venv_path` (default `.venv`) and `install_cmd`. `venv_path` MUST be a relative path without `..` components that resolves inside the project root; otherwise config validation SHALL fail with `python: invalid venv_path <value>`. On setup, only when `<project_root>/<venv_path>/pyvenv.cfg` is missing, it MUST pick `python3` from PATH (else `python`), fail with `Python installation appears incomplete — `<python> --version` failed` if `<python> --version` fails, and create the virtualenv with `<python> -m venv <path>`. It then installs dependencies using the first that applies:
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
