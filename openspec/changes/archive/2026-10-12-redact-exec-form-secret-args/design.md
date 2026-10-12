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
- Changing how space-separated command lines are handled. (The line rules change only for plain flow sequences; see D2.)

## Decisions

### D1. One new rule, `exec-form-arg`, placed after the command-line flag rules and before `key-assignment`
The rule has two front ends. Both produce a list of element spans (byte range of the element body, without quotes) and pass it to one shared walker:
- **Flow front end:** finds each `[` that opens a sequence. A `[` qualifies:
  - at line start, or after `:`, `=` or `- `;
  - inside another sequence (after `[` or `,`);
  - after a Dockerfile `CMD`/`ENTRYPOINT`/`RUN`/`HEALTHCHECK`/`SHELL` word (in any case, as a whole word), optionally followed by up to 8 `--flag[=value]` options of up to 4 KiB each (`RUN --mount=type=secret,id=x [...]`).

  YAML node properties (`&anchor`, `!!seq`) may sit before the `[`. Every look-back is bounded: at most two properties of up to 256 bytes, or at most 8 Dockerfile flags of up to 4 KiB. The Dockerfile look-back stops at the first word that is neither a flag nor an instruction, so it never passes another spaced `[`, and the look-backs of different `[`s cover disjoint text. The scan stays linear. The front end splits the sequence into elements with the same quote handling as `flow_collection`. A quoted element may continue onto indented lines, as in YAML and like a block item, and is then one element (`["--token", "abc\n  def"]` → `["--token", "<redacted>"]`). A quote spanning lines counts only when a separator or close (`,`, `]`, `}`, or `:` in a mapping) follows it. Otherwise, as for an unclosed quote, the sequence is given up at the end of the quote's first line, so sequences on later lines are still scanned. It crosses newlines and stops at a blank line or an unbalanced end, as `flow_collection` does. A `#` at line start or after whitespace starts a comment that runs to the end of the line. A comment line is not blank, and a plain element ends at ` #`. Nested collections are scanned with an explicit stack, so deep nesting stays linear:
  - each `[ … ]` is one command with its own tool context (`[["redis-cli", "-a", "x"]]`);
  - in its parent, a nested sequence or `{ … }` mapping is one opaque element that is never redacted and takes the value slot of a flag before it;
  - the parent's remaining elements continue the parent's command with its tool context (`[x, [y], --password, z]`, `[x, {a: b}, --password, z]`);
  - a sequence closed before the scan is given up keeps its redactions.
- **Block front end:** groups maximal runs of lines at one indent whose trimmed text is `- <scalar>` (or `-<tab><scalar>`), where the scalar is not a `key: value` mapping, an alias or a nested collection. Leading node properties (`&anchor `, `!tag `) are skipped. Each item's scalar is one element:
  - a quoted item: its body without the quotes, which may continue onto indented lines;
  - a `|`/`>` block scalar: its indented body lines (the header stays, and a redacted body becomes one `<redacted>` line);
  - a plain item: the rest of the line up to a ` #` comment, plus any deeper continuation lines that are not comments or `- ` items.

  Comment lines and blank lines inside a run are skipped, and do not end the run.

The walker keeps a per-sequence tool context and redacts element *i+1* when element *i* is:
- a flag (`-x` / `--xxx`, with no `=`) that matches the key rule (the same predicate `key_assignments` uses, so `--requirepass`, `--password`, `--token`, `--api-key` and `--masterauth` all match). The rule sees the whole flag, dashes included, because `-Dpw` is a secret only through its `-D` prefix;
- `-a` or `--pass`, once an element whose basename is `redis-cli` has appeared;
- `-P`, once a `sqlcmd` element has appeared;
- `-p`, once a `login` element (preceded by a tool element), `sshpass`, or one of the `mongo*` tools has appeared;
- `-u` or `--user`, once a `curl` element has appeared. Here only the part after the first `:` of the next element is redacted (`admin:<redacted>`), and an element with no `:` is kept, as `curl_users` does.

Which next elements count as values:
- **After a secret-named flag:** the next element is the value even when it starts with `-`, since a password may start with a dash. The exceptions are a long flag (`--name`) and exactly `--`, so `--no-auth` followed by `--port` keeps both. A short `-x` counts as the value: it is as likely a dash-prefixed secret as a flag, and the rule errs toward privacy.
- **After a tool's password flag (`-a`, `-P`, `-p`, `-u`):** the next element is never redacted when it starts with `-`.

A glued element is redacted inside its own span:
- `--<secret>=value` keeps `--<secret>=`;
- `-pX` is redacted after a MySQL/MariaDB client (any `mysql*`, `mariadb` or `mariadb-*` basename) or `sshpass`, and `-PX` after `sqlcmd`;
- curl's `--user=user:X` and `-uuser:X` keep `user:`.

