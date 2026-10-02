# Tasks

## 1. Static config validation

- [x] 1.1 Extract `static_issues(config) -> Result<Vec<String>>` from `check_impl` in `src/commands/check.rs`. It covers parse/normalization, unknown extra keys, invalid shells and explicit-port conflicts, and needs no package manager or env manager. Make `check_impl` use it with unchanged output. Verify that all existing `check_impl_*` unit tests and `tests/cli.rs` check tests pass unchanged, and that new unit tests cover `static_issues` on a config with `redis: { prot: 6380 }` (1 issue) and a clean config (0 issues).

## 2. Module catalog

- [x] 2.1 Add `modules::catalog()` returning, for each registry entry: canonical name, aliases, `is_service`, `default_port`, injected `<CANON>_HOST/_PORT` names (services only) and `known_extra_keys`. It must serialize to compact JSON. Verify with a unit test that every `REGISTRY` name and every `ALIASES` alias appears, and that `postgresql` lists `POSTGRESQL_HOST`.

## 3. AI client (`ai-assist`)

- [x] 3.1 Create `src/ai/mod.rs` with `Request`, `Reply`, `Client::from_env()` (finds `claude` on PATH, reads `DEVY_AI_MODEL`) and a `Transport` trait. Add `CliTransport`, which runs `claude -p --output-format json --tools "" --strict-mcp-config --disable-slash-commands --no-session-persistence --system-prompt <short> [--model …]`, sends the prompt on stdin, runs from an empty temp dir and has a 5-minute timeout. Verify with unit tests using a `FakeTransport`:
  - no `--model` by default, and `--model <override>` when set
  - tools, MCP and session persistence are disabled in the args
  - a JSON `is_error` reply yields `AI request failed: <claude's message>`
  - non-JSON failure output yields the last stderr line
  - a timeout or spawn failure yields `AI request failed: <reason>`
  - the reply model is taken from `modelUsage`
- [x] 3.2 Create `src/ai/redact.rs` with key-name and value-pattern redaction (secret key names, URL userinfo passwords, PEM blocks, `sk-`/`ghp_`/`github_pat_`/`xox`/`AKIA` tokens), and per-file (8 KiB) and total (100 KiB) truncation with the `… [truncated]` marker. Verify with unit tests for every spec scenario under Redaction and Bounded context size.
- [x] 3.3 Add `Request::render_preview()` used by `--show-context`, built from the same `Request` that would be sent. Verify with a unit test that the preview contains the model, the system prompt passed in the args and the exact stdin prompt.

## 4. Deterministic detection (`init --detect`)

- [x] 4.1 Create `src/init_detect/` with a `Draft` type, a `Detector` trait and a comment-preserving YAML renderer with stable ordering, plus the generated-by header and `# TODO:` lines. Verify with a unit test that an empty draft renders to plain-init content plus the header, and that it parses with `DevyConfig::load`.
- [x] 4.2 Implement runtime detectors (`.nvmrc`, `.node-version`, `package.json` engines, `.tool-versions`, `.ruby-version`, `.python-version`, `rust-toolchain(.toml)`, `go.mod`). Verify with unit tests using fixture temp dirs, one per source, asserting name and version.
- [x] 4.3 Implement the compose detector (`docker-compose.yml` / `compose.yaml`) using registry stripping, `canonical_name` and the image-name table, with digit-leading tags as versions. Unmapped images become TODOs. Verify with unit tests for `postgres:16`, `redis:7-alpine`, `mongo:7`, `bitnami/kafka:3.7` and an unmapped `myorg/api:latest`.
- [x] 4.4 Implement the package-scripts detector (npm/yarn/pnpm/bun chosen by lockfile) and the `.env.example` detector (service host/port rewrite to `${<CANON>_HOST}`/`${<CANON>_PORT}`, secret-looking values left as TODO). Verify with unit tests, including the `DATABASE_URL` → `postgres://${POSTGRESQL_HOST}:${POSTGRESQL_PORT}/app` scenario. Confirm `.env` is never read by placing a sentinel value in `.env` and asserting it is absent.
- [x] 4.5 Wire `--detect` into `src/cli.rs` (with `conflicts_with = "ai"`) and `src/commands/init.rs`, with the existing-file check first. Verify with `tests/cli.rs` cases:
  - the Node + compose fixture produces the expected `devy.yml`, and `devy check` on it reports no unrecognized keys
  - `--detect --ai` exits 2
  - the existing init tests still pass
- [x] 4.6 Document `devy init --detect` in README (Quick start and an init section listing the detected sources). Verify the README example commands match the clap help.

## 5. AI init (`init --ai`)

- [x] 5.1 Build the init prompt (system: one short sentence; stdin: schema summary, catalog JSON, rules, detected draft and redacted allow-listed files including `Makefile`, `Procfile`, `README.md` and `.github/workflows/*.yml`). Verify with a unit test using a fixture dir that the prompt contains the catalog and draft and omits `.env` contents.
- [x] 5.2 Implement reply extraction (fenced `yaml` block, otherwise the whole text), validation via `static_issues`, a single repair request carrying the errors, and writing with the AI header naming the model, followed by printing config warnings. Verify with `FakeTransport` unit tests:
  - valid first reply → 1 request, file written
  - invalid then valid → 2 requests, and the second contains the `prot` error
  - invalid twice → no file, exit-1 error mentions `devy init --detect`
- [x] 5.3 Wire `--ai` and `--show-context` (`requires = "ai"`) into the CLI. Verify with `tests/cli.rs`, with an empty PATH so `claude` is never run:
  - `init --ai` without `claude` exits 1 with the not-found message, suggests `--detect`, and writes no file
  - `init --ai --show-context` without `claude` exits 0, prints context, and writes no file
  - `--show-context` alone exits 2
  - an existing `devy.yml` without `--force` fails before looking for `claude`
- [x] 5.4 Document `devy init --ai`, `--show-context`, the `claude` CLI requirement, `DEVY_AI_MODEL`, how `claude` is sandboxed, the redaction rules and what is sent in the README. Verify the README mentions every env var and flag above.

## 6. Integration

- [x] 6.1 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. Verify all pass.
- [x] 6.2 Run `devy init --ai` manually against a real repo (for example a Rails + Postgres + Redis app) with a signed-in `claude`, then run `devy check`. Record the model, the number of requests and any manual edits needed in this task.
  - Smoke test (2026-10-02, not the real-repo run): a scratch fixture with `.nvmrc` and a `postgres:16` compose service. 1 request in about 5 s with `claude`'s default model, `claude-opus-5-5`. It wrote a valid config with `node` 22, `postgresql` 16 and `DATABASE_URL` built from `${POSTGRESQL_HOST}`/`${POSTGRESQL_PORT}`. No edits needed.
