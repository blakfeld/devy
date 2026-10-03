# Tasks

## 1. Dependencies and scaffolding

- [ ] 1.1 Bump `rustls` to ≥0.23.45 (`cargo update -p rustls`) and `anyhow` past RUSTSEC-2026-0190; verify `cargo audit` reports neither advisory
- [ ] 1.2 Replace `serde_yml` with `serde_norway` behind a `use serde_norway as yaml` alias across all call sites (D12); add a nested-alias document test that checks expansion is bounded; verify `cargo test` passes and `cargo audit` no longer lists `serde_yml`/`libyml`
- [ ] 1.3 Add `sha2`, `tempfile` and `regex` as direct dependencies, and create empty `src/validate.rs`, `src/fs_safe.rs`, `src/trust.rs`, `src/installers.rs`, `src/config_diff.rs` modules; verify `cargo build` and `cargo clippy --all-targets -- -D warnings` pass on all CI targets

## 2. Value validation (project-config, lock-file)

- [ ] 2.1 Implement `validate.rs` rules: `dep_name` (allowing `:`), `version`, `list_entry`, `command_name`, `rel_path_inside`, `env_key`, with unit tests for each audit payload (`-oDPkg::…`, `./evil.deb`, `evilorg/tap/formula`, `1.0;id`, `./x`, `$(id>/tmp/p)`, `../../`, `/tmp/shared`); verify the tests pass
- [ ] 2.2 Call the rules from `config.rs` for dependency names and versions, command names, hook and command `cwd`, and `environment` keys, using the `<location>: invalid <kind> <value>` error; resolve `cwd` against the project root in `exec.rs`; verify with config unit tests plus a `tests/cli.rs` case where `devy check` rejects a hostile name
- [ ] 2.3 Add `helpers::extra_list` that applies `list_entry`, and use it for node/typescript `global_packages`, rust `targets`/`components` and gcloud `components`; validate python `venv_path` at config time; verify module unit tests
- [ ] 2.4 Validate lock entries on load (`resolved_version`, `image_digest` format, `assigned_port` ≥1024 unless it matches the explicit config port); verify lock unit tests for each lock-file spec scenario
- [ ] 2.5 Stop config discovery at directories not owned by the current user, and ignore a foreign-owned `devy.yml`; verify a unit test using a temp tree with a faked owner check
- [ ] 2.6 Update README's devy.yml reference with the naming rules; verify the documented examples load with `devy check`

## 3. Package-manager argument hardening (package-managers)

- [ ] 3.1 Add `--` before package specs in apt (`install`, `dpkg-query -W`), brew (`install`, `tap`, `list`), npm `-g`, rustup `target`/`component add`, rbenv `install`/`local`, and gcloud `components install`; update the argv assertions in existing unit tests; verify the tests pass
- [ ] 3.2 Run apt through `/usr/bin/sudo /usr/bin/apt-get` and define availability as the existence of `/usr/bin/apt-get`; verify the apt unit test expects the absolute argv
- [ ] 3.3 Tighten `validate_tap` (no `.`/`..` parts, alphanumeric first character), fix its misleading comment, and confirm a name containing `/` never reaches `brew install`; verify the Homebrew tap scenarios as unit tests
- [ ] 3.4 Make the winget installed check match the ID exactly instead of by substring; verify a unit test where `Foo.Bar` is installed and `Foo.Ba` is not treated as installed
- [ ] 3.5 Under brew, refuse lock `resolved_version` values that `is_formula_pin` treats as formula selectors; verify a unit test where lock `"16"` does not switch the install to `node@16`

## 4. Filesystem safety primitives (filesystem-safety)

- [ ] 4.1 Implement `fs_safe::write_atomic` (symlink refusal, `create_new` + `O_NOFOLLOW`, random temp name, fsync, mode, rename) and `fs_safe::private_dir` (0700, exclusive create, owner and mode check); verify unit tests for a symlinked destination, pre-planted temp-name symlinks, and a pre-existing foreign or wrong-mode directory (Unix-only tests gated with `cfg`)
- [ ] 4.2 Move the lock, `devy.yml` writes (init, doctor), `.devy-lock`, the failure record, every stamp file (`helpers::write_stamp`, node, gcloud), `500_devy.lisp`, `my.cnf`, `vault.hcl`, `nginx.conf`, Kafka `server.properties`, RabbitMQ files and export output to `write_atomic`; verify that a committed-symlink `tests/cli.rs` case for `.devy_bun_stamp` and `devy.lock.<pid>.tmp` leaves the target untouched
- [ ] 4.3 Move the socket fallback directory, AI working directory (random name, removed after use) and nix service log files under a per-user base via `private_dir`; verify a unit test that a pre-created foreign socket dir makes the service start fail with the ownership error
- [ ] 4.4 Add a `clippy.toml` `disallowed-methods` entry for `std::fs::write`, and annotate the remaining legitimate uses; verify `cargo clippy --all-targets -- -D warnings` passes

