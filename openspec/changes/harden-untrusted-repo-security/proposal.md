# Proposal

## Why

A security audit of devy found that a malicious repository can turn its committed files into code execution in several ways. The worst cases go beyond the user: some yield **root** (through `sudo apt-get`), some expose services to the LAN, and some need no `devy up` at all (bash tab completion runs code from command names). The remaining findings let the repo plant persistent shell or `PATH` changes, overwrite files outside the project through committed symlinks, smuggle hooks in through config that devy itself writes, or leak secrets to the AI provider. devy treats `devy.yml`, `devy.lock` and the repo tree as trusted input. Today nothing asks the user before running any of that content. Developers and coding agents now clone and `devy up` unfamiliar repos routinely, so this needs fixing before wider adoption.

## What Changes

- **BREAKING: project trust.** A new `devy allow` command records trust for a project root together with a digest of `devy.yml` and `devy.lock`; `devy allow --revoke` removes it. Commands that run repo-defined code (`up`, `down`, `start`, `restart`) require trust:
  - The code they run includes hooks, `after_install`, `install_cmd`, the implicit project installs (npm/bun/deno/bundle/pip/mix/gradle/…) and shadowenv activation.
  - Interactive runs show what will execute and prompt.
  - Non-interactive runs fail with a hint to run `devy allow`.
  - When devy rewrites `devy.lock` or `devy.yml` itself, it keeps the trust record current.
- **Strict validation of config and lock values**, when they load:
  - What is validated: dependency names, versions (including lock `resolved_version`), list entries (`global_packages`, `targets`, `components`, …), command names, `cwd`, `venv_path`, lock image digests and lock ports.
  - Every package-manager call ends option parsing with `--`.
  - A dependency name containing `/` or ending in `.deb` is rejected, so a name can no longer add a tap or point to a local package file.
- **Shell completion:** bash completion no longer passes project data through `compgen -W`; `_commands` never prints invalid names, because such configs fail to load.
- **Shadowenv:**
  - devy runs `shadowenv trust` only for a trusted project, and only when `.shadowenv.d/` holds nothing but devy's own file.
  - devy refuses `.shadowenv.d/`, `.devy/` or the venv when they are symlinks or tracked by git.
  - devy finds `shadowenv`, `nix`, `brew`, `sudo` and `apt-get` outside project-local `PATH` entries.
- **Safe filesystem writes:**
  - Every file devy writes in the project or a shared temp location is created without following symlinks and renamed into place. This covers the lock, `devy.yml`, stamps, shadowenv, service configs, `.devy-lock` and failure records.
  - Shared-temp directories (sockets, AI working directory, logs) are per-user, mode 0700 and owner-checked.
  - Config discovery stops at directories not owned by the user.
- **Config devy writes:**
  - `init --detect` strips control characters from text it copies into comments.
  - AI `init` and `doctor` flag any added or changed executable field (hooks, `after_install`, `install_cmd`, `commands`, `tap`, `image`, `environment` keys that affect execution) and require explicit confirmation; `doctor --yes` never applies those.
- **Services:**
  - MySQL/MariaDB `cli_args` becomes an allowlist of tuning keys.
  - MinIO console, MailHog UI/API and RabbitMQ epmd and distribution bind to 127.0.0.1.
  - The Meilisearch key and docker credentials leave argv.
  - Default docker tags are pinned versions instead of `latest`.
- **AI:**
  - The redactor covers Bearer/JWT, mid-line assignments, JSON, `.npmrc`/`.netrc`, URL passwords containing `/`, more token prefixes and YAML block scalars.
  - Symlinked project files are never read for context.
  - `claude` runs in a fresh private directory and is resolved outside project `PATH` entries.
- **Output:** devy strips terminal control characters from untrusted text it prints: config values, logs and AI replies.
- **Installers:** bootstrap and script installers (nix, brew, deno, bun, gcloud) download to a file over HTTPS-only, are checked against a pinned SHA-256, and then run.
- **Export:** `devy export` escapes `name` correctly for Nix strings, `''` strings and the shell.
- **Non-spec hardening:**
  - Dependencies: bump `rustls`, replace `serde_yml` with a maintained YAML crate, bump `anyhow`.
  - CI: pin actions to SHAs, set least-privilege `permissions`, pass `CARGO_REGISTRY_TOKEN` via `env`, publish `SHA256SUMS`, and run `cargo audit`.

## Capabilities

### New Capabilities
- `project-trust`: `devy allow`, the trust store, which commands are gated, the prompt, invalidation when files change, and non-interactive behavior.
- `filesystem-safety`: no-follow writes, refusal of symlinked or git-tracked devy-managed paths, private temp directories, and binaries resolved outside the project.

### Modified Capabilities
- `project-config`:
  - name, version, list-entry and path validation
  - config discovery ownership boundary
  - command-name rules
  - detected init sanitization
  - AI init review of executable fields
- `lock-file`: value validation on load; no-follow atomic writes.
- `environment-up`: trust check in the phase ordering.
- `shell-environment`: shadowenv trust only after project trust, with a clean `.shadowenv.d`.
- `shell-integration`: safe bash completion.
- `package-managers`:
  - verified bootstrap installers
  - tap and dependency-name rules
  - `--` and absolute-path privileged commands
- `dependency-modules`: verified script installers, `venv_path` containment.
- `service-modules`: `cli_args` allowlist; MinIO, MailHog and RabbitMQ loopback binds; Meilisearch key off argv; private socket directory.
- `service-ports`: `cli_args` sanitization becomes an allowlist; lock ports validated.
- `docker-services`: pinned default tags, credentials via env file, digest validation.
- `ai-assist`: broader redaction, symlink refusal, private working directory.
- `environment-doctor`: executable-field changes never auto-applied.
- `nix-export`: escaping of `name`.
- `cli`: `allow` subcommand; control-character stripping in output.

## Impact

- **Code:**
  - `src/config.rs`, `src/lock.rs`
  - every `src/package_manager/*`
  - `src/modules/{helpers,mod,generic,deno,ruby,bun,gcloud,node,typescript,rust,python,mysql,mariadb,minio,mailhog,rabbitmq,meilisearch,…}.rs`
  - `src/env_manager/shadowenv.rs`
  - `src/commands/{up,down,service,hook,list_commands,init,doctor,export,exec,shared,failure_record}.rs`
  - `src/init_detect/*`, `src/ai/{mod,redact,init_prompt}.rs`, `src/output.rs`
  - new `src/trust.rs` and `src/fs_safe.rs`
- **Behavior:**
  - First `devy up` in every existing project prompts once for trust, or fails in CI until `devy allow` runs. This is the main breaking change.
  - Configs with now-invalid names, dangerous `cli_args` keys or bad `venv_path` fail validation.
- **Dependencies:**
  - Adds `sha2` and `tempfile`.
  - Replaces `serde_yml`.
  - Bumps `rustls` (via `ureq`) and `anyhow`.
- **CI:** all three workflows change.
- **Overlap:** `add-worktree-environments` also modifies `shell-integration` tab completion and adds `.devy/.gitignore`. Whichever lands second must rebase its delta.
