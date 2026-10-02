# Proposal

## Why

When `devy up` fails, the user gets one `error:` chain. For example, a Nix attribute isn't found, a service fails its readiness check, a port is taken, or a hook exits non-zero. Turning that into a fix means knowing devy's config keys, the backend's quirks and where service logs live. devy already holds most of that context, including `devy.yml`, `devy.lock`, the selected backend, `devy check`'s findings and service logs. Combined with an LLM, it can explain the failure in plain language and propose a concrete `devy.yml` fix.

## What Changes

- **New `devy doctor` subcommand.** It diagnoses the project's environment and the most recent `devy up` failure:
  - It always runs the same deterministic checks as `devy check` and reports their findings.
  - When AI assistance is available (see the `ai-assist` capability from `add-ai-init`), it sends a redacted bundle to the model and prints a diagnosis: summary, likely cause and suggested steps. The bundle contains the failure record, `devy.yml`, `devy.lock`, platform and backend, check findings, and recent log lines for affected services (via `add-service-logs`).
  - Without AI (no API key, or `--no-ai`), it prints only the deterministic findings plus a note that AI diagnosis is unavailable and how to enable it. It still exits 0.
- **Suggested `devy.yml` fixes.** When the model proposes a config change, devy shows it as a unified diff and validates the result as a loadable, valid `devy.yml` before offering it. It writes the change only after the user confirms at an interactive prompt, or with `--yes`. A non-interactive stdin without `--yes` never applies a fix. Suggested shell commands are printed and never executed.
- **`devy up` records failures.** When `devy up` fails inside a project, it writes a failure record to `.devy/last-up-failure.json` containing the error chain, the step that failed, the dependency involved, platform, backend and timestamp. After the `error:` line it prints a hint to run `devy doctor`. A successful `devy up` removes the record. Neither the record nor the hint makes a network call.
- **Completions and help** include `doctor` and its flags.

## Capabilities

### New Capabilities
- `environment-doctor`: the `devy doctor` command. Covers the inputs it gathers, deterministic findings, AI diagnosis output, the no-AI fallback, suggested fixes (diff, validation, confirmation, `--yes`, non-interactive safety) and exit codes.

### Modified Capabilities
- `environment-up`: `devy up` writes a failure record on failure, clears it on success and prints a `devy doctor` hint.
- `cli`: `doctor` joins the built-in subcommand list.
- `shell-integration`: completion includes `doctor` and its `--yes` and `--no-ai` flags.

## Impact

- **Code:**
  - new `src/commands/doctor.rs`
  - `src/cli.rs` (new subcommand)
  - `src/main.rs` / `src/commands/up.rs` (failure recording and the hint)
  - `src/commands/check.rs` (findings exposed as data rather than only printed)
  - `src/commands/hook.rs` (completions)
- **Dependencies on sibling changes:**
  - `add-ai-init` provides the `ai-assist` capability: API client, key and model configuration, secret redaction, payload preview and consent, and a mockable transport.
  - `add-service-logs` provides log retrieval for services.
  - Both must land first.
- **In-flight `add-docker-service-manager`:**
  - Also modifies `shell-integration`'s Tab completion requirement, so whichever change archives second must merge both completion additions.
  - Docker-managed services' logs and runtime status reach doctor through `add-service-logs` and the check findings without extra work here.
- **Files:** `.devy/last-up-failure.json` is a new local, uncommitted file under the existing `.devy/` directory.
- **No new crates.** HTTP goes through `ureq` via `ai-assist`.