An element that holds a flag word, blanks and then a value (`"-P pw"`, `"-a pw"`, `"--requirepass pw"`, `"-u admin:pw"`) has its value redacted within the element whenever the flag on its own would take the next element as a password, a secret or a curl user. The glued forms are checked first: after a MySQL/MariaDB client or `sshpass`, an element `-p…` (and after `sqlcmd`, `-P…`; after `curl`, `-u…` past its first `:`) is a glued password, blanks included (D4). A flag element followed only by blanks (`"--password "`, `"-P "`) is a bare flag and takes the next element.

*Alternative: extend each regex to accept `"\s*,\s*"` and `\n\s*-\s+` as separators.* Rejected:
- The regexes would grow a second alternation each, and the tool context (`redis-cli` earlier in the same array) cannot be expressed without the `[^\n]*?` scan crossing lines.
- Plain flow elements (`[redis-server, --requirepass, x]`) and multi-line arrays would still be missed.

*Alternative: limit the rule to keys named `command`, `entrypoint`, `test` or `args`.* Rejected:
- Kubernetes `args:`, Dockerfile `CMD [...]`, JSON `"args": [...]` and CI `run:` lists use other keys.
- The trigger is already narrow: a flag-shaped element whose name names a secret.

### D2. Ordering relative to existing rules, and the internal marker
**Order.** The rule runs after the command-line flag rules (`mysql-password-flag` through `curl-user`) and before `xml-element` and `key-assignment`:
- **Before `key-assignment`:** the glued `--password=x` element is redacted with correct boundaries first.
- **After the command-line rules:** they still see every tool name. If `exec-form-arg` ran first, it could redact a tool name that is itself a value (`[--password, mysql] -phunter2`, `x: [--token=mysql] -phunter2`), hiding it from `mysql_password_flags` and leaking the `-p` value.
- **`CMD-SHELL` strings:** a string element such as `"redis-cli -a x ping"` is one element to `exec-form-arg`, so the command-line rules redact inside it.

**Plain flow sequences.** A command-line rule would otherwise treat a plain flow sequence as a command line. Its value class `[^\s'"]+` accepts `,` and `]`, so `[sqlcmd, -P, pw]` became `[sqlcmd, -P<redacted> pw]`, leaking `pw`. So `mysql_password_flags`, `sqlcmd_passwords` and the unquoted `curl_users` pattern share `redact_flag_values`:
- It scans the text's flow sequences the same way `exec-form-arg` does, once per rule and only when the rule matches.
- It notes the structure of every fully scanned sequence: the bytes of its span outside its scalars and comments, such as brackets, separators, quotes and blanks. A comment inside a sequence is never structure, so a command line in it is redacted as on HEAD (`[x, # sqlcmd -S db -P hunter2` → `-P <redacted>`).
- Each value is redacted except for those structure bytes:
  - `[mysql, -ppw]` → `[mysql, -p<redacted>]`;
  - `[mysql, -ppw]xyz` → `[mysql, -p<redacted>]<redacted>`;
  - `[sqlcmd, -P ,pw]` → `[sqlcmd, -P ,<redacted>]`.
