# Tasks

## 1. Reproduce the leak

- [x] 1.1 In `src/ai/redact.rs` tests, add `exec_form_secret_args_redacted`. It asserts exact `text()` output for each confirmed case:
  - `command: ["redis-server", "--requirepass", "hunter2"]`
  - `test: ["CMD", "redis-cli", "-a", "hunter2", "ping"]`
  - the block list `command:\n  - redis-server\n  - --requirepass\n  - hunter2\n`
  - `command: [redis-server, --requirepass, hunter2]`
  - `["mongod", "--password", "hunter2"]`
  - `["sqlcmd", "-P", "hunter2"]`
  - `["CMD", "mysqladmin", "ping", "-phunter2"]`
  - a three-line flow sequence

  Verify with `cargo test exec_form_secret_args_redacted` that the test fails on the current code.
- [x] 1.2 Add `exec_form_glued_value_keeps_structure`. It asserts that `args: ["--api-key=abc123", "--port", "80"]` becomes `args: ["--api-key=<redacted>", "--port", "80"]` and that `["mysqld", "--password=hunter2"]` keeps its closing `"]`. Verify it fails on the current code.
- [x] 1.3 Add `exec_form_non_secret_args_kept`. It asserts that these are unchanged:
  - `["redis-server", "--port", "6380", "--appendonly", "yes"]`
  - `["mysql", "-p", "appdb"]` (D4; a bare `-p` element prompts. Since 6.34, `-p` with a value in the same element, such as `"-p appdb"`, is a glued password.)
  - `["x", "--no-auth", "--port", "1"]`
  - a block list of package names

  Verify it passes on the current code, as a guard against over-redaction.
- [x] 1.4 In `tests/cli.rs`, add `init_show_context_redacts_exec_form_compose`. It writes a `compose.yaml` with an exec-form redis `command` and `healthcheck.test`, runs `devy init --show-context`, and asserts that the context does not contain `hunter2` and does contain `"--requirepass", "<redacted>"` (spec scenario "Exec-form compose command"). Verify it fails on the current code.

## 2. Shared argument walker

- [x] 2.1 Implement the walker (D1). It takes element spans, keeps the tool context (`redis-cli`, `sqlcmd`, `login`/`sshpass`/`mongo*`, MySQL/MariaDB clients) and handles three kinds of element:
  - secret-named flags, where it redacts the next element unless that element starts with `-`;
  - tool-scoped short flags;
  - glued values (`--<secret>=`, `-pX`).

  Verify with direct unit tests on span lists, including basename matching of `/usr/local/bin/redis-cli`.

## 3. Flow sequences

