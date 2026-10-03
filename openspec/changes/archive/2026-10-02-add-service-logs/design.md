# Design

## Context

Log locations today are a side effect of how each backend launches services:

- **nix on macOS:** `write_launchagent` in `src/package_manager/nix.rs` points both `StandardOutPath` and `StandardErrorPath` at `std::env::temp_dir()/devy-<name>.log`.
- **nix on Linux:** the systemd user unit has no `StandardOutput=` setting, so output goes to the user journal.
- **brew:** `brew services` writes to paths that each formula's plist chooses.
- **apt:** system units log to the system journal.
- **winget:** Windows services have no common log location.
- **docker** (`add-docker-service-manager`, not yet implemented): containers log to the container runtime.
- **Module-specific files:** some modules also write their own logs under `.devy/data/<name>/`. nginx writes `error.log` and `access.log`, the search servers write `logs/`, Kafka writes `app-logs/`, and RabbitMQ sets `RABBITMQ_LOG_BASE` to `log/`.

Service operations go through the `Module` trait, which delegates to the `PackageManager` trait using the backend service name (`Module::service_name`). `src/commands/service.rs::resolve` already does name resolution and "is a service" validation for `start`, `stop` and `restart`.

The AI half depends on the `ai-assist` capability from `add-ai-init`: the client, key and model config, opt-in, redaction, and the mockable transport. This design only uses that capability and does not specify it.

## Goals / Non-Goals

**Goals:**
- One log-source abstraction per backend, so every service-capable backend gains logs by implementing a single method.
- `devy logs` uses only `std` and existing system tools, with no new crates.
- The context collector is shared by `ask` and `--explain`, and is testable without a network.

**Non-Goals:**
- Changing where services log. For example, moving the macOS log into `.devy/` or giving the Linux unit a log file is out of scope. The shared `$TMPDIR/devy-<name>.log`, which is not per-project, stays as it is.
- Log rotation, search and filtering (`--since`, `--grep`), and structured parsing.
- Reading the Windows Event Log.
- Multi-turn chat in `devy ask`. Each run is a single question and answer.

## Decisions

### 1. `PackageManager::log_source(name) -> Result<LogSource>`
Logs belong to the backend, not the module, because the same redis module logs differently under nix, brew and docker. Add a method to the `PackageManager` trait that returns one of:

```
enum LogSource {
    Files(Vec<PathBuf>),          // tailed in-process
    Command { program, args },    // journalctl / docker logs, built per (lines, follow)
    Unsupported(String),          // message shown as the error
}
```

The default implementation returns `Unsupported` with the generic message, so the test doubles in `package_manager/mod.rs` keep compiling. `Module` gets a thin `log_source(pm, dep)` default that passes `self.service_name(dep)` through, the same way `is_running` does. The docker backend, when it lands, overrides this per-dependency. The `service_manager` dispatch introduced by `add-docker-service-manager` decides which backend answers.

*Alternative:* a method on `Module` with per-module overrides. Rejected because log location never depends on the module, only on the backend.

The `Command` variant takes `lines` and `follow` as arguments rather than baking them in, so each backend maps them to its own flags:
- `journalctl -n N [-f] --no-pager -o cat`
- `docker logs --tail N [-f]`

### 2. File tailing in Rust
Tailing files in Rust works the same on every platform and is easy to test with temp files. Last-N works by reading backwards from EOF in 8 KiB blocks until N newlines are found. Follow polls the file every 250 ms for growth. If the file shrinks, devy restarts from offset 0, which handles truncation when launchd rewrites the file on restart.

For brew, `log_path` and `error_log_path` from `brew services info --json` become `Files([...])`. With two files and no follow, the stdout file is shown and then the stderr file, each under a dimmed path label. With follow, the two files are polled together.

*Alternative:* shell out to `tail -n/-F`. Rejected because `tail` is not available on Windows, and Rust tailing makes the multi-source prefixing in Decision 3 uniform.

### 3. Multi-service output
Without follow, devy runs the services sequentially, printing `output::header(name)` and then the lines for each. With follow, it runs one reader thread per service, each sending `(name, line)` over an `mpsc` channel. The main thread prints `<name padded> | line`.

- **Command sources:** stdout and stderr are both piped and read line by line.
- **Single service:** the source's stdout and stderr are inherited, so colors and the `journalctl` pager behavior pass through unchanged.