- A value that is only structure (`[sqlcmd, -P, pw]`, whose "value" is the `,`; `[mysql, -p, appdb]` (D4); `[sqlcmd, -P]`) is left alone, and `exec-form-arg` then redacts the next element where the tool takes one.
- Outside scanned sequences, and in sequences the scan gave up on, the value is redacted whole, `,` and `]` included, exactly as on HEAD (`mysql -pab,cd`, `a, sqlcmd, -P ,hunter2`, `[x], curl, -u a:]hunter2`).
- As a second line of defence, only bytes that are themselves structure (`[ ] { } , " '`, blanks, CR, LF) may stay visible inside a value. Any other byte is redacted, even inside a gap. A mapping's `:` is not among them, since a password may hold one: `[{a: mysql -pab:cd}]` → `[{a: mysql -p<redacted>}]`.
- In a flow mapping, a `:` ends a plain scalar only as a mapping indicator, that is, when a blank, the line end or one of `,[]{}` follows it, as in YAML. So `ab:cd` is one scalar.
- `curl_users` handles every `-u`/`--user` of a curl command (curl uses the last) in one left-to-right pass over the text:
  - **Quote state:** the pass keeps the shell's quote state (single and double quotes, `\` escapes outside single quotes, an unclosed quote lasting to the line end). It uses this only to tell where a command ends: at an unquoted `;`, `&`, `|` or the line end. A quoted `;` (`-d "x;y"`) does not end it, and neither does a line break after an unescaped `\` (`\` + LF or CRLF), which continues the command on the next line.
  - **Commands:** a `curl` word anywhere starts a curl command, even inside a quoted span (`Use curl "for example: curl -u admin:x"`). This can only hide more.
  - **Flags:** inside a curl command, every `-u`/`--user` at a word start (after a blank, `=`, a quote or the line start) is a flag, however it is quoted. `--user-agent` and the like are not.
  - **Arguments:** each flag's argument is parsed once. Flags are not looked for again in the text it covered, but the pass still walks that text for quotes, command ends and `curl` words, so `a:pw|curl -u b:x` finds the second command, while `a:b;echo -u c:d` leaves `echo`'s argument alone. An argument that is itself a flag at a word start (`-u -u c:x`, `-u=-u=…`) is left for the pass to reach, so each flag costs constant work.
  - **Key rule mode:** a key that ends a password's hidden tail, or takes the next word as its value, must be one the key rule accepts with the run's own `spaced` setting (`false` for `credential_parts`, `credential_parts_inline` and `value`), since that rule redacts what is left.
  - **Linear:** each byte is visited by the pass once and by at most one argument parse, so the work is linear. A quoted password's argument covers the whole shell word it ends, glued tail included, even when a key in the tail ends the hidden part early (that key's value is left to later rules).

  For each `user:password` argument:
  - A quote before the user (`'bob:pa ss'`) or right after its `:` (`bob:"p w"`) bounds the password up to the matching closing quote, with `\"` escaping a `"`. The quotes are kept, and an empty password (`"bob:"`) is kept as is.
  - Text glued to the closing quote is part of the same shell word, which on a command line also ends at `|`, so it is redacted too (`bob:"<redacted>"<redacted>`, while `"a:pw"|curl` keeps `|curl`). Elsewhere, as on HEAD, a `|` glued to a quoted value is part of the value (`TOKEN="ab"|x` → `TOKEN="<redacted>"`). The hidden tail stops before a key that the key rule takes as an assignment of its own (`"pw"--password x`), which that rule then redacts. The tail never ends inside an earlier rule's redaction, so the internal marker is never split.
  - An unquoted password runs to a blank or quote, as on HEAD, so the whole word is hidden (`Sup3r(Secret)zq`, `pa&zq`), and it keeps the structure of a scanned flow sequence. A key glued to its end takes the next word as its value, as in D3 (`c:Secret;--password x` → `c:<redacted> <redacted>`, `c:Secret,api_key: x` → `c:<redacted> <redacted>`). A `;`, `&` or `|` inside the redacted text ends the command, and stays hidden with it, as on HEAD (`a:b; echo -u c:d` → `a:<redacted> echo -u c:d`).

**Internal marker.** While `RULES` run, rules write a private marker (`\u{E000}redacted\u{E000}`) in place of a secret. The public entry points (`text`, `credential_parts`, `credential_parts_inline`, and `value` through them) turn the marker into `<redacted>` once, at the end. Before the rules run, any `U+E000` in the input is replaced by `U+FFFD`, so only a rule can write the marker. `is_secret` compares against this neutralised input, so the replacement alone never counts as a redaction. Every check for "already redacted" looks for the marker:
- the element guard in `exec-form-arg`;
- the D3 next-element guard;
- the mid-line "earlier redaction kept" rule in `key-assignment`;
- the URL-password check.

A literal `<redacted>` in the input is therefore never taken for a rule's redaction. At worst it is redacted again, and it reads the same. `exec-form-arg` leaves a literal `<redacted>` element as written rather than turning it into the marker, so the D3 guard cannot trust it either.

### D3. `key-assignment` respects quoted-element boundaries
When the key is preceded by an opening quote that is itself the start of a flow element (after `[`, `,` or `- `), the value is the rest of the element, up to the matching closing quote (`["DB_PASSWORD=correct horse battery", "X=1"]` → `["DB_PASSWORD=<redacted>", "X=1"]`). This fixes the mangled output in cases D1 does not reach, such as a key that is not flag-shaped (`["PASSWORD=x", "y"]`), and keeps D1's output stable. JSON objects (`"key": "value"`) are unaffected because the quote closes before the separator.

The bound applies only when that quote really closes the element: it is on the same line and is followed by optional whitespace and then `,`, `]`, `}`, `#` or the line end, and the value before it is not empty. Otherwise the value is measured as before (unbounded), so inputs like `- "TOKEN="abc"`, `- "TOKEN=ab"cd"` or `- "TOKEN='''abc` followed by more lines are never less redacted than without the bound.

