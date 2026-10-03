# Tasks

## 1. Per-project nix service names

- [ ] 1.1 Give the nix backend the project root and slug (at construction, per design D1) and replace the `sh.devy.{name}`, `devy-{name}.service` and `$TMPDIR/devy-{name}.log` call sites in `src/package_manager/nix.rs` with one naming function. Verify with unit tests that `redis` in project `app` at `/src/app` yields `sh.devy.app-<hash>.redis`, `devy-app-<hash>-redis.service` and `devy-app-<hash>-redis.log`, and update the existing `sh.devy.redis` test expectations
- [ ] 1.2 Write `DEVY_PROJECT_ROOT=<root>` into the plist `EnvironmentVariables` and the systemd unit's `Environment=` lines. Verify with the existing plist and unit rendering tests extended to assert the entry, including a root containing spaces and `%`
- [ ] 1.3 Implement stale-unit discovery for a service: legacy-named units owned by this root (matched through `.devy/data` or `.devy/nix-profile` paths in program arguments or working directory), and other-slug units whose `DEVY_PROJECT_ROOT` equals this root. Verify with tests over temp LaunchAgents and systemd dirs covering an owned legacy unit, another project's legacy unit, and a renamed-slug unit
- [ ] 1.4 Migrate stale units before start and stop (through `up`, `start`, `stop`, `restart`, `down`), printing `○ migrated <name> to a per-project service name`. Make read-only running checks fall back to an owned legacy unit without removing it, and add the non-failing `devy check` note. Verify with tests that `start` removes an owned legacy plist, `status` leaves it in place and reports running, and another project's legacy unit is never touched
- [ ] 1.5 Update `devy logs` nix sources to the new log file and unit names, with fallback to the legacy source when only it exists. Verify with `src/commands/logs.rs` tests for both macOS and Linux sources and the fallback
- [ ] 1.6 Update the README "Commands" and "Choosing a package manager" sections for the new launchd label, systemd unit and log names, plus a downgrade note. Verify `rg 'sh\.devy\.[a-z]+\b' README.md` finds no outdated label examples

## 2. Worktree detection and `.devy/.gitignore`

- [ ] 2.1 Add `src/worktree.rs`, which detects a linked worktree from the `.git` file (absolute or relative `gitdir:`, `commondir`, bare repositories) and derives the main checkout's project root, including monorepo subdirectories, without running `git`. Verify with temp-dir tests for: main checkout, linked worktree, relative gitdir, a submodule `.git` file, a bare common dir, and a subdirectory project
- [ ] 2.2 Add worktree enumeration and branch lookup: read `HEAD` in the common dir and in each `worktrees/*/HEAD`, and locations from `worktrees/*/gitdir`. Verify with tests that resolve a branch to its checkout and report no match for an unknown or detached-HEAD-only branch
- [ ] 2.3 Add a single helper that creates `.devy/` and writes `.devy/.gitignore` (`*`) only when missing, and route every existing `.devy/` creation through it (nix profile, data dirs, stamps). Verify with tests that the file is created once and existing content is preserved, plus a `tests/cli.rs` check after `devy up` with a stub backend
- [ ] 2.4 Print `worktree of <main root>` (or `worktree (no main checkout)`) under the `devy status` header. Verify with a `tests/cli.rs` case that builds a fake worktree `.git` file layout in the temp dir

## 3. Worktree-local ports

