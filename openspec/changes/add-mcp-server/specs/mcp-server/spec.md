# Spec Delta

## Purpose

`devy mcp` runs a Model Context Protocol server over stdio. It gives coding agents structured, permission-scoped access to a devy project's services, ports, environment and commands.

## ADDED Requirements

### Requirement: Stdio MCP server
`devy mcp` SHALL run an MCP server that reads newline-delimited JSON-RPC 2.0 messages from stdin and writes newline-delimited JSON-RPC 2.0 messages to stdout. It SHALL serve requests one at a time, in arrival order, until stdin reaches end-of-file. Then it SHALL exit 0. A line that is not valid JSON SHALL get a JSON-RPC parse error response (`-32700`). An unknown method SHALL get a method-not-found error (`-32601`). Neither SHALL stop the server.

#### Scenario: Clean shutdown on EOF
- **WHEN** the client closes stdin
- **THEN** `devy mcp` exits with status 0

#### Scenario: Malformed input
- **WHEN** the client sends a line that is not valid JSON
- **THEN** devy writes a JSON-RPC error response with code `-32700`
- **AND** continues serving later requests

#### Scenario: Unknown method
- **WHEN** the client sends a request with method `resources/list`
- **THEN** devy responds with a JSON-RPC error with code `-32601`

### Requirement: Protocol handshake
The server SHALL answer `initialize` with:
- a supported protocol version
- server info naming `devy` and its crate version
- a `tools` capability

If the client requests a protocol version that devy supports, devy SHALL respond with that version. Otherwise devy SHALL respond with the newest version it supports. The server SHALL accept the `notifications/initialized` notification without responding, and SHALL answer `ping` with an empty result.

#### Scenario: Supported version echoed
- **WHEN** the client sends `initialize` with a protocol version devy supports
- **THEN** the response's `protocolVersion` equals the requested version
- **AND** `serverInfo.name` is `devy`

#### Scenario: Unsupported version
- **WHEN** the client sends `initialize` with protocol version `1999-01-01`
- **THEN** the response's `protocolVersion` is the newest version devy supports

### Requirement: Stdout carries only protocol messages
While `devy mcp` runs, every byte written to stdout SHALL be part of a JSON-RPC message. Progress markers, tables and warnings that devy's commands normally print SHALL go to stderr or be captured into tool results. The stdout and stderr of child processes started by a tool SHALL be captured and SHALL NOT be inherited by the server's stdout.

#### Scenario: Warnings do not corrupt the stream
- **WHEN** a tool call triggers a devy warning, such as an unapplied port under Nix
- **THEN** every line on stdout parses as a JSON-RPC message
- **AND** the warning appears on stderr or in the tool result text

#### Scenario: Child output is captured
- **WHEN** `run_command` runs a command that prints to stdout
- **THEN** that output appears in the tool result
- **AND** it is not written to the server's stdout outside a JSON-RPC message

### Requirement: Project discovery
The server SHALL locate `devy.yml` from its working directory using the same discovery rules as other devy commands. It SHALL re-read `devy.yml` and `devy.lock` on every tool call, so edits made while the server runs take effect. If no `devy.yml` is found, the server SHALL still start and complete the handshake. Each tool call SHALL then return a tool error containing `devy.yml not found — are you inside a devy project?`.

#### Scenario: Started outside a project
- **WHEN** `devy mcp` starts in a directory with no `devy.yml` and the client calls `services`
- **THEN** the call returns a result with `isError: true` whose text contains `devy.yml not found`

#### Scenario: Config edited while running
- **WHEN** a dependency is added to `devy.yml` after the server started and the client calls `services`
- **THEN** the result includes the new service

### Requirement: Read-only tools
`tools/list` SHALL always advertise these tools:

| Tool | Returns |
|---|---|
| `status` | The information `devy status` reports |
| `services` | Each service dependency's name and whether it is running |
| `ports` | Each service's resolved host and port, using the same resolution as `devy up` (including `devy.lock` reuse) |
| `get_env` | The environment variables and PATH entries devy would activate for the project |
| `list_commands` | Each project command's name and command line |
| `check` | The same validation as `devy check`, with the issue count and whether it passed |

Each tool SHALL declare a JSON Schema for its input. Each result SHALL contain a human-readable text content block. Where the data is structured, the result SHALL also contain `structuredContent` with the same data. Read-only tools SHALL NOT install, start, stop or write anything, including `devy.lock` and shadowenv files.

#### Scenario: Tool catalog without mutations
- **WHEN** `devy mcp` runs without `--allow-mutations` and the client calls `tools/list`
- **THEN** the result lists exactly `status`, `services`, `ports`, `get_env`, `list_commands` and `check`

#### Scenario: Ports are structured
- **WHEN** `devy.yml` declares `redis` and the client calls `ports`
- **THEN** `structuredContent` contains an entry for `redis` with its host and port

#### Scenario: Check failure is not a protocol error
- **WHEN** `devy check` would find 2 issues and the client calls `check`
- **THEN** the tool returns a normal result that reports 2 issues and that the check did not pass
- **AND** the result is not a JSON-RPC error

#### Scenario: Read-only tools write nothing
- **WHEN** the client calls every read-only tool in a project with no `devy.lock`
- **THEN** no `devy.lock` or `.shadowenv.d` file is created

