## MODIFIED Requirements

### Requirement: TypeScript module
The typescript module SHALL install Node as the node module does, and accept a `global_packages` extra key, which is part of its allowed extra keys. Node counts as installed as for the node module. On every `devy up`, whether or not Node was already installed, setup MUST run `npm install -g typescript <global_packages…>` unless that package list (in config order, `typescript` first) matches `.devy_ts_global_stamp` in the project root, and MUST write the stamp after a successful run. On every `devy up` it MUST also run the same lockfile-based project install as the node module, stamped in `.devy_ts_local_stamp`. Its nixpkgs attribute follows the node module's, including versioned attributes.

#### Scenario: Fresh install
- **WHEN** `typescript` is not installed
- **THEN** devy installs Node and runs `npm install -g typescript`

#### Scenario: Global packages
- **WHEN** `typescript` is declared with `global_packages: [eslint, prettier]` and is not installed
- **THEN** devy runs `npm install -g typescript eslint prettier`
- **AND** `devy check` does not report `global_packages` as unrecognized

#### Scenario: Node already installed
- **WHEN** Node is already installed through the package manager but the `typescript` npm package is not, and there is no `.devy_ts_global_stamp`
- **THEN** devy reports Node as already installed and still runs `npm install -g typescript`, then writes `.devy_ts_global_stamp`

#### Scenario: Global package added later
- **WHEN** a previous `devy up` installed `typescript` and `global_packages: [eslint]` is then added to devy.yml
- **THEN** the next `devy up` runs `npm install -g typescript eslint`

#### Scenario: Global packages unchanged
- **WHEN** `global_packages` are unchanged since the last run and `.devy_ts_global_stamp` matches
- **THEN** `npm install -g` is not run again

### Requirement: Ruby module
The ruby module SHALL install Ruby through rbenv on brew, apt and nix: install `rbenv` (plus `ruby-build` on apt) if it is missing, then `rbenv install --skip-existing <version>`, defaulting to 3.3.6. On winget it MUST install `RubyInstallerTeam.Ruby.<major>` (major defaults to 3). On setup, when rbenv is available, it MUST run `rbenv local <version>`. Without a pinned version, an existing `.ruby-version` is left alone, and otherwise 3.3.6 is used. Before running `rbenv local <version>`, setup MUST make sure that version is installed in rbenv, running `rbenv install --skip-existing <version>` when it is not, so the version passed to `rbenv local` is always one devy has installed. If a `Gemfile` exists it MUST run `bundle install`, preferring `$RBENV_ROOT/shims/bundle`, stamped in `.bundle/.devy_stamp` against `Gemfile.lock` or `Gemfile`. It contributes `RBENV_ROOT` (`$RBENV_ROOT`, or `~/.rbenv`) and prepends `<rbenv_root>/bin` and `<rbenv_root>/shims` to PATH. On brew, apt and nix, ruby counts as installed when `rbenv` is on PATH and either the pinned `version` is installed in rbenv or, with no pinned version, rbenv lists any Ruby version (`rbenv versions --bare` is non-empty). On winget it counts as installed when the RubyInstaller package is installed. The lock version is the pinned `version`, or else the output of `rbenv local`; it is not read from the installed Ruby. On winget the version is looked up through the package manager under the name `ruby`, not the RubyInstaller id. Its lock source is always `rbenv`, including on winget.

#### Scenario: Gemfile present
- **WHEN** the project has a `Gemfile` and `Gemfile.lock` and no matching stamp
- **THEN** devy runs `bundle install` in the project root and writes `.bundle/.devy_stamp`

#### Scenario: Existing .ruby-version respected
- **WHEN** ruby has no `version` in `devy.yml` and the project has `.ruby-version`
- **THEN** devy does not run `rbenv local`

#### Scenario: Another Ruby already in rbenv
- **WHEN** ruby has no `version`, the project has no `.ruby-version`, and rbenv has only Ruby 3.2.2 installed
- **THEN** devy reports ruby as already installed, then setup runs `rbenv install --skip-existing 3.3.6` followed by `rbenv local 3.3.6`, and `devy up` succeeds

#### Scenario: Default Ruby already installed
- **WHEN** ruby has no `version`, the project has no `.ruby-version`, and rbenv already has Ruby 3.3.6
- **THEN** setup does not build Ruby again and runs `rbenv local 3.3.6`

### Requirement: Rust module
The rust module SHALL install through rustup, regardless of the package manager, and accept `toolchain` (default `stable`), `targets` and `components`. Rust counts as installed only when rustup is available (in `$CARGO_HOME/bin` / `~/.cargo/bin`, or on PATH outside the project), the configured toolchain is installed in rustup, and every listed target and component is installed for that toolchain. These checks only read rustup's local state; they never download or install anything. When rust is not installed, devy runs the official `sh.rustup.rs` installer with `-y --no-modify-path` only if rustup itself is missing, then (using `~/.cargo/bin/rustup` when rustup is still not on PATH) `rustup toolchain install <tc>` and `rustup default <tc>` when the toolchain is not installed, and `rustup target add --toolchain <tc> <target>` and `rustup component add --toolchain <tc> <component>` for each listed entry that is not installed. An already-installed toolchain is not updated. So adding a target or component, or changing `toolchain`, takes effect on the next `devy up`. Removing a target or component from the list does not uninstall it. If `Cargo.toml` exists it MUST run `cargo fetch`, stamped in `.devy_rust_stamp` against `Cargo.lock` or `Cargo.toml`. Its lock source is `rustup` and its version comes from `rustc --version`.