## 5. Devy-managed directories and executable resolution (filesystem-safety)

- [ ] 5.1 Implement the managed-path check (real directory, owned by the user, not tracked by `git ls-files`) for `.devy/`, `.shadowenv.d/` and the venv, plus the nix-profile `/nix/store` target check; verify `tests/cli.rs` cases for a committed `.venv/bin/git` and a fake `.devy/nix-profile`
- [ ] 5.2 Add `which_outside_project` and use it for `shadowenv`, `nix`, `brew`, `dpkg-query` and `claude`, allowing only the verified project nix profile; remove the `<prepend>/shadowenv` fallback; verify the unit tests in the "Planted sudo" and "Project-local nix ignored" scenarios

## 6. Shell completion (shell-integration)

- [ ] 6.1 Rewrite the bash snippet to read `_commands`/`_services` with `mapfile` and build `COMPREPLY` by literal prefix match, with built-ins in a static array; add `allow` and `--revoke` to the bash, zsh and fish snippets; verify a `tests/cli.rs` case that sources the bash snippet with a stub `_commands` printing `$(touch …)` and checks that the file is not created, plus the existing completion snapshot tests

## 7. Project trust (project-trust, environment-up, cli)

- [ ] 7.1 Implement the trust store (path resolution, 0700/0600 modes, record per `sha256(canonical_root)`, digest of `devy.yml` + `devy.lock`) with unit tests for edit invalidation and the copied-path case; verify the tests pass
- [ ] 7.2 Implement the trust-summary enumeration in `config_diff.rs` (hooks, `after_install`, `install_cmd`, implicit setup steps per module, taps, images, execution-affecting env keys), and expose implicit setup steps from each module via a `Module` method; verify the summary scenario as a unit test
- [ ] 7.3 Add the `devy allow [--revoke]` subcommand; verify `tests/cli.rs` cases for allow, revoke, and `devy --help` listing `allow`
- [ ] 7.4 Gate `up`, `down`, `start` and `restart` with an interactive prompt or a non-interactive error, and insert the trust and managed-path steps into the `up` phase order; verify `tests/cli.rs` cases where `before_up: touch <marker>` is not run when declined or non-interactive, and is run after `devy allow`
- [ ] 7.5 Refresh the trust record after devy's own lock and `devy.yml` writes, only when the project was trusted at command start; verify that `devy up --update` followed by `devy up` does not prompt
- [ ] 7.6 Add `devy allow` to `.github/workflows/integration.yml`, `.github/scripts/doctor-e2e.sh`, every `tests/cli.rs` helper that runs `up`/`down`, and README (new "Trusting a project" section); verify the full test suite and a local run of `doctor-e2e.sh`

## 8. Shadowenv (shell-environment)

- [ ] 8.1 Run `shadowenv trust` only for trusted projects whose `.shadowenv.d` contains only `500_devy.lisp`, and fail with the named-files error otherwise; verify the "Committed lisp file" scenario as a `tests/cli.rs` case and the existing nix-install scenario in unit tests

## 9. Services (service-modules, service-ports)

- [ ] 9.1 Replace `sanitized_mysql_args` key checks with the allowlist, warn on skipped tokens, and move forced args after user tokens in `mysql_family_launch`; verify the "Dangerous option skipped" and "Forced bind wins" scenarios as unit tests
- [ ] 9.2 Under brew, write MySQL/MariaDB settings to a devy-owned `my.cnf.d/devy.cnf` and warn when `my.cnf` lacks `!includedir`, without overwriting it; verify a brew unit test with a pre-existing `my.cnf`
- [ ] 9.3 Bind the MinIO console to `127.0.0.1` (always passing `--console-address`, choosing a free port when unset, validating `console_port`), export `MINIO_CONSOLE_ADDRESS=127.0.0.1:<port>`, and warn about default credentials; verify the MinIO unit tests and scenarios
- [ ] 9.4 Add `-ui-bind-addr`/`-api-bind-addr 127.0.0.1:8025` to MailHog, `ERL_EPMD_ADDRESS` and `inet_dist_use_interface` to RabbitMQ, and `transport.host=127.0.0.1` to Elasticsearch/OpenSearch; verify the launch-definition unit tests
- [ ] 9.5 Pass the Meilisearch `master_key` via `MEILI_MASTER_KEY` instead of `--master-key`; verify a launch unit test asserting that the key is not in argv

## 10. Docker services (docker-services)

- [ ] 10.1 Pass container credentials via a 0600 `--env-file` deleted after `docker run`, and check the `sh.devy.project` label before reusing a container; verify docker argv unit tests and a label-mismatch unit test
- [ ] 10.2 Pin MinIO and MailHog default tags to exact versions; verify the image-table unit test

