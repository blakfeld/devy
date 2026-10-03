# Proposal

## Why

Coding agents such as Claude Code work inside devy projects, but they can't see what devy knows: which services are running, which ports were assigned, which environment variables a project needs, and which project commands exist. As a result they guess at ports, scrape colored terminal tables, or run `npm run dev` without the shadowenv environment. Exposing devy as a Model Context Protocol (MCP) server gives agents structured, permission-scoped access to that state, so an agent can notice "tests fail because redis is down" and start redis itself.

## What Changes

- **New `devy mcp` subcommand.** It runs an MCP server over stdio for the project discovered from the working directory, the same way other commands find `devy.yml`. Register it with Claude Code using `claude mcp add devy -- devy mcp`.
- **Read-only tools, always available:**
  - `status`: install, service and environment status, the same information as `devy status`.
  - `services`: service dependencies and whether each is running.
  - `ports`: each service's resolved host and port.
  - `get_env`: the resolved project environment, with secret-looking values redacted.
  - `list_commands`: the project commands defined in `devy.yml`.
  - `check`: validation results, the same as `devy check`.
- **Mutating tools, only with `devy mcp --allow-mutations`:**
  - `start_service`, `stop_service` and `restart_service`.
  - `up`, which never bootstraps a package manager or re-resolves the lock.
  - `run_command`, which runs only a command defined in `devy.yml` and never arbitrary shell.

  Without the flag, these tools are not advertised, and calling them returns an error.
- **stdout carries only protocol messages** while `devy mcp` runs. All human-oriented output (progress markers, tables, warnings) goes to stderr or is captured and returned as tool output.
- **Tab completion and help** list `mcp` and complete `--allow-mutations`.
- **No AI API calls.** devy only serves tools. The agent is the client.
- **Optional future tool.** A `logs` tool depends on the sibling change `add-service-logs` and will be added after that change lands, if it does. It is not part of this change.

## Capabilities

### New Capabilities
- `mcp-server`: the `devy mcp` stdio server. Covers protocol handshake and version, the tool catalog, read-only versus mutation gating, secret redaction, captured output, and the rule that stdout carries only protocol messages.

### Modified Capabilities
- `cli`: adds `mcp` to the built-in subcommands.
- `shell-integration`: tab completion includes `mcp` and its `--allow-mutations` flag.

## Impact

- **Code:**
  - New `src/commands/mcp.rs`, with a small JSON-RPC/MCP loop plus tool handlers.
  - A `Mcp` variant in `src/cli.rs`.
  - A stderr-only mode in `src/output.rs`.
  - Data-returning helpers factored out of `status`, `service`, `ports` and `list_commands`, which print directly today.
  - A captured-output variant of `exec::spawn_cmd`.
  - Completion lists in `src/commands/hook.rs`.
- **Dependencies:** none new. Uses the existing `serde_json`, with no async runtime.
- **Docs:** a README section on using devy with coding agents.
- **Interaction with `add-docker-service-manager`:**
  - That change also modifies `shell-integration` tab completion. Whichever change archives second must rebase its MODIFIED block onto the other.
  - Once it lands, `services` and `status` include each service's backend (`package` or `docker`).
  - `down --volumes` is deliberately not exposed.