A plain word value (the `Token` and `Spaced` forms) is always redacted whole, as on HEAD, and scanning resumes after it. Nothing inside the word is shown again. Such a word can end in a key glued to it whose own value is the next word: `x token=abc;password: zq1 y`. Then that next word is redacted too (`x token=<redacted> <redacted> y`). The key qualifies when all of these hold:
- its match starts inside the word, not at the internal marker;
- either its `:`/`=` separator ends the word, or it is a secret flag (`#--requirepass`) ending the word, followed by a blank.

The next word is measured as a value of the `Spaced` form, so a quoted value keeps its quotes (`password: "hunter 2"` → `"<redacted>"`). When the redacted word itself ends in such a key, the redaction passes on to the word after it (`password: b;secret: x`), and so on. A next word is left to the key rule when it is an assignment the rule would take as one: a key match at the word's first key character (after leading punctuation, as in `;--password`) that the rule's own checks accept. Otherwise it is redacted (`password: secretsauce more`). Each search sees only a word, or the next word, plus one character, so the scan stays linear.

A `Token` or `Spaced` value word is always redacted whole, even when it is itself a key (`password: --token x`). A key covering the whole word counts as glued to it, at offset 0, so when its own value comes after a blank, that next word is redacted too: `password: --token zq` → `password: <redacted> <redacted>`. The same holds for `password: api_key: zq`. A word whose value is inside it (`api_key=abc`, `TOKEN:abc`) is hidden as it is. A bare word that only names a secret (`SECRET_KEY_123`, `PASSWORD`) is not a flag or `:`/`=` key, so the word after it stays (`--password <redacted> --port 5432`). The chain never takes a `#` comment as a value. When checking whether a next word is an assignment, the check looks at a bounded separator. With a longer run of blanks it cannot tell, so it redacts both that word and the word after the separator (`password: KEY<70 blanks>value` → `<redacted><70 blanks><redacted>`), stopping at a comment. A key is found even right after an earlier rule's redaction (`#eyJa.b.c'password: x`). Since a word is never cut, no internal marker can be split.

Two related boundary rules apply to the spaced `key value` form:
- a value is absent when the key is a secret flag (it starts with `-` and passes the key rule), the value starts with `,`, and the element after that `,` is already a whole redaction written by `exec-form-arg` (the internal marker of D2, never a literal `<redacted>` from the input). So `["--password" , "x"]` → `["--password" , "<redacted>"]` and `["--password" ,hunter2]` → `["--password" ,<redacted>]` keep their separator. Every other spaced value (`"password" ,hunter2`, `- "--password" ,hunter2`, `["--password" ]hunter2`) is measured as on HEAD;
- a `#` followed by whitespace or the line end (`--password # c`) is a comment, not a value.

### D4. A bare MySQL `-p` element prompts
For the MySQL/MariaDB clients, a separate `-p` prompts for the password and the next argument is the database name, so `["mysql", "-p", "appdb"]` is kept. The existing line rule (`mysql_password_flags`) likewise redacts only the glued form. In exec form an element is one argument (one argv entry), so `-p<anything>` within one element, blanks included, is a glued password: `["mysql", "-p appdb"]` → `["mysql", "-p<redacted>"]`. Only a bare `-p` element prompts. The mongo/login/sshpass `-p` form takes the next argument, which matches `command_password_flags`.

### D5. Block sequences need not sit under a known key
`nested_values` already redacts every item under a secret-named key. The block front end is independent of the parent key and only looks at the items, so `command:`, `args:` and anonymous nested lists are all covered.

## Risks / Trade-offs

- [Over-redaction: `--keyfile /path` or `--cert-dir /x` in exec form hides a path from the model] → This already happens on space-separated lines (`spaced_assignment` treats every `-flag` key as an assignment). The behavior stays consistent and it errs toward privacy.
- [A YAML list of prose bullets in a README (`- --token` followed by `- explanation`)] → The next bullet would be redacted. This is rare and harmless, since it only hides text.
- [Performance on large files] → Both front ends are single linear passes, and the flow front end reuses the blank-line stop from `flow_collection`. A given-up sequence resumes where it stopped, look-backs are bounded, and multi-line block items advance the line cursor past what they consumed. The linear-time tests in `redact.rs` cover a long exec-form line, deep nesting, many openers, Dockerfile flags and tags, block scalars, continuations and comments.
- [Dash-prefixed values after a secret-named flag (`--token -v`)] → The short flag is hidden. This errs toward privacy, while long flags and `--` stay visible.
- [Rule interaction regressions] → The full existing `redact.rs` test suite must stay green. New tests assert exact output strings, not only the absence of the secret, so structural damage is caught.

## Migration Plan

None. Redaction only affects what is sent to the model and printed by `--show-context`. Roll back by reverting the rule.

## Open Questions

- Should `devy init` also send `Dockerfile` (where exec form is common)? This is independent of this change, which already covers that syntax.
