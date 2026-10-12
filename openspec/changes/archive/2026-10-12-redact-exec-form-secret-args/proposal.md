# Proposal

## Why

`devy init` always sends compose files (`compose.yaml`, `docker-compose.yml`, …) to the AI provider, after redaction. The redactor's command-flag rules (`redis-cli -a`, `--requirepass`, `--password`, …) only match space-separated command lines, so the same secrets written in compose's **exec form** are sent in the clear. Confirmed against the current `src/ai/redact.rs`:

- `command: ["redis-server", "--requirepass", "hunter2"]` is sent unchanged.
- `test: ["CMD", "redis-cli", "-a", "hunter2", "ping"]` is sent unchanged.
- `command:` followed by block-list items `- redis-server`, `- --requirepass`, `- hunter2` is sent unchanged.
- `command: redis-server --requirepass hunter2` is correctly redacted.

Exec form is the documented and most common way to write compose `command`, `entrypoint` and `healthcheck.test`, so this is a high-severity leak. A related defect makes things worse: `["mysqld", "--password=hunter2"]` is redacted, but the redaction swallows the element's closing `"]`. Likewise, `["--api-key=abc", "--port", "80"]` loses the `",` between elements, so the model receives malformed YAML.

## What Changes

- The redactor treats each element of a command sequence as one argument. This covers:
  - flow sequences (`[...]`, quoted or plain elements, possibly spanning lines, holding comments, or nested, where each sequence is one command and a nested sequence or mapping is one element of its parent);
  - block sequences (consecutive `- item` lines at one indent, including quoted, block-scalar and multi-line items);
  - YAML anchors and tags before the sequence or an item;
  - Dockerfile `--flag` options before an exec-form `[`.
- In such a sequence, devy redacts the element after a secret-valued flag:
  - a long or short flag whose name names a secret (`--requirepass`, `--password`, `--token`, `--api-key`, `--masterauth`, …), even when the value starts with `-` (unless it is a long flag or `--`);
  - `-a` or `--pass` once a `redis-cli` element has appeared;
  - `-P` once a `sqlcmd` element has appeared;
  - `-p` once one of the clients that already take it as the next argument on a command line has appeared (`<tool> login`, `sshpass`, the MongoDB tools);
  - the password part of `user:password` after `curl -u`/`--user`.
- devy redacts a glued value inside one element: `--password=x`, `-pX` after a MySQL/MariaDB client or `sshpass`, `-PX` after `sqlcmd`, and curl's `--user=u:x`/`-uu:x`. Since an element is one argument, a glued value may contain blanks (`"-pmy pass"`); only a bare `-p` element prompts. An element holding a flag, blanks and a value (`"-a pw"`, `"--requirepass pw"`) has the value redacted, and a flag element followed only by blanks (`"--password "`) takes the next element. The element's own closing quote and the separators between elements are kept.
- Existing space-separated command-line redaction (including `CMD-SHELL` strings) is unchanged. The MySQL, sqlcmd and curl line rules leave the separators of exec-form sequences alone, and the new rule, which runs after them, redacts the element values.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `ai-assist`: the "Redaction before sending" requirement gains exec-form command arguments as a credential shape. It also gains scenarios for the flow, block-list and healthcheck forms, and for keeping the sequence's structure intact.

## Impact

- `src/ai/redact.rs`:
  - a new rule in `RULES`, placed after the existing command-flag rules and before `key-assignment`;
  - rules write a private marker for redactions, turned into `<redacted>` at the end, so a literal `<redacted>` in the input is never trusted as a redaction;
  - the `key-assignment` value is the rest of a quoted sequence element when the element's closing quote really ends it. A spaced value is absent when it is a comment, or a `,` after a secret flag whose next element is already redacted;
  - `mysql_password_flags`, `sqlcmd_passwords` and `curl_users` keep HEAD's semantics, except that the structure of a fully scanned flow sequence (brackets, separators, quotes) inside a value stays visible. `curl_users` covers every `-u` of a curl command, and a quoted password (`-u a:"pw"`, `-u a:'p w'`, matched to its own quote), marking text glued to its closing quote;
  - a plain `key-assignment` word value is still redacted whole. When a key glued to its end takes the next word as its value (`token=abc;password: x`), that word is redacted too, and so on along a chain of such words, unless the next word is an assignment the key rule takes as one;
- Unit tests in `src/ai/redact.rs`, plus a `tests/cli.rs` case that runs `devy init --show-context` on a compose file.
- No config, CLI, dependency or output-format changes. The only user-visible effect is more `<redacted>` in AI context and `--show-context` output.