## 11. Verified installers (package-managers, dependency-modules)

- [ ] 11.1 Implement `installers.rs` (pinned URL and SHA-256 table, HTTPS-only `ureq` download with redirects restricted to https, write into `private_dir`, verify, run via argv); verify unit tests for a digest mismatch and a truncated body using a local test server
- [ ] 11.2 Switch the Nix and Homebrew bootstrap, deno, bun (positional `bun-v<version>`) and gcloud installers to `installers.rs`, removing every `curl … | sh` string; fill in the pinned versions and digests; verify `rg "\| *(ba)?sh" src` finds nothing and the bootstrap scenarios pass

## 12. Config written by devy (project-config, environment-doctor)

- [ ] 12.1 In `init --detect`, sanitize and truncate TODO text, drop invalid versions and script names into TODOs, and skip symlinked sources; verify the "Newline injection through .nvmrc" and "Symlinked .env.example" scenarios as unit tests
- [ ] 12.2 In AI `init`, add the untrusted-data system prompt line; when the reply has executable fields, list them, prompt in a terminal, and otherwise strip them into `# TODO: review suggested <field>` comments; verify the "Injected hook is not written silently" scenario with a stub `claude`
- [ ] 12.3 In `doctor`, discard proposals containing control characters, list executable-field changes, and refuse them under `--yes` with the review hint; verify the "--yes never adds a hook" scenario with a stub `claude`

## 13. AI assist (ai-assist)

- [ ] 13.1 Rewrite `redact.rs` as an ordered rule list (D9) and add a fixture test for every leak the audit found (Bearer/JWT, mid-line, JSON, tabs, `.npmrc`, `.netrc`, docker `auth`, URL password with `/`, query token, `mysql -p`, new prefixes, YAML block scalar); verify that all fixtures pass and the existing redaction tests still pass
- [ ] 13.2 Read AI context files only when they are regular, non-symlink files inside the project root (init, ask, logs `--explain`, doctor); verify the "README symlinked to a credentials file" scenario
- [ ] 13.3 Run `claude` from a random private directory with user-only setting sources, resolved outside project PATH, and remove the directory afterwards; verify with a stub `claude` that records its cwd and arguments

## 14. Output sanitization (cli, environment-check)

- [ ] 14.1 Add `output::clean` and apply it in every output helper, the table printers, log streaming (keeping SGR on a TTY) and AI reply printing; verify unit tests that OSC 52, OSC 8 and cursor movement are removed and SGR is kept for TTY log output
- [ ] 14.2 Mask secret-looking values in `devy status`; verify the "Secret values masked" scenario in `tests/cli.rs`

## 15. Nix export (nix-export)

- [ ] 15.1 Escape `name` for Nix `"…"` and `''…''` strings and single-quote it for the shellHook echo; make `nix_attr_name` escape `\` and `${`; verify that the "Hostile project name" scenario evaluates with `nix-instantiate --parse` in a test gated on `nix` being available, plus string-level unit tests

## 16. Fish quoting

- [ ] 16.1 Add a fish-specific quoter for extra arguments in `exec.rs` that escapes `\` before `'`; verify unit tests for arguments ending in `\` and containing `'`

## 17. CI and release hardening

- [ ] 17.1 Pin every action in `ci.yml`, `integration.yml` and `release.yml` to a full SHA with a version comment, and add `.github/dependabot.yml` for `github-actions` and `cargo`; verify with `rg "uses: .*@v" .github/workflows` returning nothing
- [ ] 17.2 Add top-level `permissions: contents: read`, grant `contents: write` only to the release job, and move `CARGO_REGISTRY_TOKEN` and `github.ref_name` into `env:`; verify a tag dry-run on a fork completes
- [ ] 17.3 Publish `SHA256SUMS` and build-provenance attestations with releases, make `publish` depend on the GitHub release job, drop `--allow-dirty`, and disable rust-cache saving on tag builds; verify the fork tag dry-run produces `SHA256SUMS`
- [ ] 17.4 Add a `cargo audit` CI step that fails on vulnerabilities, and add `node_modules/` and `repomix-output.xml` to `.gitignore`; verify that CI is green

## 18. Integration checks

- [ ] 18.1 Build a hostile fixture repo under `tests/fixtures/hostile/` that combines every audit payload (hooks, `.deb` name, symlinked stamps and lock temp names, committed `.shadowenv.d` lisp, committed `.venv/bin`, hostile command name, `cli_args`, `.nvmrc` newline, symlinked README). Add a `tests/cli.rs` case that runs `check`, `up` (non-interactive and declined), `init --detect` and bash completion against it. Verify that no marker file outside the temp dir is created or modified and that every command exits as specified
- [ ] 18.2 Re-run the security-analyst review over the finished branch, scoped to the audit's original chunks; verify that no Critical or High findings remain and record any Medium leftovers as follow-up issues
