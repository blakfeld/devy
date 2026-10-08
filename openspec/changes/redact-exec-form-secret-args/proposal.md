# Proposal

## Why

`devy init` always sends compose files (`compose.yaml`, `docker-compose.yml`, …) to the AI provider, after redaction. The redactor's command-flag rules (`redis-cli -a`, `--requirepass`, `--password`, …) only match space-separated command lines, so the same secrets written in compose's **exec form** are sent in the clear. Confirmed against the current `src/ai/redact.rs`:

- `command: ["redis-server", "--requirepass", "hunter2"]` is sent unchanged.
- `test: ["CMD", "redis-cli", "-a", "hunter2", "ping"]` is sent unchanged.
- `command:` followed by block-list items `- redis-server`, `- --requirepass`, `- hunter2` is sent unchanged.
- `command: redis-server --requirepass hunter2` is correctly redacted.

Exec form is the documented and most common way to write compose `command`, `entrypoint` and `healthcheck.test`, so this is a high-severity leak. A related defect makes things worse: `["mysqld", "--password=hunter2"]` is redacted, but the redaction swallows the element's closing `"]`. Likewise, `["--api-key=abc", "--port", "80"]` loses the `",` between elements, so the model receives malformed YAML.

## What Changes

- The redactor treats each element of a command sequence as one argument. This covers flow sequences (`[...]`, quoted or plain elements, possibly spanning lines) and block sequences (consecutive `- item` lines at one indent).
- In such a sequence, devy redacts the element after a secret-valued flag:
  - a long or short flag whose name names a secret (`--requirepass`, `--password`, `--token`, `--api-key`, `--masterauth`, …);
  - `-a` or `--pass` once a `redis-cli` element has appeared;
  - `-P` once a `sqlcmd` element has appeared;
  - `-p` once one of the clients that already take it as the next argument on a command line has appeared (`<tool> login`, `sshpass`, the MongoDB tools).
- devy redacts a glued value inside one element: `--password=x`, `-pX` after a MySQL/MariaDB client, and `-pX` after `sshpass`. The element's own closing quote and the separators between elements are kept.
- Existing space-separated command-line redaction (including `CMD-SHELL` strings) is unchanged.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `ai-assist`: the "Redaction before sending" requirement gains exec-form command arguments as a credential shape. It also gains scenarios for the flow, block-list and healthcheck forms, and for keeping the sequence's structure intact.

## Impact

- `src/ai/redact.rs`:
  - a new rule in `RULES`, placed after the existing command-flag rules and before `key-assignment`;
  - the `key-assignment` value scan stops at the end of a quoted sequence element.
- Unit tests in `src/ai/redact.rs`, plus a `tests/cli.rs` case that runs `devy init --show-context` on a compose file.
- No config, CLI, dependency or output-format changes. The only user-visible effect is more `<redacted>` in AI context and `--show-context` output.
