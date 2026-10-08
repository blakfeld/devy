# Design

## Context

See proposal.md (Why). The current state, as checked against the code and the formula on 2026-10-08:

- `MinioModule::nix_launch` passes `--address 127.0.0.1:<port>`, `--console-address 127.0.0.1:<console>` and `MINIO_ROOT_*` through `LaunchSpec.env`. `docker_spec` passes credentials with `secret_env`. Under brew and apt, `start_via_pm` passes no launch spec, so `pm.start_service("minio", None)` runs the package's own unit unchanged.
- Homebrew `Formula/m/minio.rb` `service do`: `run [opt_bin/"minio", "server", "--certs-dir=#{etc}/minio/certs", "--address=:9000", var/"minio"]`. It has no `environment_variables`, and `brew services` rewrites `homebrew.mxcl.minio.plist` on each start.
- MinIO takes its address and console address only from flags. It takes root credentials from `MINIO_ROOT_USER`/`MINIO_ROOT_PASSWORD` in the environment, or from a file named by the `MINIO_CONFIG_ENV_FILE` environment variable. No file at a fixed path is read.
- Debian and Ubuntu have no `minio` source package (sources.debian.org and Launchpad both return none). MinIO's upstream systemd unit reads `EnvironmentFile=/etc/default/minio` and runs `/usr/local/bin/minio`, but the user installs that unit by hand, outside apt.
- `loopback.rs` secures Kafka, ZooKeeper and RabbitMQ by writing devy-owned files (`# devy-managed`) that those services read from `<etc>`. Where it cannot own a file, it warns once.
- `devy up` already validates config (`pm.validate_config`) for non-docker deps before ports, install and start. `devy check` collects `module_issues` (counted as issues) and module warnings. `Module::config_issues(dep)` has no access to the package manager.
- `ports::unapplied_port_warning` already warns that a non-default `port` is not applied under brew or apt. It does not cover the credentials or the bind address.

## Goals / Non-Goals

**Goals:**
- Devy never starts a MinIO that listens on every interface with a login other than the one the user configured.
- Fail early: `devy check` and `devy up` report the problem before anything is installed.
- Keep nix, docker and winget behaviour byte-for-byte unchanged.

**Non-Goals:**
- Running brew-installed binaries under devy's own launchd or systemd units. That would be a general "brew backend with LaunchSpec" feature, and it is out of scope.
- Fixing the same class of problem for Meilisearch's `master_key` under brew. Meilisearch should be checked in a separate review finding.
- Winget. It has no MinIO service integration that devy configures, and it is not covered by `loopback.rs` either.

## Decisions

1. **Fail closed under brew and apt instead of reconfiguring.** `loopback.rs` works only when the service reads a config file at a path devy can own. MinIO has no such file: the Homebrew plist is regenerated, and `MINIO_CONFIG_ENV_FILE` would itself have to be set in the environment. Alternatives considered:
   - *Edit `~/Library/LaunchAgents/homebrew.mxcl.minio.plist`.* Rejected: `brew services start` overwrites it, and the edit would break the rule against rewriting files devy does not own.
   - *Write `/etc/default/minio` under apt (devy-owned, mode 0600).* Rejected for now. No distro package uses it, the unit is installed by hand, and writing `/etc` needs root, which would only turn into a warning. It adds secret-bearing file handling for a path almost nobody can reach.
   - *Warn only.* Rejected: the exposure is HIGH (LAN-reachable object store with a known password), and a warning is easy to miss among `devy up` output.
   - *Run the brew binary under a devy-written unit, like nix does.* This is the proper long-term fix, but the unit writer lives inside `NixPackageManager`. Rejected as too large for a security fix.

2. **New hook `Module::backend_config_issues(&self, dep, pm) -> Vec<String>`, empty by default.** `config_issues` cannot see the backend, and the refusal must happen before install. `devy up` calls the hook in its existing "validate config" loop for non-docker deps and bails on the first issue, with the dep name as context. `devy check` adds the issues as `Note::Issue` for non-docker deps. A refusal in `pm.validate_config` was rejected because it would put module knowledge in the package managers.

3. **Defence in depth in `MinioModule::start`.** `devy service start`/`restart` skip `up`'s validation, so `start` bails with the same message under brew or apt before calling `start_via_pm`. `is_running`, `stop` and `health_check` stay unchanged, so a user can still stop a Homebrew MinIO that an older devy started.

4. **No opt-in escape hatch.** No `allow_insecure` key is added. Users who need MinIO without docker can use the nix backend for the project. A user who really wants Homebrew's MinIO can run it outside devy. This keeps `known_extra_keys` and the spec unchanged.

5. **Message text** (one shared `const` or function in `minio.rs`): `"MinIO's <backend> service listens on every interface with the default minioadmin login and ignores access_key, secret_key, port and console_port — set docker: true or use the nix backend"`, where `<backend>` is `Homebrew` or `apt`.

6. **Nothing added to `loopback.rs`.** It stays the home for file-based loopback config only. The loopback spec requirement names MinIO as refused, so the coverage gap is documented.

## Risks / Trade-offs

- [Existing brew users' `devy up` starts failing] → The message names both fixes. The proposal marks the change **BREAKING**, and README documents it.
- [A Homebrew MinIO started by an older devy keeps running, still exposed] → `devy up` fails before touching it. The message could suggest `brew services stop minio`. The implementer adds that hint when `pm.is_service_running("minio")` is true at refusal time (cheap, and kept out of the spec).
- [`devy check` now exits non-zero for configs that passed before] → This is intended, since the setup was insecure.

## Migration Plan

Users change `minio` to `docker: true`, or switch the project to the nix backend, then run `devy up`. Rollback means reverting the commit. No data migration is needed: Homebrew's data stays in `$(brew --prefix)/var/minio`, untouched.

## Open Questions

- Should the "still running" hint (Risks, second item) also run `brew services stop minio` automatically? The default is no: devy does not stop services it was not asked to stop.