Ctrl-C ends the process. The child processes share the foreground process group, so they get the signal too. On SIGINT the exit status is mapped to 0 so it meets the spec. devy has no signal handling today and `std` provides none, so this adds the small, widely used `ctrlc` crate, which is cross-platform including Windows console Ctrl-C. Its handler sets a flag that the follow loop checks, and the command then returns `Ok(())`.

*Alternative:* `libc::signal` behind `cfg(unix)`. Rejected because it needs unsafe code and a separate Windows path, which `ctrlc` already handles.

### 4. "No logs yet" detection
- **Files:** the files do not exist or are empty.
- **journalctl:** run first with `-n N` and no follow. If it exits 0 with empty output, the service has no logs. If it exits non-zero with "No journal files" or a permission error on stderr, devy shows the permission hint for apt and treats the nix user journal as having no logs.
- **docker:** the container does not exist, which maps to `No logs yet`.

### 5. Extra log hints via `Module::extra_log_paths(data_dir) -> Vec<PathBuf>`
These files are owned by the module, since only nginx knows it writes `error.log`. The hook is called only when the backend is nix, because the paths come from nix launch definitions. Only existing paths are printed.

### 6. Context collector (`src/commands/ask.rs::collect_context`)
`collect_context` builds a plain-text, sectioned document: `devy.yml`, `devy.lock`, platform, a dependency status table, and per-service logs. It reuses:
- `DevyConfig::load_with_root`
- the same status probes as `devy status`
- `log_source` with `lines = 50`, read to completion with a short timeout (5 s per source), so a hung `journalctl` cannot stall `ask`

The whole document goes through `ai-assist` redaction. There is also a hard size cap (about 60 KB). When the cap is hit, the oldest log lines are dropped first. `--explain` uses the same builder narrowed to one service plus that service's `--lines` worth of log.

The prompts are fixed system-prompt strings in `ask.rs`:
- `ask`: answer about this devy environment, prefer concrete devy commands and `devy.yml` edits, and say when the context is insufficient.
- `explain`: give the most likely cause, the evidence lines, and the fix steps.

`--show-context` prints exactly the request body text, with the system prompt, context and question, and returns before `ai-assist` is invoked. That is why it works without an API key.

### 7. Message changes
- `src/commands/service.rs` warnings (lines 68 and 123) gain `— run devy logs <name>`.
- `wait_for_stopped` in `src/modules/mod.rs` changes its suffix.
- The `devy up` health warning (`up.rs:359`) is deliberately left unchanged, because it belongs to the `environment-up` spec. It can follow in a separate change.

## Risks / Trade-offs

- **[Risk]** The macOS log path is shared across projects, since two projects with a `redis` service write the same `$TMPDIR/devy-redis.log`. → Accept for now, because this is existing behavior. `devy logs` shows whatever is there, and the proposal is honest about the path. A per-project log path is a follow-up worth considering.
- **[Risk]** Older Homebrew versions may omit `log_path` and `error_log_path` from `brew services info --json`. → When neither key is present, return `Unsupported("brew did not report a log path for <name> — check $(brew --prefix)/var/log")`.
- **[Risk]** On apt systems the journal is not readable without a group, which degrades `devy logs`. → Show a clear hint (spec), and never escalate with sudo.
- **[Risk]** Logs can contain secrets, such as connection strings with passwords, and `ask` would send them. → Everything goes through `ai-assist` redaction, `--show-context` lets users audit what is sent, and log volume is capped at 50 lines per service.
- **[Trade-off]** Follow mode polls files instead of using filesystem notifications. 250 ms latency is fine for humans and avoids a `notify` dependency.
- **[Risk]** This change and `add-docker-service-manager` both modify the shell-integration `Tab completion` requirement. → Whichever change is archived second must rebase its delta to include the other's additions: `--volumes` after `down`, and the `logs`/`ask` entries.

## Migration Plan

This change is purely additive, with no config, lock or data migration. Rollout follows the task order:
1. `devy logs` for the existing backends.
2. The docker source, with or after `add-docker-service-manager`.
3. `ask` and `--explain` after `add-ai-init`.

Rolling back means removing the subcommands. The only other user-visible change is the message wording.

## Open Questions

- Should `devy logs` page its output (`less -R`) when stdout is a TTY and the output exceeds the screen? It is deferred because it does not change the spec, only presentation.
