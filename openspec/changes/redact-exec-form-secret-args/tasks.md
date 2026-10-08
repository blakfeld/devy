# Tasks

## 1. Reproduce the leak

- [ ] 1.1 In `src/ai/redact.rs` tests, add `exec_form_secret_args_redacted`. It asserts exact `text()` output for each confirmed case:
  - `command: ["redis-server", "--requirepass", "hunter2"]`
  - `test: ["CMD", "redis-cli", "-a", "hunter2", "ping"]`
  - the block list `command:\n  - redis-server\n  - --requirepass\n  - hunter2\n`
  - `command: [redis-server, --requirepass, hunter2]`
  - `["mongod", "--password", "hunter2"]`
  - `["sqlcmd", "-P", "hunter2"]`
  - `["CMD", "mysqladmin", "ping", "-phunter2"]`
  - a three-line flow sequence

  Verify with `cargo test exec_form_secret_args_redacted` that the test fails on the current code.
- [ ] 1.2 Add `exec_form_glued_value_keeps_structure`. It asserts that `args: ["--api-key=abc123", "--port", "80"]` becomes `args: ["--api-key=<redacted>", "--port", "80"]` and that `["mysqld", "--password=hunter2"]` keeps its closing `"]`. Verify it fails on the current code.
- [ ] 1.3 Add `exec_form_non_secret_args_kept`. It asserts that these are unchanged:
  - `["redis-server", "--port", "6380", "--appendonly", "yes"]`
  - `["mysql", "-p", "appdb"]` (D4)
  - `["x", "--no-auth", "--port", "1"]`
  - a block list of package names

  Verify it passes on the current code, as a guard against over-redaction.
- [ ] 1.4 In `tests/cli.rs`, add `init_show_context_redacts_exec_form_compose`. It writes a `compose.yaml` with an exec-form redis `command` and `healthcheck.test`, runs `devy init --show-context`, and asserts that the context does not contain `hunter2` and does contain `"--requirepass", "<redacted>"` (spec scenario "Exec-form compose command"). Verify it fails on the current code.

## 2. Shared argument walker

- [ ] 2.1 Implement the walker (D1). It takes element spans, keeps the tool context (`redis-cli`, `sqlcmd`, `login`/`sshpass`/`mongo*`, MySQL/MariaDB clients) and handles three kinds of element:
  - secret-named flags, where it redacts the next element unless that element starts with `-`;
  - tool-scoped short flags;
  - glued values (`--<secret>=`, `-pX`).

  Verify with direct unit tests on span lists, including basename matching of `/usr/local/bin/redis-cli`.

## 3. Flow sequences

- [ ] 3.1 Implement the flow front end and register `exec-form-arg` in `RULES` after `curl-user` and before `xml-element`/`key-assignment` (D2):
  - it starts at a `[` in a value position;
  - it handles quoted and plain elements;
  - it crosses lines and stops at a blank line;
  - it skips sequences with nested collections.

  Verify that the flow cases from 1.1 pass, along with 1.3, and that `cargo test` in `src/ai/redact.rs` stays green.
- [ ] 3.2 Make `key-assignment` stop its value scan at the closing quote of a quoted flow element (D3). Verify that 1.2 passes, that `["PASSWORD=x", "y"]` keeps `, "y"]`, and that the existing JSON and `.npmrc` tests still pass.
- [ ] 3.3 Extend the linear-time test with a 100 KiB single-line flow sequence of alternating `"--password", "x"` elements. Verify that it completes within the existing time bound.

## 4. Block sequences

- [ ] 4.1 Implement the block front end (D1, D5):
  - it groups runs of `- <scalar>` lines at one indent, skipping comments and blanks;
  - it excludes mapping items and `|`/`>` headers;
  - it strips quotes from each item;
  - it rewrites a redacted item as `<lead>- <redacted>`, keeping the original quotes when present.

  Verify that the block-list cases from 1.1 and 1.4 pass, along with a quoted block list (`- "--password"` / `- "hunter2"`), and that the `nested_values` tests stay green.

## 5. Spec and docs

- [ ] 5.1 Add exec-form command arguments to the "What is redacted" list in `README.md` (AI features). Verify that the README example output matches what `devy init --show-context` prints for the 1.4 fixture.
- [ ] 5.2 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`, and verify that all pass. Then run the `code-reviewer` and `security-analyst` reviews on the diff, as the project rules require.
