# Design

## Context

- `src/commands/init.rs` writes a fixed string. The `--force` check is its only logic.
- `check_impl` (`src/commands/check.rs`) mixes two kinds of checks:
  - static checks: `normalized_dependencies`, `known_extra_keys`, `config_warnings`, `validate_shell`, explicit-port conflicts
  - environment checks that need a `PackageManager` and `EnvManager`: install status, written env vars

  A draft config has to be validated before anything is installed, so only the static half applies.
- Service env vars are `<CANONICAL>_HOST` / `<CANONICAL>_PORT`, derived from `modules::canonical_name` in `up.rs`. For example `postgres` → `POSTGRESQL_*`.
- The module registry and `ALIASES` are static tables in `src/modules/mod.rs`, so a catalog for the prompt can be generated from them rather than hand-maintained.
- `which` and `serde_json` are already dependencies. There is no async runtime.
- `add-ai-doctor` and `add-service-logs` (`devy ask`) will reuse the AI client defined here.

## Goals / Non-Goals

**Goals:**
- One small, synchronous AI client module shared by every AI command.
- Deterministic detection that is useful without AI, and that also seeds the AI prompt.
- Never write an invalid `devy.yml`.
- Fully testable without network access or a `claude` install.

**Non-Goals:**
- Streaming output, tool use, or multi-turn chat in `init`.
- Supporting other AI providers or local models. The `Transport` trait allows this later, but only the `claude` CLI is built.
- Scanning subdirectories or monorepo workspaces. `init` stays current-directory-only.
- Detecting dependencies from lockfile contents, such as gems that need system libraries.

## Decisions

### D1. `src/ai/` module that shells out to the `claude` CLI
`ai::Client { model: Option<String>, transport: Box<dyn Transport> }` with `fn complete(&self, req: &Request) -> Result<Reply>`, where `Reply` carries the text and the model `claude` reports in `modelUsage`. `Transport` has one method that takes the argument list and stdin and returns the process's success flag, stdout and stderr.

The production `CliTransport` runs the `claude` binary found by `which` (which resolves `claude.exe` / `claude.cmd` on Windows) as:

```
claude -p --output-format json --tools "" --strict-mcp-config --disable-slash-commands --no-session-persistence --system-prompt <short> [--model <DEVY_AI_MODEL>]
```

- The prompt goes on stdin. Only a one-line system prompt goes on the command line, because a Windows `.cmd` shim limits the command line to about 8 KB. The schema, catalog and rules travel on stdin.
- The working directory is an empty temp dir, so the scanned project's `.claude/settings.json` hooks, MCP config and `CLAUDE.md` are not loaded.
- `--bare` is not used because it forces `ANTHROPIC_API_KEY` auth and would ignore the user's signed-in account.
- devy kills the process after 5 minutes. `claude` startup plus a large-model reply can take longer than an API round trip.
- Failures come from the JSON `is_error` / `result` fields, or from stderr when stdout is not JSON.
- Retries for 429/529 are left to `claude`.

Tests inject a `FakeTransport` that returns canned JSON and records the arguments and stdin of every call.

- *Alternative:* calling the Anthropic Messages API over `ureq` with `ANTHROPIC_API_KEY`. This was built first and then replaced. It makes every user create and export an API key, and devy then has to keep that key safe. Shelling out reuses the account the user is already signed into.
- *Alternative:* an Anthropic SDK crate. Rejected because it adds an async runtime and dependencies.

devy never reads or handles credentials. Only AI code paths call `Client::from_env()`, so no other command looks for `claude`.

### D2. Redaction is a pure function over text and key/value pairs
`ai::redact::value(key, value)` and `ai::redact::text(s)` implement the spec's key-name list and value patterns (URL userinfo, PEM, known token prefixes). `--show-context` and the real request both go through the same `Request` builder, so the preview cannot differ from what is sent. Dotenv files are allow-listed: only `.env.example`, `.env.sample` and `.env.template` are read.

### D3. Detection is a list of independent detectors producing a `Draft`
`src/init_detect/` defines `trait Detector { fn detect(&self, dir: &Path, draft: &mut Draft) }`, with one detector each for node, ruby, python, rust, go, tool-versions, compose, package scripts and env-example.
- `Draft` holds ordered deps (name, version, extras), commands, env and `todos: Vec<String>`.
- Rendering to YAML is hand-written with stable ordering, rather than `serde_yml::to_string`, so the output has comments and is pleasant to read.
- The render is then re-parsed with `DevyConfig::load` in tests to guarantee validity.