- [x] 3.1 Implement the flow front end and register `exec-form-arg` in `RULES` after `curl-user` and before `xml-element`/`key-assignment` (D2):
  - it starts at a `[` in a value position;
  - it handles quoted and plain elements;
  - it crosses lines and stops at a blank line;
  - it skips sequences with nested collections. (Superseded by 6.10 and 6.17: nested collections are scanned, and each sequence is a command of its own. The rule's position is superseded by 6.18.)

  Verify that the flow cases from 1.1 pass, along with 1.3, and that `cargo test` in `src/ai/redact.rs` stays green.
- [x] 3.2 Make `key-assignment` stop its value scan at the closing quote of a quoted flow element (D3). Verify that 1.2 passes, that `["PASSWORD=x", "y"]` keeps `, "y"]`, and that the existing JSON and `.npmrc` tests still pass.
- [x] 3.3 Extend the linear-time test with a 100 KiB single-line flow sequence of alternating `"--password", "x"` elements. Verify that it completes within the existing time bound.

## 4. Block sequences

- [x] 4.1 Implement the block front end (D1, D5):
  - it groups runs of `- <scalar>` lines at one indent, skipping comments and blanks;
  - it excludes mapping items and `|`/`>` headers;
  - it strips quotes from each item;
  - it rewrites a redacted item as `<lead>- <redacted>`, keeping the original quotes when present.

  Verify that the block-list cases from 1.1 and 1.4 pass, along with a quoted block list (`- "--password"` / `- "hunter2"`), and that the `nested_values` tests stay green.

## 5. Spec and docs

- [x] 5.1 Add exec-form command arguments to the "What is redacted" list in `README.md` (AI features). Verify that the README example output matches what `devy init --show-context` prints for the 1.4 fixture.
- [x] 5.2 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`, and verify that all pass. Then run the `code-reviewer` and `security-analyst` reviews on the diff, as the project rules require.

## 6. Review findings and approved scope

- [x] 6.1 Patch the MySQL, sqlcmd and curl line rules so that in a plain flow sequence (tool name followed by `,`) a value starting with `,`/`]` is left to `exec-form-arg` and a trailing `,`/`]` stays outside the redaction (D2). Verify `[sqlcmd, -P, pw]`, `[mysql, -ppw]`, `[mysql, -p, appdb]`, `[sqlcmd, -P]`, and that `mysql -pab,cd` is still fully redacted (`line_rules_leave_plain_flow_values_to_exec_form`). (Approach superseded by 6.18; the listed results still hold.)
- [x] 6.2 Handle comments inside flow sequences: `#` at line start or after whitespace runs to the line end, a comment line is not blank, and a plain element ends at ` #` (`exec_form_flow_comments`).
- [x] 6.3 Make a D3-bounded value the rest of its element, and apply the bound only when the closing quote is followed by optional whitespace and `,`, `]`, `}`, `#` or the line end, with a non-empty value; otherwise fall back to the unbounded scan (`key_assignment_value_is_the_rest_of_its_element`, `key_assignment_unclosed_element_falls_back`).
- [x] 6.4 Match the full MySQL/MariaDB family (`mysql*`, `mariadb`, `mariadb-*`) in the walker (`exec_form_mysql_family_glued_password`).
- [x] 6.5 Treat a spaced value as absent when it is a separator after a closed quoted element, or a `#` comment (`spaced_key_keeps_separator_and_comment`).
- [x] 6.6 Align the D1 wording and the `is_secret_flag` doc: the key rule sees the whole flag, dashes included (`-Dpw`).
- [x] 6.7 Redact a dash-prefixed value after a secret-named flag, unless it is a long flag or `--`; a short `-x` counts as the value (D1) (`exec_form_dash_prefixed_secret`).
- [x] 6.8 Accept YAML anchors/tags before a flow `[` and before a block item's scalar, and a tab after a block item's `-` (`exec_form_properties_and_tabs`).
- [x] 6.9 Redact block-scalar, multi-line quoted and plain-continuation block items as whole values (`exec_form_multi_line_block_values`, `exec_form_block_run_boundaries`).
- [x] 6.10 Treat each innermost flat sequence of a nested flow sequence as its own command (`exec_form_nested_sequences`).
- [x] 6.11 Redact the password of `user:password` after `curl -u`/`--user`, and in glued `--user=`/`-u` elements (`exec_form_curl_user`).
- [x] 6.12 Allow Dockerfile `--flag[=value]` options between the instruction and its `[` (`exec_form_dockerfile_flags`).
- [x] 6.13 Add coverage for CRLF, non-ASCII, flow element shapes, end-to-end tool passwords and adversarial input sizes (`exec_form_line_endings_and_non_ascii`, `exec_form_flow_element_shapes`, `exec_form_tool_passwords_end_to_end`, `exec_form_adversarial_inputs_stay_fast`).
- [x] 6.14 Require a `[`/`,` element opener before the tool name (as well as a `,` after it) for the plain-flow handling of the MySQL, sqlcmd and curl line rules, so command lines keep HEAD's whole-word values (`line_rules_need_a_flow_element_for_flow_handling`). (Superseded by 6.18.)
- [x] 6.15 Apply the closed-element absent-value rule only when the key's quote opens a flow or block element (`closed_element_rule_needs_an_element`). (Superseded by 6.19.)
- [x] 6.16 Allow up to 8 Dockerfile flags of up to 4 KiB each, and match instruction words in any case (`exec_form_dockerfile_flags`, `exec_form_adversarial_inputs_stay_fast`).
- [x] 6.17 Scan nested flow collections with a stack: each inner sequence is its own command, and the outer sequence continues after a nested sequence or mapping, which is one opaque element (`exec_form_nested_sequences`, `exec_form_adversarial_inputs_stay_fast`).
- [x] 6.18 Move `exec-form-arg` to just before `mysql-password-flag`, return the MySQL, sqlcmd and curl line rules to HEAD's regexes, and leave a match alone only when its value shows nothing but separators around an existing `<redacted>` (D2) (`line_rules_match_head_outside_exec_form`, `exec_form_runs_before_line_rules`, `line_rules_leave_plain_flow_values_to_exec_form`, `rule_order_starts_with_multiline_shapes`). (Rule position and guard superseded by 6.23.)
- [x] 6.19 Treat a spaced value as absent only when a secret flag is followed by `,` and an element that exec-form already redacted; otherwise measure it as on HEAD (D3) (`spaced_separator_is_absent_only_before_a_redacted_element`, `closed_element_rule_needs_an_element`).
- [x] 6.20 Let quoted flow elements continue onto indented lines, giving up an unclosed quote at the end of its line (`exec_form_multi_line_quoted_flow_element`).
- [x] 6.21 Add Dockerfile `SHELL` to the instruction words (`exec_form_dockerfile_flags`).
- [x] 6.22 Cover several sequences on one line (`exec_form_several_sequences_on_a_line`).
- [x] 6.23 Run `exec-form-arg` after the command-line rules again, so they see every tool name, and have the MySQL, sqlcmd and curl rules (`redact_flag_values`) keep only the structure bytes of fully scanned flow sequences visible inside a value (D2) (`line_rules_see_tool_names_exec_form_redacts`, `line_rules_keep_exec_form_separators`, `line_rules_match_head_outside_exec_form`, `rule_order_starts_with_multiline_shapes`).
- [x] 6.24 Write a private marker for redactions while rules run, neutralise its delimiter in the input, turn it into `<redacted>` at the public entry points, and make every "already redacted" check look for the marker; leave a literal `<redacted>` element as written (D2) (`literal_redacted_is_not_trusted`, `output_never_holds_the_internal_marker`, `exec_form_walker_keeps_other_args`).
- [x] 6.25 Accept a quoted flow element spanning lines only when a separator or close follows it; otherwise resume at the end of its first line (`exec_form_multi_line_quote_needs_a_separator_after_it`).
- [x] 6.26 Extend the linear-time test with many command-line matches in one long sequence (`exec_form_adversarial_inputs_stay_fast`).
- [x] 6.27 Record comments inside flow sequences as non-element pseudo-scalars, so they are never structure gaps, and let only structure bytes of a gap stay visible in a command-line value (D2) (`line_rules_redact_in_flow_comments`, `only_structure_bytes_of_a_gap_stay_visible`).
- [x] 6.28 Make `is_secret` compare against the neutralised input, so a `U+E000` alone does not count as a redaction (`is_secret_ignores_the_neutralised_marker_char`).
- [x] 6.29 Redact a `:` inside a command-line value in a flow mapping, and end a plain mapping scalar at `:` only when it is a mapping indicator (D2) (`line_rule_values_in_flow_mappings_hide_colons`, `only_structure_bytes_of_a_gap_stay_visible`).
- [x] 6.30 End a plain `key-assignment` word value before the last key glued inside it, so that key is scanned again with its own value (D3) (`key_glued_to_a_value_is_rescanned`). (Superseded by 6.33: the cut revealed word tails.)
- [x] 6.31 Redact `sqlcmd`'s glued `-PX`, and the value after blanks in an element that also holds a password flag, except a MySQL `-p` (D1) (`exec_form_flag_and_value_in_one_element`). (The MySQL exception is superseded by 6.34.)
- [x] 6.32 Accept a quoted password in `curl -u user:"pw"`, keeping its quotes (D2) (`curl_quoted_password_keeps_its_quotes`).
- [x] 6.33 Redact a plain word value whole again (remove the cut and `last_glued_key`). When a key glued to the word's end (`:`/`=` ending the word, or a secret flag) takes the next word as its value, redact that word too, unless it starts an assignment. Ignore matches at the internal marker (D3) (`key_glued_to_a_value_hides_its_next_word`).
- [x] 6.34 Check glued forms before the blank split: `-p…` after a MySQL/MariaDB client or `sshpass`, `-P…` after `sqlcmd` and `-u…` after `curl` are glued passwords, blanks included; only a bare `-p` element prompts (D1, D4) (`exec_form_glued_password_with_blanks`, `exec_form_flag_and_value_in_one_element`).
- [x] 6.35 Treat a flag element followed only by blanks as a bare flag, and treat the spaced `key-assignment` value after it as absent when the element closes before a separator and an element exec-form already redacted, a `]` or the line end (D1, D3) (`exec_form_flag_with_trailing_blanks`).
- [x] 6.36 Accept a quote right after the user's `:` in `curl -u a:'p w'`, and hide text glued to the closing quote (D2) (`curl_quoted_password_keeps_its_quotes`).
- [x] 6.37 Chain the next-word redaction while each redacted word ends in a glued key, and leave a next word to the key rule only when it is an assignment the rule would take as one (shared `not_an_assignment`) (D3) (`key_glued_values_chain`, `glued_key_next_word_needs_an_accepted_assignment`).
- [x] 6.38 Measure the next word as a `Spaced` value, so quoted values keep their quotes (D3) (`glued_key_next_word_needs_an_accepted_assignment`).
- [x] 6.39 Match curl's closing quote to its opener (with `\"` escapes), keep an empty password, and mark a glued tail up to a key the key rule takes as an assignment (D2) (`curl_password_quotes_tails_and_every_user`, `curl_quoted_password_keeps_its_quotes`).
- [x] 6.40 Redact every `-u`/`--user` of a curl command, bounded by `;`, `&`, `|` and the line end after the first (D2) (`curl_password_quotes_tails_and_every_user`).
- [x] 6.41 Never split an earlier redaction when a curl tail stops before a glued key (D2) (`curl_tail_never_splits_a_marker`).
- [x] 6.42 Find a `-u`/`--user` right at the scan position, so `-u -u c:x` redacts the second argument (D2) (`curl_user_flag_without_argument`). (Superseded by 6.55: single-pass curl scan.)
- [x] 6.43 End an unquoted curl password at `;&|<>()`, end the command at `;&|`, and pass a key glued to the password's end on to the next word (D2, D3) (`curl_unquoted_password_ends_at_shell_metacharacters`, `curl_password_quotes_tails_and_every_user`).
- [x] 6.44 Skip quoted spans when looking for a curl command's end (D2) (`curl_command_bounds_skip_quoted_text`). (Superseded by 6.55: single-pass curl scan.)
- [x] 6.45 Run an unquoted curl password to a blank or quote again (as on HEAD), pass a glued key on through the chain, and end the command at a `;`, `&` or `|` inside the redacted word (D2) (`curl_unquoted_password_is_the_whole_word`, `curl_password_quotes_tails_and_every_user`).
- [x] 6.46 Find a curl command's first `-u` with the same quote-aware command scan as the later ones, and resume the search for `curl` where each command ended (D2) (`curl_first_user_flag_uses_the_command_scan`). (Superseded by 6.55: single-pass curl scan.)
- [x] 6.47 Step over an unclosed quote's character only, and do not take `--user-agent` for `--user` (D2) (`curl_first_user_flag_uses_the_command_scan`). (Superseded by 6.55: single-pass curl scan.)
- [x] 6.48 Leave a `key-assignment` word value that is itself an accepted assignment to the key loop, with a bounded check (D3) (`value_that_is_an_assignment_is_left_to_the_key_rule`, `long_minified_line_with_many_matches_is_redacted`). (Superseded by 6.49: it left the value visible.)
- [x] 6.49 Redact a `key-assignment` word value whole again, and when the word is itself a flag or `:`/`=` key (at offset 0) whose value comes after a blank, redact that next word too. Stop the chain at a `#` comment, and treat a next word whose separator runs past the bound as a value (D3) (`value_word_is_redacted_whole_and_passes_on_its_key`, `long_minified_line_with_many_matches_is_redacted`).
- [x] 6.50 Make the curl command scan linear: once a quote is found unclosed on a line, later quotes of its kind there are lone characters (D2) (`curl_scan_is_linear_and_escape_aware`). (Superseded by 6.55: single-pass curl scan.)
- [x] 6.51 Take a quote after an unescaped `\` as literal in the curl command scan (D2) (`curl_scan_is_linear_and_escape_aware`). (Superseded by 6.55: single-pass curl scan.)
- [x] 6.52 Drop the marker guard in `ends_in_glued_key`, so a key right after an earlier redaction passes its value on; no marker is split (D3) (`glued_key_after_an_earlier_redaction`, `output_never_holds_the_internal_marker`).
- [x] 6.53 Resume the search for `curl` at a `;`, `&` or `|` inside a redacted password word (D2) (`curl_glued_after_a_password`). (Superseded by 6.55: single-pass curl scan.)
- [x] 6.54 Redact both a key-like next word whose separator runs past the bound and the word after the separator (D3) (`long_separator_hides_both_words`, `value_word_is_redacted_whole_and_passes_on_its_key`).
- [x] 6.55 Replace the curl command search with one linear pass that keeps the shell quote state only for command ends, starts a curl command at any `curl` word, and parses each `-u`/`--user` argument at a word start once, never rescanning (D2) (`curl_single_pass_finds_every_flag`, `curl_single_pass_is_linear`, `curl_scan_is_linear_and_escape_aware`, `curl_glued_after_a_password`, `curl_first_user_flag_uses_the_command_scan`, `curl_user_flag_without_argument`, `curl_command_bounds_skip_quoted_text`).
- [x] 6.56 Stop a shell word's tail at `|` as well (`shell_word_tail`). (Superseded by 6.58: `|` ends only a command-line word.)
- [x] 6.57 Cover a quoted curl password's whole shell word (glued tail included) as examined, so it is never searched for flags again (D2) (`curl_single_pass_is_linear`).
- [x] 6.58 Restore `shell_word_tail`'s stop set (a `|` glued to a quoted value is part of it), and stop curl's tail at `|` with its own helper (D2) (`pipe_after_a_quoted_value`).
- [x] 6.59 Continue a curl command across a line break after an unescaped `\`, LF or CRLF (D2) (`curl_continuation_lines`).
- [x] 6.60 Pass the run's `spaced` setting to the curl rule, so a password's tail stops only at a key the same run's key rule redacts (D2) (`curl_tail_follows_the_runs_key_rule`).

### Deferred (user-approved)

- The query-parameter rule swallows a key glued to its value: `x ?token=a;password: b` leaks `b`, as on main.
- A no-break space (NBSP) is not taken as a blank before a value, as on main.
