# Proposal

## Why

`devy doctor` sends `devy.yml` to claude after redaction and asks for the whole corrected file back, but writes the reply without restoring what redaction hid. A fix for an unrelated problem can therefore replace a real secret (for example a Meilisearch `master_key`) or a redacted comment with the literal text `<redacted>`. Non-executable fields do not block `--yes`, so `devy doctor --yes` writes that data loss silently. In interactive mode the placeholder shows in the diff, mixed in with the real fix, and is easy to accept by mistake.

## What Changes

- Before validating a proposed `devy.yml`, devy puts back the original content of every line that redaction changed and that the proposal leaves unchanged. The model then only has to copy the placeholder line through.
- After that step, a proposal that contains more `<redacted>` placeholders than the current `devy.yml` is discarded with `suggested devy.yml change was invalid and was not offered: it would write the "<redacted>" placeholder over values hidden from claude`. This covers a model that edits, moves or re-indents a redacted line. `devy.yml` is left unchanged in every mode, including `--yes`.
- The reply rules sent to claude tell it to copy every line containing `<redacted>` unchanged and never edit it.
- The diff, the executable-entry listing and the written file all use the restored text.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `environment-doctor`: the "Suggested configuration fix" requirement gains restoration of redacted values and refusal of proposals that would write the redaction placeholder.

## Impact

- `src/commands/doctor.rs`: the proposal path in `run` (between the hidden-text check, validation and `offer_fix`), plus `REPLY_RULES`.
- `src/ai/redact.rs`: no rule changes. The fix reuses `redact::text`, `redact::cap_file` and `redact::REDACTED`.
- Tests: unit tests in `doctor.rs`, and `tests/cli.rs` cases using the fake claude.
- No config, lock or CLI-flag changes, and no new dependencies.
