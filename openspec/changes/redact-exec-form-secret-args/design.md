# Design

## Context

See proposal.md (Why). `redact::text` (`src/ai/redact.rs`) folds a fixed list of `RULES` over the whole text. Each command-flag rule is a regex over one shell-style line:
- `redis_cli_passwords` (~L660)
- `mysql_password_flags`
- `command_password_flags`
- `sqlcmd_passwords`

They require `[ \t]` between the flag and its value, so a `", "` separator between them, or a newline plus `- `, never matches. `key_assignments_with` (~L720) only treats `--flag value` as an assignment when whitespace alone separates the two (`spaced_assignment`), so it misses `"--requirepass", "x"` too. Its `--password=x` path does match inside an element. But the value scanner (`redact_plain_value` / `shell_word_tail`) does not know it is inside a quoted element, so it consumes the closing `"` and the `, ` or `]` that follow.

Structures that already exist and can be reused:
- `quoted_len` and `flow_collection`, which scan quoted and plain flow elements without being fooled by `]` inside quotes;
- `nested_values`, the line-aware model for indented YAML blocks;
- `names_a_secret` and `secret_key_pattern`, the key rule.

## Goals / Non-Goals

**Goals:**
- Redact the secret argument in every confirmed form: flow sequences (quoted, plain, multi-line), block sequences, and healthcheck `CMD` arrays.
- Produce output that is still a well-formed sequence: the same number of elements, with quotes and separators kept.
- Use one argument model for both sequence syntaxes, so a new tool-scoped flag is added in one place.

**Non-Goals:**
- Parsing YAML or JSON properly. The redactor stays a line- and pattern-based text pass, since it also runs on logs and READMEs.
- Adding new files to the `devy init` file list (e.g. `Dockerfile`). The new rule is generic and covers them if they are added later.
- Changing how space-separated command lines are handled.

## Decisions

### D1. One new rule, `exec-form-arg`, placed after the line-based command rules and before `key-assignment`
The rule has two front ends. Both produce a list of element spans (byte range of the element body, without quotes) and pass it to one shared walker:
- **Flow front end:** finds each `[` that opens a sequence. A `[` qualifies after `:`, `=`, `- `, at line start, or after a Dockerfile `CMD`/`ENTRYPOINT`/`RUN`/`HEALTHCHECK` word. The front end then splits it into elements with the same quote handling as `flow_collection`. It crosses newlines and stops at a blank line or an unbalanced end, as `flow_collection` does. A sequence with a nested `[`/`{` element is skipped, because exec form is a flat list of strings.
- **Block front end:** groups maximal runs of lines at one indent whose trimmed text is `- <scalar>`, where the scalar is not a `key: value` mapping, a `|`/`>` header or a nested collection. Each item's scalar, with surrounding quotes removed, is one element. Comment lines and blank lines inside a run are skipped, and do not end the run.

The walker keeps a per-sequence tool context and redacts element *i+1* when element *i* is:
- a flag (`-x` / `--xxx`, with no `=`) whose name, with its leading dashes removed, matches the key rule (the same predicate `key_assignments` uses, so `--requirepass`, `--password`, `--token`, `--api-key` and `--masterauth` all match);
- `-a` or `--pass`, once an element whose basename is `redis-cli` has appeared;
- `-P`, once a `sqlcmd` element has appeared;
- `-p`, once a `login` element (preceded by a tool element), `sshpass`, or one of the `mongo*` tools has appeared.

The walker never redacts a next element that itself starts with `-`, so a boolean flag such as `--no-auth` followed by `--port` keeps both. A glued element is redacted inside its own span: `--<secret>=value` keeps `--<secret>=`, and `-pX` is redacted after a MySQL/MariaDB client or `sshpass`.

*Alternative: extend each regex to accept `"\s*,\s*"` and `\n\s*-\s+` as separators.* Rejected:
- The regexes would grow a second alternation each, and the tool context (`redis-cli` earlier in the same array) cannot be expressed without the `[^\n]*?` scan crossing lines.
- Plain flow elements (`[redis-server, --requirepass, x]`) and multi-line arrays would still be missed.

*Alternative: limit the rule to keys named `command`, `entrypoint`, `test` or `args`.* Rejected:
- Kubernetes `args:`, Dockerfile `CMD [...]`, JSON `"args": [...]` and CI `run:` lists use other keys.
- The trigger is already narrow: a flag-shaped element whose name names a secret.

### D2. Ordering relative to existing rules
The rule runs after the line-based command rules, so a `CMD-SHELL` string element (`"redis-cli -a x ping"`) is still handled by `redis_cli_passwords`, and an element already reading `<redacted>` is left as is. It runs before `key-assignment`, so the glued `--password=x` element is redacted with correct boundaries first.

### D3. `key-assignment` respects quoted-element boundaries
When the key is preceded by an opening quote that is itself the start of a flow element (after `[`, `,` or `- `), the value scan stops at the matching closing quote. This fixes the mangled output in cases D1 does not reach, such as a key that is not flag-shaped (`["PASSWORD=x", "y"]`), and keeps D1's output stable. JSON objects (`"key": "value"`) are unaffected because the quote closes before the separator.

### D4. MySQL `-p value` (spaced) is not redacted
For the MySQL/MariaDB clients, a separate `-p` prompts for the password and the next argument is the database name. The existing line rule (`mysql_password_flags`) redacts only the glued form, and exec form follows it. The mongo/login/sshpass `-p` form takes the next argument, which matches `command_password_flags`.

### D5. Block sequences need not sit under a known key
`nested_values` already redacts every item under a secret-named key. The block front end is independent of the parent key and only looks at the items, so `command:`, `args:` and anonymous nested lists are all covered.

## Risks / Trade-offs

- [Over-redaction: `--keyfile /path` or `--cert-dir /x` in exec form hides a path from the model] → This already happens on space-separated lines (`spaced_assignment` treats every `-flag` key as an assignment). The behavior stays consistent and it errs toward privacy.
- [A YAML list of prose bullets in a README (`- --token` followed by `- explanation`)] → The next bullet would be redacted. This is rare and harmless, since it only hides text.
- [Performance on large files] → Both front ends are single linear passes, and the flow front end reuses the blank-line stop from `flow_collection`. The existing linear-time tests in `redact.rs` are extended with a long exec-form line.
- [Rule interaction regressions] → The full existing `redact.rs` test suite must stay green. New tests assert exact output strings, not only the absence of the secret, so structural damage is caught.

## Migration Plan

None. Redaction only affects what is sent to the model and printed by `--show-context`. Roll back by reverting the rule.

## Open Questions

- Should `devy init` also send `Dockerfile` (where exec form is common)? This is independent of this change, which already covers that syntax.
