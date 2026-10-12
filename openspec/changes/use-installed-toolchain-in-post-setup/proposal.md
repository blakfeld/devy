# Proposal

## Why

Module setup steps run whatever tool is on devy's own PATH, not the toolchain devy just installed. During `devy up`, devy never adds `.devy/nix-profile/bin` to its own PATH or to the PATH of the commands it starts; only shadowenv adds it later, in the user's shell. The current spec even records this as intended ("Setup tools run from devy's own PATH"). So:
- **Tools aren't found.** On a first nix `devy up`, `npm`, `pnpm`, `mix`, `rebar3`, `dart`, `zig`, `shards`, `dotnet`, `mvn` and `gradle` fail with "not found", or a different system copy runs.
- **The wrong Python is used.** The virtualenv is created with whichever `python3` comes first on PATH (macOS `/usr/bin/python3` or brew's default) even when `version: "3.12"` installed `python312`. Because the venv is only created when `pyvenv.cfg` is missing, it is never rebuilt afterwards.
- **Global npm installs can't work under nix.** Once `npm` does come from the profile, `npm install -g` targets the read-only `/nix/store` prefix.

rust and ruby already resolve their binaries explicitly; the other language modules need the same.

## What Changes

- **Installed toolchain first.** Setup commands (post-setup steps, and typescript's `npm install -g` in its install step) SHALL take their program from the package manager's PATH entries (the verified `.devy/nix-profile/bin` under nix) before devy's own PATH.
- **The child process sees it too.** Those commands SHALL run with the package manager's PATH entries prepended to their PATH, so the scripts and subprocesses they start (`node` from npm lifecycle scripts, `erl` from `mix`, `java` from `./gradlew`) find the same toolchain.
- **Python venv uses the declared Python.** With a `version`, devy prefers `python<major.minor>` over `python3`/`python`. When an existing venv was built by a different Python (major.minor, read from `pyvenv.cfg`), devy recreates it, which also re-runs the dependency install.
- **Writable npm global prefix under nix.** Under the nix backend, `npm install -g` runs with `--prefix <project_root>/.devy/npm-global`, and node/typescript contribute `<project_root>/.devy/npm-global/bin` to PATH and `NPM_CONFIG_PREFIX` to the environment.
- **Spec correction.** The "Setup tools run from devy's own PATH" requirement is replaced by "Setup tools use the installed toolchain".

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `dependency-modules`: setup-tool resolution and child PATH (replaces "Setup tools run from devy's own PATH"); Python venv interpreter selection and rebuild; Node and TypeScript global installs under nix.

## Impact

- **Code:**
  - `src/modules/helpers.rs`: new toolchain lookup and command builder.
  - `src/fs_safe.rs`: a lookup restricted to given directories (reusing `executable_in`).
  - `src/package_manager/mod.rs` and `nix.rs`: a quiet accessor for the toolchain directories.
  - The modules `python`, `node`, `typescript`, `dart`, `zig`, `crystal`, `erlang`, `elixir`, `dotnet`, `java` and `kotlin`.
  - `README.md`.
- **Behaviour:**
  - brew, apt and WinGet are unchanged: they contribute no PATH entries.
  - Under nix, an existing project's venv is rebuilt once if it was created by a different Python than the one now in the profile.
- **Out of scope:**
  - Spawning `.cmd`/`.bat` shims on Windows (`resolve-windows-command-shims`).
  - `JAVA_HOME` and keg-only brew bin directories (`derive-java-home-from-installed-jdk`).
