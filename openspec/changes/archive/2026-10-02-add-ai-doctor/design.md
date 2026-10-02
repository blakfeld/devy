# Design

## Context

- `devy up` (`src/commands/up.rs`) returns an `anyhow::Error`. `main.rs` prints `error: {err:#}` and exits 1. Nothing about the failure is kept after the process exits.
- `devy check` (`check_impl` in `src/commands/check.rs`) prints and counts issues as it goes, then returns `SilentExit(1)`. Its findings exist only as printed output. Hard config errors short-circuit with `?`.
- `src/output.rs` provides the markers (`header`, `step`, `success`, `info`, `warn`). No code reads from stdin interactively yet.
- AI access (client, key and model configuration, redaction, preview and consent, mockable transport) comes from the `ai-assist` capability in `add-ai-init`. Service log retrieval comes from `add-service-logs`. This change consumes both and defines neither.

## Goals / Non-Goals

**Goals:**
- Doctor is useful with no network: deterministic findings always print.
- Every AI-sourced change to the user's files passes through validation and explicit consent.
- `devy up`'s failure path stays offline and cheap.

**Non-Goals:**
- Auto-running suggested commands, or re-running `devy up` after a fix.
- Fixes to files other than `devy.yml` (lock, hooks, the user's code).
- Multi-turn chat. Free-form Q&A belongs to the `devy ask` work in `add-service-logs`.
- Recording failures of commands other than `devy up`.

## Decisions

### 1. Failure record written from `up::run`, typed step tracking
`up_impl` keeps a small `UpProgress { step, dependency }` that it updates at each phase boundary in the spec's step list, plus the current dep inside the install and service loops. `up::run` wraps `up_impl`. On `Err`, it serializes `{ error_chain, step, dependency, platform, backend, devy_version, timestamp }` to `.devy/last-up-failure.json` through a temp file and rename. It sets mode 0600 on Unix. On `Ok` it removes the file.

The hint has to appear *after* `error:`, which `main.rs` prints. So `up::run` wraps the error in a `HintedError { inner, hint }`. `main.rs` prints `error: {inner:#}` and then the hint. This keeps `SilentExit` handling intact and leaves other commands unaffected.

- *Alternative:* parse the stderr text in doctor. That is brittle and loses the step and dependency.
- *Alternative:* have `main.rs` record every command's failure. That is too broad, and `check`'s `SilentExit` is not a failure worth diagnosing.

`--dry-run` routes to `check::run` in `cli.rs` and never reaches `up::run`, so it records nothing for free. Config-load failures before the root is known (no `devy.yml`) have no place to write and are skipped. A YAML parse error *after* the root is found is recorded, with `step = "load config"`. That requires splitting `DevyConfig::load_with_root` into locate and parse inside `up::run`.

### 2. `check_impl` split into `collect_findings` and a printer
Extract a `Findings { issues: Vec<Finding>, warnings: Vec<String>, hard_error: Option<anyhow::Error> }` collector. `check_impl` becomes "collect, then print, then decide the exit code", with output byte-identical to today, which existing tests guard. Doctor calls the collector directly and converts `hard_error` into a finding instead of bailing. One source of truth means doctor and check can't drift.

The dependency and environment tables in `shared.rs` currently print while counting. The collector reuses their counting logic. Doctor prints the same tables under its `Checks` header.

### 3. Structured AI response
Doctor asks `ai-assist` for a JSON object:

```json
{ "summary": "...", "likely_cause": "...", "steps": ["..."], "devy_yml": "<full proposed file or null>" }
```

The request goes through ai-assist's structured-output helper, if it offers one, or otherwise a JSON-only instruction plus parse-and-retry-once.

- *Full file, not a patch.* Model-generated unified diffs often misapply. devy computes the diff itself from the current and proposed text, so what the user sees is exactly what gets written. The prompt tells the model to preserve comments and ordering, and the diff makes any loss visible.
- *Alternative:* RFC 6902 JSON Patch over parsed YAML. Rejected because it loses comments and formatting on write.

The system prompt includes a compact reference of devy's `devy.yml` schema, known module keys (from `known_extra_keys`) and backend notes. It does not dump the whole README.

### 4. Validation before offering
The proposed text goes through the same parse and normalization as `DevyConfig` loading, plus `pm.validate_config` per dependency, `known_extra_keys`, shell validation and read-only port resolution. These are the hard-error and config-issue parts of check, not install, service or env state. Unknown-key or invalid-shell findings make the suggestion invalid too, since the fix shouldn't introduce new issues.

### 5. Applying the fix
The diff is a minimal line-based unified diff, implemented in-crate with a simple LCS over lines to avoid adding a crate. Inputs are small. Confirmation uses `std::io::IsTerminal` on stdin. The write is atomic: temp file in the same directory, then rename, preserving the original file's permissions. There is no `.bak`: `devy.yml` is committed, so git is the undo.

### 6. Log gathering
For services named in the failure record or reported stopped or not ready by the findings, doctor calls the `add-service-logs` retrieval API for the last 50 lines. Missing logs (unsupported backend, no file) are noted in the bundle as "no logs available" rather than failing.

### 7. Testability
The AI transport is the mockable one from `ai-assist`. The prompt reader is injected as a `&mut dyn BufRead` plus an `is_tty` flag. CLI integration tests in `tests/cli.rs` cover: no key (offline path), `--no-ai`, a non-tty that doesn't apply, the failure record lifecycle using a failing `before_up` hook, and the hint text. AI-path tests run at the unit level with a canned response.

## Risks / Trade-offs

- [Secrets in `devy.yml`, logs or the error chain sent to a third party] → All payloads go through `ai-assist` redaction and preview and consent. Doctor sends only the listed inputs, never the environment file or arbitrary files.
- [Model proposes a plausible but wrong config] → Validation catches invalid configs. The diff and the default-No prompt keep the user in control, and doctor never runs `up` itself.
- [Prompt injection via log lines or `devy.yml` content steering the model] → The output is constrained to text plus a `devy.yml` proposal that is validated and confirmed. Commands are never executed, so the blast radius is a suggested config diff the user reviews.
- [Failure record leaks into git] → It lives under `.devy/`, which already holds the nix profile and is expected to be ignored. Tasks add a README note recommending `.devy/` in `.gitignore`.
- [Comments lost by a full-file rewrite] → The diff shows it, and the user can decline.
- [Refactoring `check_impl` changes its output] → Existing check tests plus a golden-output test must stay green before doctor work starts.
- [Merge conflict with `add-docker-service-manager` on the `shell-integration` Tab completion requirement] → Whichever archives second merges both completion lists.

## Migration Plan

This change is purely additive. Existing projects get the failure record and hint on their next failed `devy up`. Rollback means removing the subcommand. Leftover `.devy/last-up-failure.json` files are harmless.

## Resolved During Implementation

- `ai-assist` landed as a wrapper around the user's `claude` CLI, with no API key and no interactive consent step. "AI available" means `claude` is on `PATH`. Consent is covered by `ai-assist`'s `--show-context` preview, so doctor accepts `--show-context` and labels its diagnosis with the model, as every AI command must.
- `add-service-logs` has not landed. Doctor reads logs through a small `LogSource` seam that selects affected services only. Until a retrieval API exists, the production source returns none and the bundle notes `no logs available` (the §6 fallback). Wiring in the real API is a one-line change when that work lands.
