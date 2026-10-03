# Proposal

## Why

When a service fails to start or its health check times out, devy says "check its logs", but it never says where those logs are. They are in a different place for every backend: `$TMPDIR/devy-<name>.log` for nix on macOS, the user journal for nix on Linux, formula-specific files for brew, the system journal for apt, and `docker logs` once the docker service manager lands. Users have to know devy's internals to debug their own environment. A single `devy logs` command removes that knowledge requirement. It also gives devy a reliable way to collect service context, which makes AI-assisted debugging (`devy ask`, `devy logs --explain`) a thin layer on top.

## What Changes

- **`devy logs [service]`** prints the most recent lines of a service's logs, whatever backend runs it. `-n/--lines <N>` sets how many lines (default 100). `-f/--follow` streams new lines until interrupted. With no service, devy shows every declared service, each section headed by the service name. In follow mode, each line is prefixed with `<name> |`.
- **Per-backend log sources:**
  - nix on macOS: the launchd log file.
  - nix on Linux: `journalctl --user -u devy-<name>.service`.
  - brew: the log files that `brew services info --json` reports.
  - apt: `journalctl -u <name>`.
  - docker (from `add-docker-service-manager`): `<container_cli> logs devy-<project>-<service>`.
  - winget: logs are not supported. devy fails with a message pointing at Event Viewer.
- **Extra log hints.** For services that also write their own log files under `.devy/data/<name>/` (nginx, Elasticsearch/OpenSearch, Kafka, RabbitMQ), devy prints `· also see <path>` after the output.
- **Logs are suggested where failures happen.** The shutdown-timeout error and the health-check-timeout warning both say `run devy logs <name>` instead of only "check its logs".
- **`devy logs <service> --explain`** sends the service's recent logs and its configuration to Claude and prints a plain-language diagnosis with suggested fixes. It cannot be combined with `--follow`.
- **`devy ask "<question>"`** answers a free-form question about the project environment. The context devy sends is the redacted `devy.yml`, `devy.lock`, platform and package manager, install and run status of each dependency, and recent logs for each service. `--show-context` prints exactly what would be sent and does not call the API.
- Shell completion covers `logs` (with service names and flags) and `ask`.

The AI parts (`--explain`, `ask`) depend on the `ai-assist` capability introduced by the sibling change **`add-ai-init`**: API key handling, model selection, explicit opt-in, redaction and the mockable transport all come from there. Plain `devy logs` does not depend on it and can ship first. The docker log source depends on **`add-docker-service-manager`** and lands with or after it.

## Capabilities

### New Capabilities
- `service-logs`: the `devy logs` command, how log sources are resolved per backend, tail/follow behavior, multi-service output, and the no-logs and unsupported-backend cases.
- `environment-assistant`: `devy ask` and `devy logs --explain`. Covers what context is collected, `--show-context`, and how answers are printed. It builds on `ai-assist`.

### Modified Capabilities
- `cli`: `logs` and `ask` join the built-in subcommands.
- `shell-integration`: completion adds `logs`, `ask`, the `logs` flags and service names.
- `service-management`: shutdown-timeout and health-timeout messages point to `devy logs <name>`.

## Impact

- **Code:**
  - New `src/commands/logs.rs` and `src/commands/ask.rs`.
  - A `log_source` method on the `PackageManager` trait (`src/package_manager/{nix,brew,apt,winget}.rs`), plus the docker backend when it exists.
  - An optional `extra_log_files` hook on `Module` (`src/modules/mod.rs`, nginx, kafka, search, rabbitmq).
  - `src/cli.rs`, `src/commands/hook.rs` (completion snippets), and the message updates in `src/modules/mod.rs` and `src/commands/service.rs`.
- **Dependencies:** `devy logs` tails files with std and shells out to `journalctl`, `docker` and `podman`. It adds one small crate, `ctrlc`, so that interrupting follow mode exits 0 on every platform. `ask` and `--explain` reuse the HTTP client and Anthropic API integration from `add-ai-init`.
- **Ordering:**
  - `add-ai-init` must be applied before the `environment-assistant` tasks.
  - The docker log source is implemented with or after `add-docker-service-manager`.
  - Both this change and `add-docker-service-manager` modify the shell-integration `Tab completion` requirement, so whichever is archived second must merge the other's additions.
