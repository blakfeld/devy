# Tasks

## 1. Reproduce the bug

- [ ] 1.1 Add a `tests/cli.rs` case, `doctor_yes_keeps_redacted_values`. It uses `project_with_failed_up`-style setup with a `devy.yml` containing a `meilisearch` dependency with `master_key: "realkey"` and a comment line that redaction changes (for example `# API_TOKEN=abc123`). The fake claude reply is `redact::text` of that file with only a port changed. Assert that after `devy doctor --yes` the file has the new port, still contains `realkey` and the original comment, and contains no `<redacted>`. Verify the test fails on current `main`, where the file gets `master_key: <redacted>`.
- [ ] 1.2 Add a `tests/cli.rs` case, `doctor_refuses_proposal_that_edits_a_redacted_line`. The fake reply re-indents or renames the `master_key: <redacted>` line. Assert, both with `--yes` and without a TTY, that stdout or stderr contains `it would write the "<redacted>" placeholder over values hidden from claude`, that no `Suggested fix` diff appears, and that `devy.yml` is byte-identical. Verify it fails on current `main`.

## 2. Restore redacted lines (environment-doctor)

- [ ] 2.1 Implement `restore_redacted(original, proposed)` in `src/commands/doctor.rs` per design D1, reusing `diff_ops`. Add unit tests for these cases:
  - a single-line value restored
  - a redacted comment restored
  - a PEM block and a `key: |` block scalar (multi-line collapse) restored
  - a line edited, so it is not restored
  - a redacted line duplicated into a new place, so it is not restored
  - two identical redacted lines in different sections, with one section moved, so the moved one is not restored
  - a round-trip property, `restore_redacted(x, &redact::text(x)) == x`, for a fixture exercising every `RULES` entry

  Verify with `cargo test restore_redacted`.
- [ ] 2.2 Add the placeholder guard: count `redact::REDACTED` in the restored proposal against the CRLF-normalized current file, and discard with the spec's warning when the count rose. Add a unit test where the current file already contains `<redacted>` and is not refused. Verify with `cargo test`.
- [ ] 2.3 Wire both into `run` in the order of design D2: after `has_hidden_text`, before `validate_with_backend`. Make the diff, `config_diff::diff` and `offer_fix` use the restored text. Verify that tasks 1.1 and 1.2 now pass and that existing doctor tests (`doctor_yes_never_adds_a_hook`, `doctor_diagnoses_with_claude_and_never_applies_fix_without_tty`) still pass.

## 3. Prompt rule

- [ ] 3.1 Extend `REPLY_RULES` in `src/commands/doctor.rs` with the copy-redacted-lines-unchanged rule (design D3). Add an assertion to an existing `--show-context` or request unit test that the rule appears in the request. Verify with `cargo test`.

## 4. Spec, docs and checks

- [ ] 4.1 Check that README or `docs/` text describing `devy doctor` fixes mentions that redacted values are kept, and that a proposal editing them is refused. Update it if it describes the fix flow. Verify by reading the rendered section.
- [ ] 4.2 Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`. All must pass.
- [ ] 4.3 Run the `code-reviewer` and `security-analyst` reviews on the diff and address every finding. Verify that both come back clean.