Compose images are mapped to modules by stripping the registry and tag and running `modules::canonical_name`, plus a small table for image names that differ from module names (`mongo` → `mongodb`, `bitnami/kafka` → `kafka`, `docker.elastic.co/elasticsearch/elasticsearch` → `elasticsearch`). A tag is used as the version only when it starts with a digit.

The `.env.example` rewrite rule: if a value's host/port matches `localhost|127.0.0.1` and the default port of a detected service, replace them with `${<CANON>_HOST}` / `${<CANON>_PORT}`.

### D4. Split static validation out of `check_impl`
Add `pub(crate) fn static_issues(config: &DevyConfig) -> Result<Vec<String>>` to `check.rs`. It covers parse, normalization, unknown extra keys, invalid shells and explicit-port conflicts, all without a package manager. `check_impl` calls it and keeps its current output, so the existing check tests still pass unchanged.

Port-conflict detection uses the explicit ports in the draft only. For a not-yet-installed project there is no lock, so this is `ports::resolve_and_check` with no lock in a read-only mode that does not consult the package manager for applicability. If that turns out to need a package manager, assume the platform's default backend's `port_applicable` without probing it.

### D5. Prompt shape
- **System prompt:** one sentence naming the task (see D1 for why it is short).
- **Prompt (stdin):**
  - devy's `devy.yml` schema summary (from the README reference)
  - the module catalog as compact JSON
  - rules: output only YAML, prefer canonical names, build connection strings from `*_HOST` / `*_PORT`, never invent extra keys
  - the detected draft YAML
  - each redacted file as an `=== path ===` block

The reply is extracted from the first fenced `yaml` block if one exists, otherwise from the whole text. `claude -p` takes one prompt per run, so the repair request re-sends the original prompt followed by `=== your previous reply ===` and `=== follow-up ===` sections that carry the reply and the validation errors. Two requests at most.

### D6. CLI flags
`Init { force, #[arg(long, conflicts_with="ai")] detect, #[arg(long)] ai, #[arg(long, requires="ai")] show_context }`. clap enforces the usage errors (exit 2). The existing-file check runs first in `init::run`, before the scan, the `claude` lookup, or running `claude`.

### D7. Docker-compose and `add-docker-service-manager`
Until that change lands, compose services are emitted as plain dependencies. Once `service_manager` exists in `DevyConfig`, the compose detector adds `service_manager: docker` per dependency only when the project has no other evidence of Nix or brew use. This is gated on `DevyConfig` having the field, so whichever change merges second makes the one-line adjustment. No spec in this change depends on it.

## Risks / Trade-offs

- **[Model emits plausible but wrong versions or keys]** → Static validation catches unknown keys and parse errors. It does not catch versions, so the header comment says "review before committing", and `devy check` / `devy up` surface install problems.
- **[Leaking secrets from tracked files]** → Allow-list of readable files, key-name and value-pattern redaction, `--show-context` preview, and no reads of `.env`. Residual risk: a secret hard-coded in `README.md` with an innocuous name. This is documented.
- **[API cost or latency]** → One request normally, two at most, and context capped at 100 KiB.
- **[Prompt catalog drifting from modules]** → The catalog is generated from `REGISTRY` / `ALIASES` / `known_extra_keys` at runtime, with a unit test asserting every registry entry appears.
- **[Model ID changes]** → devy hard-codes no model. `claude` picks its default, and `DEVY_AI_MODEL` overrides it.
- **[`claude` CLI flags change]** → The flags are pinned in one function, `Request::args`, and covered by a unit test. An unknown-flag failure surfaces as `AI request failed: claude exited with an error: <stderr>`.
- **[User's own hooks still run]** → User-level Claude Code settings, including hooks, still apply; only project-level settings are excluded by the temp working directory. This is acceptable because they are the user's own configuration.

## Migration Plan

This change is additive. Plain `devy init` output and messages are unchanged, so the existing `tests/cli.rs` init tests must pass untouched. To roll back, remove the flags.

## Open Questions

- Whether to also offer `--ai` output as a diff against an existing `devy.yml` (an "improve my config" mode). This is deferred and does not affect this change.
