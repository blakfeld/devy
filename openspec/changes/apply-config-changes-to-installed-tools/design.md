# Design

## Context

`install_binary` (`src/commands/up.rs`) calls `runner.is_installed(dep)`. When that returns true it prints `○ <dep> already installed` and skips `install()`. `post_setup` runs on every `devy up`. The same `is_installed` feeds the `devy status` / `devy check` table (`src/commands/shared.rs` `dep_row`). Today:

- `RustModule::is_installed` is `rustup_program().is_some()`. Toolchain, targets and components are applied only inside `install()`.
- `TypeScriptModule::is_installed` checks only the Node package. `npm install -g typescript …` lives in `install()`.
- `BunModule` / `DenoModule::is_installed` check that a binary exists. `version` is used only by `install()`.
- `RubyModule::is_installed` with no `version` accepts any rbenv Ruby. `post_setup` runs `rbenv local 3.3.6` when there is no `.ruby-version`, and rbenv rejects a version that isn't installed.

The current main spec states each of these as intended behaviour. This change reverses that (see proposal.md).

## Goals / Non-Goals

**Goals:**
- Each config field these modules accept takes effect on the next `devy up` after it changes.
- Installed checks stay local and offline. They never install anything, never touch the network and run no project-controlled binary.
- Ruby setup never runs `rbenv local` with a version devy hasn't made sure is installed.

**Non-Goals:**
- Uninstalling targets, components or global packages that were removed from the list.
- Changing `install_binary`'s control flow, or the `Module` trait signature (`is_installed` has no project root).
- Enforcing `devy.lock` versions on user-global installers (bun, deno, rustup).
- Other modules with similar patterns, such as node `global_packages` (already stamped) and gcloud `components` (already stamped).

## Decisions

1. **Rust: make `is_installed` reflect the configured state; make `install()` incremental.** `is_installed` = rustup found AND `rustup target list --installed --toolchain <tc>` succeeds (the toolchain exists) and lists each configured target AND `rustup component list --installed --toolchain <tc>` lists each configured component. A component matches by exact name or by `<name>-<host triple>` (rustup prints `clippy-aarch64-apple-darwin`). `install()` runs the rustup installer only when rustup is missing. It runs `toolchain install` + `default` only when the toolchain is missing, so an existing `stable` is never re-synced over the network on each `devy up`. It adds only missing targets and components. Keep the parsing in pure functions (`missing_targets(installed: &str, wanted)`, etc.) so tests need no rustup.
   - *Alternative: a stamp in post_setup.* Rejected. A project-local stamp can't see that the user removed a target with rustup, and `devy status` would still show rust as fine.
   - *Alternative: always run the rustup commands in post_setup.* Rejected. `rustup toolchain install stable` checks for and downloads updates on every run.
   - All rustup invocations use the resolved `rustup_program()` (never a bare PATH lookup) and set `RUSTUP_AUTO_INSTALL=0` so a query can't trigger an install.

2. **TypeScript: move global installs into `post_setup` with a stamp; mirror node.** `install()` installs Node only. `post_setup` runs `npm install -g typescript <globals…>` unless `.devy_ts_global_stamp` holds the same newline-joined list. It reuses `npm_global_install_args` (validation and `--` separator) and `helpers::write_stamp_text`. The global step runs before the project-install step, and its stamp is checked even when there is no `package.json`.
   - *Alternative: `is_installed` checks for `tsc` on PATH.* Rejected. It doesn't cover `global_packages`, and probing `npm ls -g` per package is slow.

3. **Bun/deno: version-aware `is_installed`, but only for versions pinned in `devy.yml`.** When `dep.version` is set, not `version_from_lock`, and (bun) not `latest`/`canary`, compare a normalized pinned version (strip `v`, `bun-v`) with `resolved_version()`, which reads the same binary the installer writes. On a mismatch or an unreadable version, report not installed. Put the comparison in a pure helper `version_satisfied(pinned, installed)` that is unit-tested.
   - *Lock versions excluded:* bun and deno install into the user's global `~/.bun` / `~/.deno`. Reinstalling to a teammate's locked version on every `devy up` would downgrade a user's global tool without anyone asking. An explicit `devy.yml` pin is that request.

4. **Ruby: post_setup installs the version it sets (`rbenv install --skip-existing <v>`) before `rbenv local <v>`.** Check with `rbenv_version_installed` first so the common case costs one `rbenv prefix`. Reuse `rbenv_outside_project` and `rbenv_version_arg` for the install. This keeps one source of truth, the `version_to_set` computation, for the version used in both places.
   - *Alternative: `is_installed` with no version requires 3.3.6.* Rejected. `is_installed` can't see `.ruby-version`, so a project with `.ruby-version: 3.2.2` would build 3.3.6 needlessly.
   - Winget is unaffected, because that path doesn't run `rbenv local` unless rbenv exists.

## Risks / Trade-offs

- [`devy status` and `devy check` now run a few extra rustup and `--version` calls] → These are local and fast. They already run `rustc --version` for the lock.
- [Changing `toolchain` runs `rustup default <tc>`, which changes the user's global default] → This matches the existing first-install behaviour and is documented in the spec. Writing `rust-toolchain.toml` instead is out of scope.
- [bun/deno on PATH from another source (brew) with a different version] → The installer writes `~/.bun/bin` / `~/.deno/bin`, and `resolved_version` prefers that binary. After one reinstall the check is satisfied, so it doesn't loop.
- [rustup output format changes] → The parsing tolerates the host-triple suffix and ignores unknown lines. If a query fails, rust is treated as not installed, which runs the idempotent `rustup … add` commands.
- [Users with an unmet config get installs on their next `devy up`] → That is the intended fix. Call it out in the changelog/README.

## Migration Plan

No data migration. `.devy_ts_global_stamp` is new. On the first `devy up` after upgrading, `npm install -g typescript …` runs once for existing typescript users. Roll back by reverting the commit.

## Open Questions

- Whether `rustup target list --toolchain <tc>` can auto-install a missing toolchain under any rustup ≥1.28 setting. `RUSTUP_AUTO_INSTALL=0` is set defensively; check this during implementation against the pinned `RUSTUP_VERSION`.
