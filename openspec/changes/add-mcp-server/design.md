# Design

## Context

See proposal.md for motivation. The current code has three properties that shape this design:

- **Commands print as they go.** `output::{header,step,success,info,…}` call `println!`. `service::list_impl` and `shared::print_*_table` print tables directly. Only `output::warn` writes to stderr. There is no "return data, then render" layer.
- **Child processes inherit stdio.** `exec::spawn_cmd`, hooks and package-manager installs inherit stdout and stderr. Status probes such as `is_running` and version queries use `.output()` and are already captured.
- **devy is synchronous** and has no async runtime. The dependencies are `clap`, `serde`, `serde_json`, `anyhow`, `ureq`, `colored`, `fs2` and `which`.

MCP's stdio transport requires that stdout carry only newline-delimited JSON-RPC. Any stray `println!` or inherited child stdout corrupts the session.

## Goals / Non-Goals

**Goals:**
- A dependency-free, synchronous MCP server (tools only) that works on macOS, Linux and Windows.
- Structural guarantees that stdout stays clean. This should not depend on remembering to avoid `println!` in every code path.
- Secure by default: read-only unless the person launching the server opts in.

**Non-Goals:**
- MCP resources, prompts, sampling, logging notifications, progress notifications and the Streamable HTTP transport.
- Long-running process management. `run_command` is for commands that finish (test, lint, migrate), not dev servers.
- A `logs` tool. It is planned as a follow-up once `add-service-logs` lands.
- Exposing `down`, `down --volumes`, `init`, `export`, `pr` or `hook`.

## Decisions

### D1. Hand-roll the protocol subset on `serde_json` instead of using an MCP crate
devy needs these methods:
- `initialize`
- `notifications/initialized`
- `ping`
- `tools/list`
- `tools/call`
- tolerance of `notifications/cancelled`

That is about 200 lines of request dispatch over `BufRead::lines()`. The official Rust SDK (`rmcp`) is built on tokio and async traits. It would pull an async runtime and its transitive dependencies into a small sync CLI, and it would force a tokio main or a nested runtime.

- **Alternative considered:** `rmcp`. It tracks spec changes automatically, but adds heavy dependencies and an async boundary around entirely blocking code. If devy later needs resources, HTTP transport or sampling, revisit this choice.
- **Pinned protocol versions:** devy supports `2025-06-18` (preferred) and `2025-03-26`. This is a list kept next to the handshake code. Version negotiation follows the spec: echo the requested version if supported, otherwise answer with the newest supported.
- **Assumption to verify at implementation time:** if a newer stable spec revision has shipped, check whether the tools-only subset changed. If it didn't, add the new version string to the supported list.
- **Structured output:** tool results use `content: [{type:"text"}]` plus `structuredContent` for structured tools. Each such tool declares an `outputSchema`, which is supported as of `2025-06-18`.

### D2. Keep stdout clean with an output sink switch plus captured children
- **Sink switch.** `output.rs` gains a process-wide sink switch (an atomic flag or `OnceLock`). `devy mcp` sets it to stderr before reading stdin. In that mode every `output::*` function writes to stderr. Colors are disabled with `colored::control::set_override(false)`, since stderr goes to the client's log.
- **Dedicated protocol writer.** The JSON-RPC writer is the only code that writes to the real stdout. It locks `std::io::stdout()`, writes one line and flushes.
- **Captured children.** Every tool that spawns processes captures their stdio, through `.output()` or piped handles. It never uses `Stdio::inherit()`.
- **Alternative considered:** redirecting fd 1 to fd 2 at startup with `dup2` and writing protocol messages to a saved fd. That is robust, but needs `unsafe`/libc on Unix and a different approach on Windows. The sink switch plus "tools never inherit stdio" is portable.
- **Enforcement:** an integration test drives the real binary and asserts every stdout line parses as JSON-RPC.

### D3. Read-only tools are in-process; most mutating tools run a child `devy`
- **Read-only tools** (`services`, `ports`, `get_env`, `list_commands`) run in-process through new data-returning helpers. For example, the service list returns `Vec<ServiceState>`, and `ports::resolve_ports` already returns data. These helpers print nothing.
- **`status` and `check`** share their rendering paths with the CLI, so they run in-process with a captured renderer. Concretely, factor `status_impl` and `check_impl` to write into a `String` or `impl Write` that the CLI sends to stdout and MCP returns as text. `check` additionally returns `{issues, passed}`.
- **`start_service`, `stop_service`, `restart_service` and `up`** spawn `std::env::current_exe()` with `start|stop|restart <name>` or `up`. They set `NO_COLOR=1`, close stdin, pipe stdout and stderr, and run in the project root. These paths call package managers, hooks and service managers that inherit stdio deep inside `up.rs` and `modules/`. A child process gives complete capture and exact CLI parity, including exit codes and the `error:` line, without threading a writer through roughly 3k lines.
- **Alternative considered:** a writer parameter threaded through `up_impl` and the modules. It is cleaner long-term, but a large refactor for little gain here.
- **`run_command` runs in-process, not as a `devy <name>` child.** A project command whose name shadows a built-in, such as `status`, could not be reached through the CLI. It uses a captured variant of `exec::spawn_cmd` (same shell validation, `cwd` and `append_extra_args` quoting), with piped stdio and a deadline.

