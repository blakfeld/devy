# Design

## Context

See proposal.md (Why). The current flow in `src/commands/doctor.rs`:
- `user_content` sends `redact::cap_file(redact::text(devy_yml))`.
- `run` takes `diagnosis.proposed_config()`, normalizes CRLF and runs `has_hidden_text`. It then validates with `validate_with_backend` and `yaml_safe::from_str_strict`, applies `match_line_endings`, prints `unified_diff`, computes `config_diff::diff`, and calls `offer_fix`.
- No step compares the proposal with what redaction changed. `redact::REDACTED` is referenced in doctor.rs only in tests.
- `redact::text` is deterministic and mostly line-preserving. `nested_values`, `key_assignments` and the other rules rewrite within a line. `pem_blocks` and `block_scalars` collapse a multi-line body into a single `<redacted>` line.
- `doctor.rs` already has a line-level LCS (`diff_ops`, `Op::{Same, Removed, Added}`) used by `unified_diff`.

## Goals / Non-Goals

**Goals:**
- A doctor fix never writes `<redacted>` where the file had real content, in any mode.
- An ordinary fix that copies redacted lines through still applies, with the original values kept and those lines absent from the diff.

**Non-Goals:**
- Changing what is redacted, or sending secrets to claude.
- AI `init`, which drafts a new file rather than editing one. Any placeholder it writes is visible in the draft the user reviews.
- The 8 KiB `cap_file` truncation of a large `devy.yml` (see Open Questions).

## Decisions

### D1. Restore first, then refuse what could not be restored
Add `restore_redacted(original: &str, proposed: &str) -> String` in `doctor.rs`:
1. Recompute `sent = redact::text(original)`, using the same CRLF-normalized `original` that `proposed` is compared with.
2. Run `diff_ops(original_lines, sent_lines)`. Each run of non-`Same` ops is a *redaction span*: sent lines `[s0, s1)` that replaced original lines `[o0, o1)`. This covers the multi-line PEM and block-scalar collapses.
3. Run `diff_ops(sent_lines, proposed_lines)`. A span is restorable when every sent line in it maps `Same` to consecutive proposal lines. Those proposal lines are then replaced by the original lines `[o0, o1)`. Any other span is left as it is.

Then a guard runs: `count(REDACTED, restored) > count(REDACTED, original)` discards the proposal with the warning in the spec. The guard compares counts rather than checking for any occurrence, so a file that already contains the literal text is not refused.

- **Alternative: refuse every proposal containing an unrestored placeholder, with no restoration.** It is simpler, but nearly every fix to a project with a secret would be refused, because the model is required to return the whole file. This defeats the feature for exactly the projects (services with credentials) where doctor is most useful.
- **Alternative: restore by key path on the parsed YAML.** It is robust to reordering, but it loses comments and formatting. devy writes the proposal text verbatim and promises to keep comments. It also cannot restore redacted comments.
- **Alternative: send placeholders with unique IDs (`<redacted:3>`).** This gives exact mapping even after reordering, but it changes the redaction format shared with every AI command and `--show-context`. It is worth revisiting if line matching proves too strict in practice.

### D2. Order in `run`
CRLF normalize, `has_hidden_text` on the model's text, `restore_redacted`, placeholder guard, validation, `match_line_endings`, diff, exec diff, offer.
- The hidden-text check stays on the model's own output, so that original content (already accepted by the user) does not trip it.
- Validation and `config_diff` see real values. An `environment` value restored unchanged therefore does not count as an executable change, and validation errors are about the real file.

### D3. Prompt rule
Append to `REPLY_RULES`: "Copy every line containing `<redacted>` exactly as given, including indentation; never edit, move or invent such lines." This makes restoration succeed more often. The guard, not the prompt, is the safety property.

### D4. Warning text
The guard reuses the existing discard path and prefix: `suggested devy.yml change was invalid and was not offered: it would write the "<redacted>" placeholder over values hidden from claude`. No new exit code applies, and doctor still exits 0.

## Risks / Trade-offs

- [Two identical redacted lines (same key and indent) in different sections, and the model reorders the sections] → LCS anchors on surrounding non-redacted lines. A moved block's lines come out as `Added`, so they are not restored, and the guard refuses the proposal. A swap can still be mapped wrongly only if the context lines are identical too, which in that case also makes the content identical. A unit test covers the reorder case.
- [A model that rewrites a redacted line (for example, re-quoting it) gets refused even when the fix was good] → This is acceptable. The warning says why, and the user can apply the fix by hand. D3 reduces how often it happens.
- [The fix depends on `redact::text` being deterministic] → It is: a pure function of its input. A unit test asserts that `restore_redacted(x, redact::text(x)) == x` for a fixture covering every rule type.

## Open Questions

- A `devy.yml` larger than `FILE_CAP` (8 KiB) is sent truncated, so a whole-file proposal can silently drop the tail. This is a related data-loss risk, but it is out of scope here. It should be tracked as a separate finding, either by refusing proposals when the sent `devy.yml` was truncated or by not asking for `devy_yml` in that case.
