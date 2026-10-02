# Proposal

## Why

`devy init` writes `name: my-project` and `dependencies: []`, so every new user starts from a blank file and has to learn the module names, extra keys, port variables and command syntax before devy does anything useful. Most of that information already sits in the repo, in `package.json`, `.tool-versions`, `Gemfile`, `pyproject.toml`, `docker-compose.yml`, `.env.example`, CI workflows and `Makefile`s. devy can read it and draft the config, offline for the obvious cases and with Claude for the rest.

This change also adds the shared AI plumbing that later changes (`add-ai-doctor`, and `devy ask` in `add-service-logs`) build on.

## What Changes

- **`devy init --detect`** scans the current directory with fixed rules (no network) and writes a draft `devy.yml` with:
  - detected runtimes and versions (`.nvmrc`, `.node-version`, `.tool-versions`, `.ruby-version`, `.python-version`, `rust-toolchain.toml`, `go.mod`)
  - services from `docker-compose.yml` images (postgres, mysql, redis, …)
  - `commands:` from `package.json` scripts and `Makefile` targets
  - `environment:` from `.env.example` keys, rewritten to use devy's injected `*_HOST` / `*_PORT` variables where they match a detected service
- **`devy init --ai`** runs the same scan, then sends a size-capped, redacted summary of it, plus devy's module catalog, to Claude, and asks for a complete `devy.yml`. devy checks the reply against the config schema and module rules before writing anything. If the check fails, it retries once with the errors attached. If the second reply also fails, devy writes nothing and exits 1.
- **`devy init --ai --show-context`** prints exactly what would be sent and exits without running `claude`.
- Every generated file starts with a header comment saying how it was produced, and notes any detected item devy could not map as a `# TODO:` comment.
- The existing `--force` rule applies to every mode. Plain `devy init` stays exactly as it is today.
- **New shared AI capability (`ai-assist`)**, used only by commands the user runs explicitly:
  - runs the user's own `claude` CLI (Claude Code) non-interactively, with tools, MCP servers and project settings disabled, so devy handles no API keys and uses whatever account the user is signed in with
  - uses `claude`'s default model, overridable with `DEVY_AI_MODEL`
  - redacts secret-looking values before anything leaves the machine
  - applies a timeout and gives a clear error when `claude` is missing or fails
  - never runs `claude` in tests
- **Interaction with `add-docker-service-manager`:** once that change lands, services detected from `docker-compose.yml` can come out as `service_manager: docker`. Until then they are emitted as ordinary service dependencies.

## Capabilities

### New Capabilities
- `ai-assist`: the opt-in AI layer shared by devy's AI commands. Covers running the `claude` CLI and model configuration, what may and may not be sent, redaction, previewing the context, timeouts, error messages, and the rule that `claude` only runs when an AI command was explicitly invoked.

### Modified Capabilities
- `project-config`: the Init command requirement gains `--detect`, `--ai` and `--show-context`, with detection rules, validation before writing, and the header comment. Plain `init` and the `--force` rules are unchanged.

## Impact

- **Code:**
  - `src/cli.rs` gets new `Init` flags.
  - `src/commands/init.rs` gains the mode dispatch.
  - New `src/init_detect/` holds the deterministic detectors.
  - New `src/ai/` holds the client, redaction and prompt building.
  - `src/commands/check.rs` gets a static validation helper split out of `check_impl` that needs no package manager or env manager.
  - `src/modules/mod.rs` gets an exported catalog of module names, aliases, service flag, default port and known extra keys.
- **Dependencies:** no new crates. `--ai` needs the `claude` CLI installed and signed in at runtime; nothing else does.
- **Network and privacy:** this is the first time devy sends project content to an AI provider, through `claude`. It happens only with `--ai`, and its contents can be previewed with `--show-context`.
- **Docs:** README sections for `init --detect` / `--ai`, the `claude` CLI requirement and the `DEVY_AI_MODEL` variable.