### D4. `run_command` gets the project environment explicitly
An agent's MCP server process usually starts outside the shadowenv-activated shell, so a plain spawn would miss `REDIS_PORT`, the Nix profile `bin`, and similar entries. `run_command` computes the same env map and PATH prepends that `up` writes to shadowenv: module `env_vars`, merged with config `environment` through `merge_env`, plus `path_prepends` and lock-resolved ports. It sets these on the child.

`get_env` reports the same values with redaction applied. One shared resolver guarantees the two never disagree.

### D5. Mutation gating is a launch flag only
The opt-in is `devy mcp --allow-mutations`, chosen by whoever registers the server, for example `claude mcp add devy -- devy mcp --allow-mutations`.

- **Rejected:** a `devy.yml` key. A cloned repository could then grant an agent permission to run its own hooks and commands.
- **Without the flag:** mutating tools are left out of `tools/list` and also rejected in `tools/call`, so a client that ignores the list still cannot invoke them.

### D6. Timeouts and output caps
- **Timeout:** `run_command` waits with a deadline, 600 s by default, which `timeout_secs` can only lower. On timeout it kills the child. On Unix it uses a new process group plus `kill(-pgid)`, so shell grandchildren also die. On Windows it uses `Child::kill`.
- **Pipe draining:** stdout and stderr are drained on reader threads to avoid pipe deadlock.
- **Output cap:** output is kept as a 64 KiB tail ring.
- **Other mutating tools:** `up` and the service tools use the same runner with a 30-minute ceiling, since first-time Nix installs are slow.

### D7. Docker interplay (`add-docker-service-manager`)
- **`services` and `status`:** once that change lands, each service's `structuredContent` entry includes `backend: "package" | "docker"`. The tools use the shared service state, so this mostly falls out naturally.
- **`ports`:** uses the shared port resolution that change relies on, so it needs no special case.
- **Spec conflict:** both changes modify `shell-integration`'s *Tab completion* requirement. The second change to archive must merge both additions: `--volumes` after `down`, and `mcp` / `--allow-mutations`.

## Security Considerations

- **Trust boundary.** The MCP client (an agent driven by an LLM, possibly influenced by prompt injection from repo content) is semi-trusted. The person who launches the server is trusted. Read-only mode exposes no code execution: read-only tools only read config and lock files, query service state and resolve ports. They never run hooks or `after_install`.
- **No arbitrary execution.** `run_command` looks up `name` in `commands:` and rejects anything else before any spawn. `args` go through the existing `sh_quote` path. A spec scenario covers injection via `args`.

  With `--allow-mutations`, the agent can run everything `devy.yml` defines, including `before_up`/`after_up` hooks through `up`. This is equivalent to the agent typing `devy up`. The README must state this plainly.
- **Bootstrap is unreachable.** `up` never passes `--bootstrap` (remote installer script) or `--update`.
- **Secrets.** `get_env` redacts by name pattern and URL userinfo. Redaction is best-effort: a secret in an innocuously named variable (for example `API_URL=https://x?apikey=…`) can leak. The docs will say so.

  `run_command` children receive unredacted values, as they must. Their output is returned to the agent, which is inherent to running commands.
- **No network listener.** stdio only, so there is no port to protect.
- **Resource use.** Timeouts, the output cap and serial request handling bound what a misbehaving client can consume.
- **Audit trail.** Each mutating tool call is logged to stderr (`devy mcp: start_service redis`). Clients typically surface stderr in their MCP logs.

## Risks / Trade-offs

- **[Risk]** A future code path adds `println!` outside `output::*` and corrupts the stream. → **Mitigation:** the integration test asserts all stdout lines are JSON for every read-only tool. Add a clippy `disallowed_macros` lint for `println!` in `src/commands/mcp.rs`.
- **[Risk]** A child `devy` differs from the parent's `devy` version (for example a stale `current_exe` after `cargo install`). → **Mitigation:** `current_exe()` resolves the running binary path. Acceptable.
- **[Trade-off]** Spawning a child `devy` costs a process launch (a few ms) per mutating call. This is negligible next to service start times.
- **[Risk]** Protocol drift as MCP revises its spec. → **Mitigation:** version negotiation keeps old clients working. The supported-version list is a single constant with a unit test.
- **[Risk]** Name-based redaction misses secrets. → **Mitigation:** documented as best-effort. Patterns can be extended later without changing the tool interface.

## Migration Plan

This is additive and needs no data migration. Rollback means removing the subcommand. The README gains a "Using devy with coding agents" section:

```sh
claude mcp add devy -- devy mcp                      # read-only
claude mcp add devy -- devy mcp --allow-mutations    # lets the agent start services and run project commands
```

## Open Questions

- Whether to add a `logs` tool. This depends on `add-service-logs`. It would be a read-only tool returning the tail of a service's log, added in a follow-up change.