### Requirement: Secret redaction in get_env
`get_env` SHALL replace a variable's value with `<redacted>` when the variable's name contains any of `PASSWORD`, `PASSWD`, `SECRET`, `TOKEN`, `KEY` or `CREDENTIAL`, ignoring case. For any value that is a URL with a password in its userinfo, `get_env` SHALL replace only that password with `<redacted>`. There SHALL be no option to disable redaction.

#### Scenario: Secret-named variable
- **WHEN** `devy.yml` sets `environment: { STRIPE_SECRET_KEY: "sk_test_123" }` and the client calls `get_env`
- **THEN** the result shows `STRIPE_SECRET_KEY` with value `<redacted>`
- **AND** `sk_test_123` appears nowhere in the result

#### Scenario: URL password
- **WHEN** `DATABASE_URL` resolves to `postgres://app:hunter2@127.0.0.1:5432/app`
- **THEN** `get_env` shows `postgres://app:<redacted>@127.0.0.1:5432/app`

#### Scenario: Ordinary variable
- **WHEN** `devy.yml` sets `LOG_LEVEL: debug`
- **THEN** `get_env` shows `LOG_LEVEL` as `debug`

### Requirement: Mutating tools require opt-in
The mutating tools `start_service`, `stop_service`, `restart_service`, `up` and `run_command` SHALL be advertised only when the server was started with `devy mcp --allow-mutations`. No setting in `devy.yml` or `devy.lock` SHALL enable them. Calling a mutating tool on a server started without the flag SHALL return a tool error stating that the server was started read-only and naming `--allow-mutations`.

#### Scenario: Mutations disabled
- **WHEN** `devy mcp` runs without `--allow-mutations` and the client calls `start_service`
- **THEN** the result has `isError: true` and its text mentions `--allow-mutations`
- **AND** no service is started

#### Scenario: Mutations enabled
- **WHEN** `devy mcp --allow-mutations` runs and the client calls `tools/list`
- **THEN** the result lists the read-only tools plus `start_service`, `stop_service`, `restart_service`, `up` and `run_command`

### Requirement: Service tools
`start_service`, `stop_service` and `restart_service` SHALL each take a required `name` and behave like `devy start`, `devy stop` and `devy restart` with that name. The result text SHALL contain the captured output of the operation. When the underlying command fails, including for an unknown or non-service name, the result SHALL have `isError: true` and contain the same error message the CLI would print.

#### Scenario: Start a service
- **WHEN** the server allows mutations, `devy.yml` declares `redis`, and the client calls `start_service` with `name: "redis"`
- **THEN** redis is started as `devy start redis` would start it
- **AND** the result is not an error

#### Scenario: Unknown service
- **WHEN** the client calls `stop_service` with `name: "nope"`
- **THEN** the result has `isError: true` and contains the CLI's error message for an unknown service

### Requirement: Up tool
The `up` tool SHALL behave like `devy up` with no flags. It SHALL NOT bootstrap a package manager and SHALL NOT re-resolve versions. It SHALL accept no input that enables `--bootstrap` or `--update`. The result SHALL contain the captured output, and SHALL have `isError: true` when `devy up` would exit non-zero.

#### Scenario: Up runs without bootstrap
- **WHEN** the package manager is missing and the client calls `up`
- **THEN** the result has `isError: true` with the same error `devy up` reports
- **AND** no installer script is downloaded or run

### Requirement: Run command tool
`run_command` SHALL take a required `name` and an optional `args` array of strings. It SHALL run only a command defined in the `commands:` map of `devy.yml`, using that command's shell and working directory. It SHALL append `args` to the command line with the same quoting as `devy <name> args…`.

The command SHALL run with:
- the project environment devy activates, meaning the environment variables and PATH entries that `get_env` reports, without redaction
- stdin closed

The result SHALL contain the command's exit status and its captured stdout and stderr. Output longer than 64 KiB SHALL be truncated to its last 64 KiB, with a note that it was truncated. A command still running after the timeout SHALL be terminated and reported as timed out with `isError: true`. The timeout defaults to 600 seconds and can be lowered by an optional `timeout_secs` input. An undefined name SHALL return `isError: true` with the same `Unknown command '<name>'…` message the CLI uses. A non-zero exit SHALL return `isError: true`.

#### Scenario: Defined command runs with project environment
- **WHEN** `devy.yml` defines `commands: { test: "cargo test" }` and `REDIS_PORT` is resolved for the project, and the client calls `run_command` with `name: "test"`
- **THEN** `cargo test` runs with `REDIS_PORT` set in its environment
- **AND** the result contains its output and exit status 0

#### Scenario: Arbitrary shell is rejected
- **WHEN** the client calls `run_command` with `name: "rm -rf /"`
- **THEN** no process is spawned
- **AND** the result has `isError: true` and contains `Unknown command 'rm -rf /'`

#### Scenario: Arguments are quoted
- **WHEN** the client calls `run_command` with `name: "test"` and `args: ["a b; echo pwned"]`
- **THEN** the command receives `a b; echo pwned` as a single argument
- **AND** `echo pwned` is not run as a separate command

#### Scenario: Timeout
- **WHEN** the client calls `run_command` with `timeout_secs: 1` for a command that sleeps for 10 seconds
- **THEN** the command is terminated after about 1 second
- **AND** the result has `isError: true` and states that the command timed out
