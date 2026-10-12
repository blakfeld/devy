# Proposal

## Why

`devy up` skips a dependency's install step whenever the module's installed check passes, and several modules' checks ignore what `devy.yml` asks for. Once a tool exists on the machine, later config changes are silently dropped: new Rust targets, components or toolchains are never added; `typescript` never installs `tsc` or its `global_packages` for anyone who already has Node; a changed bun/deno `version` is never applied; and ruby without a `version` counts as installed with any rbenv Ruby while setup then runs `rbenv local 3.3.6`, which rbenv refuses, so `devy up` aborts. The current specs even document these as scenarios. `devy.yml` is meant to be the source of truth, so these are correctness bugs.

## What Changes

- **rust**: counts as installed only when rustup is present, the configured `toolchain` is installed, and every configured `target` and `component` is installed for it. Otherwise `devy up` runs the install steps (without re-running the rustup installer when rustup exists), so editing `toolchain`, `targets` or `components` takes effect on the next `devy up`.
- **typescript**: `npm install -g typescript <global_packages…>` moves into setup and runs on every `devy up`, skipped when the package list matches a new `.devy_ts_global_stamp` (the same scheme as node's `.devy_node_global_stamp`). Having Node already no longer skips it.
- **bun / deno**: with a `version` pinned in `devy.yml`, they count as installed only when the installed binary reports that version; otherwise the verified installer runs again with the pinned version. Unpinned (and bun `latest`/`canary`) keep the "any binary" check.
- **ruby** (rbenv backends): setup makes sure the version it is about to pass to `rbenv local` is installed (`rbenv install --skip-existing <version>`) first, so the no-`version`, no-`.ruby-version` case uses 3.3.6 consistently instead of failing.
- `devy status` and `devy check` report rust, bun and deno as not installed while their configured state is unmet, because they use the same installed check.
- No config changes needed. Users whose config is unmet will see installs that were previously skipped on their next `devy up`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `dependency-modules`: the Rust module, TypeScript module, Ruby module and Script-installed runtimes requirements change their installed checks and setup steps; the scenarios that documented the ignored config are replaced.

## Impact

- Code: `src/modules/rust.rs`, `src/modules/typescript.rs`, `src/modules/bun.rs`, `src/modules/deno.rs`, `src/modules/ruby.rs` (installed checks, install, post_setup). No change to `install_binary` control flow in `src/commands/up.rs`.
- New stamp file `.devy_ts_global_stamp` in the project root, alongside the existing stamps.
- Extra local, offline `rustup`, `bun --version`, `deno --version` and `rbenv prefix` calls per `devy up`, `devy status` and `devy check`.
- Changing a pinned bun/deno version, or the rust `toolchain`, changes the user's global install (`~/.bun`, `~/.deno`, rustup default), just as a first install does today.