- [ ] 3.1 Add the `.devy/worktree.yml` model (`version: 1`, `ports`) with load (missing means empty, unparseable means empty plus a warning) and atomic write-on-change. Verify with round-trip, corrupt-file and unchanged-content tests
- [ ] 3.2 Change `resolve_ports` to take a port source (the lock, or the worktree file) chosen by one helper that uses worktree detection, and switch `up`, `start`, `restart`, `check`, `status` and docker resolution to it. Add the worktree-specific `'<name>' has no port in this worktree yet — run `devy up` first` error. Verify with `ports.rs` tests: a worktree ignores the lock's `assigned_port`, reuses its own recorded port, and `--update` keeps worktree ports
- [ ] 3.3 In `up`, when in a worktree, write resolved ports to `.devy/worktree.yml` and copy each entry's `assigned_port` from the previous `devy.lock` (absent stays absent). Verify with tests that `devy.lock` isn't rewritten when only ports differ and that a new service gets no `assigned_port` in the lock
- [ ] 3.4 Add the "fixed port shared with the main checkout" warning to `up` and `check` in worktrees. Verify with a test that the warning appears for an explicit nix port and not for a brew service
- [ ] 3.5 Add a README section, "Working in git worktrees", covering port isolation, `.devy/worktree.yml`, explicit-port caveats, and shared brew/apt/WinGet services. Verify the section is linked from the Commands section

## 4. `devy up --from`

Depends on the archive, extract, eligibility and `.devy-lock` helpers from `add-service-snapshots` (its tasks 1.1, 2.1–2.3).

- [ ] 4.1 Add `--from [<source>]` to `Commands::Up` (optional value), with the dry-run ignore warning and source resolution (none → main checkout, path → same common dir, else branch) and all error messages from the spec, raised before any install. Verify with `tests/cli.rs` cases for each error and a unit test for path validation
- [ ] 4.2 Load the source as a full project context (its `devy.yml`, backend and port source) and acquire its `.devy-lock` with a 120-second bounded wait and the busy error. Verify with a test that holds the source lock and asserts the busy failure using a shortened test timeout
- [ ] 4.3 Implement the per-service copy after installs and before the service start phase: eligibility, skipping when the target has data or the source has none, the major-version guard, stop source → archive → restart source → extract into target, and partial-extract cleanup that still restarts the source. Verify with `MockPackageManager` tests covering copied, skipped (target has data), skipped (no source data), version mismatch, ineligible backend, and extraction failure with the source restarted
- [ ] 4.4 Verify that one-time init is skipped after a copy: a test where postgresql data is extracted (with `PG_VERSION` present) and `initdb` is not invoked
- [ ] 4.5 Document `devy up --from` in the README worktrees section and the `up` command reference, including that the source's services are briefly stopped. Verify the documented example commands match the CLI's `--help`

## 5. `devy prune`

- [ ] 5.1 Implement nix resource discovery: scan the LaunchAgents or systemd user dir for `sh.devy.*` / `devy-*` units, read `DEVY_PROJECT_ROOT`, and treat a unit as stale when `<root>/devy.yml` is missing. Units without a recorded root are excluded. Verify with temp-dir tests
- [ ] 5.2 Implement docker resource discovery through `docker`, falling back to `podman`, with `ps -a --filter label=sh.devy.project`, pairing each container with its same-name volume, and skipping with a note when no CLI exists. Verify with the existing fake container CLI used in `service_runner/tests.rs`
- [ ] 5.3 Add `devy prune [--yes]` (no `devy.yml` needed): listing, an interactive `[y/N]` prompt, the non-interactive refusal without `--yes`, removal (stop, unload or disable, delete unit file; remove container and volume), and `○ nothing to prune`. Verify with `tests/cli.rs` cases for nothing to prune, non-interactive refusal, and `--yes` removing a stale fake unit while leaving a live project's unit
- [ ] 5.4 Register `prune` as a built-in in `src/cli.rs` and `--help`, add `prune`, `--yes` and `up --from` to the zsh, bash and fish completion scripts in `src/commands/hook.rs`, and document `devy prune` in the README. Verify with the existing hook snapshot and completion tests updated for the new candidates

## 6. Integration

- [ ] 6.1 Run an end-to-end check on macOS and Linux with nix: create `git worktree add ../app-feat`, run `devy up` in both checkouts, and confirm distinct ports in `REDIS_PORT`, distinct running units, and an unchanged `git diff devy.lock` in the worktree. Then run `devy up --from` in the worktree, confirm data is present, run `git worktree remove`, and confirm `devy prune --yes` removes only the worktree's units
- [ ] 6.2 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`, and `openspec validate add-worktree-environments --strict`, all clean