#### Scenario: Extra target
- **WHEN** rust is configured with `targets: [wasm32-unknown-unknown]` and rustup is not yet installed
- **THEN** devy runs `rustup target add --toolchain stable wasm32-unknown-unknown`

#### Scenario: Target added after rustup exists
- **WHEN** `rustup` is already installed with the `stable` toolchain and `targets: [wasm32-unknown-unknown]` is added to devy.yml
- **THEN** devy does not run the rustup installer, `rustup toolchain install` or `rustup default`, and runs `rustup target add --toolchain stable wasm32-unknown-unknown`

#### Scenario: Toolchain changed
- **WHEN** `rustup` is installed with only `stable` and `toolchain` is changed to `1.80.0`
- **THEN** devy runs `rustup toolchain install 1.80.0` and `rustup default 1.80.0`

#### Scenario: Configured state already present
- **WHEN** rustup, the configured toolchain and every listed target and component are already installed
- **THEN** devy reports rust as already installed and runs no `rustup` install command

#### Scenario: Status shows a missing component
- **WHEN** `components: [clippy]` is configured and clippy is not installed for the toolchain
- **THEN** `devy status` reports rust as not installed

### Requirement: Script-installed runtimes
The deno, bun and gcloud modules SHALL use their official install scripts instead of the package manager. Each script SHALL be downloaded and checked as defined by the package-managers requirement on verified installer downloads, and SHALL then be run from the downloaded file with the version passed as a separate argument or environment variable, never interpolated into a shell command string.
- deno: the deno installer run with `sh <file>`, with `-s v<version>` when pinned (a version already starting with `v` is passed as is). Lock source `deno-installer`. Runs `deno install` when `deno.json` (preferred) or `deno.jsonc` exists, stamped in `.devy_deno_stamp` against that file.
- bun: the bun installer run with `bash <file>`, with the pinned version passed as the installer's version argument (`bun-v<version>`) when pinned. Lock source `bun-installer`. Runs `bun install` when `package.json` exists, stamped in `.devy_bun_stamp` against `bun.lockb`, or `package.json` when there is no lockfile.
- gcloud: uses the package manager on brew (`google-cloud-sdk`) and winget (`Google.CloudSDK`), where installed means the package is installed. On nix and apt it counts as installed when `gcloud` is on PATH or at `$HOME/google-cloud-sdk/bin/gcloud` (which is then used for `components` and the version), and otherwise downloads the verified Google Cloud CLI archive pinned in devy for the current OS and architecture, unpacks it with `tar` in a private staging directory under `$HOME`, moves it to `$HOME/google-cloud-sdk` (failing if that already exists) and runs its bundled installer with `bash $HOME/google-cloud-sdk/install.sh --quiet --usage-reporting=false --path-update=false --command-completion=false`, removing `$HOME/google-cloud-sdk` again if that fails. (Google's `install_google_cloud_sdk.bash` downloads an unversioned, unverified archive, so devy does not use it.) Lock source `gcloud-installer` on every backend. Accepts `components`, installed one per command with `gcloud components install --quiet -- <component>` unless the sorted list matches `.devy_gcloud_components_stamp`.

Deno and bun count as present when the binary is on PATH or at the installer's location (`~/.deno/bin/deno`, `~/.bun/bin/bun`). Without a `version` in `devy.yml` (and, for bun, with `latest` or `canary`), present means installed. With any other `version` set in `devy.yml`, they count as installed only when the binary devy reads the lock version from (the installer's binary, else one on PATH outside the project) reports that version, ignoring a leading `v` (and, for bun, `bun-v`). Otherwise devy runs the installer again with the pinned version. A version taken only from `devy.lock` does not trigger a reinstall. None of these three modules adds anything to PATH (`~/.deno/bin`, `~/.bun/bin` and the gcloud SDK `bin` are not added).

#### Scenario: Pinned deno version
- **WHEN** deno is configured with `version: "1.40.0"` and is not installed
- **THEN** devy runs the verified deno installer with `-s v1.40.0` as separate arguments

#### Scenario: Pinned version changed
- **WHEN** deno 1.40.0 is installed and its `version` in devy.yml is changed to `1.41.0`
- **THEN** devy runs the verified deno installer with `-s v1.41.0`

#### Scenario: Pinned version already present
- **WHEN** bun is configured with `version: "1.1.0"` and `~/.bun/bin/bun --version` prints `1.1.0`
- **THEN** devy reports bun as already installed and does not run the installer

#### Scenario: Unpinned runtime present
- **WHEN** deno has no `version` in devy.yml and `deno` is on PATH
- **THEN** devy reports deno as already installed, even if `devy.lock` records a different version

#### Scenario: gcloud components unchanged
- **WHEN** gcloud `components` are unchanged since the last run
- **THEN** `gcloud components install` is not run again

#### Scenario: Pinned bun version honored
- **WHEN** bun is configured with `version: "1.1.0"` and is not installed
- **THEN** devy runs the verified bun installer with `bun-v1.1.0` as its version argument
