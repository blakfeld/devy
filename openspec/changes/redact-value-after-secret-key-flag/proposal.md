# Proposal

## Why

When a secret-named key is followed by a flag, the key-assignment rule redacts the flag and sends the real value: `gh secret set API_KEY --body hunter2` becomes `gh secret set API_KEY <redacted> hunter2`. That line is common in Makefiles, READMEs and CI workflows, all of which devy sends to the provider. The same space-separated shape also leaks through the CLI config setters (`aws configure set aws_secret_access_key X`, `npm config set //registry.npmjs.org/:_authToken X`, `yarn config set npmAuthToken X`, `git config github.token X`), whose lower-case or dotted keys the spaced rule does not treat as assignments.

## What Changes

- Key-assignment rule: in the space-separated form (`KEY value`) with a key that is not itself a flag, a candidate value that starts with `-` is no longer the assigned value. If that word is a value-carrying flag (`--body`, `-b`, `--value`, spaced or as `--flag=value`), the word after it (or the part after `=`) is redacted instead; any other flag is left alone. Flag keys keep their current behavior (`--password -x` still redacts `-x`).
- New rule for `gh secret set`: the value of `--body`/`-b` (spaced, `--body=value`, or glued `-bvalue`) is redacted wherever it sits in the command (`gh secret set API_KEY --env prod --body hunter2`), whatever the secret's name, because a `gh secret` body is always a secret.
- New rule for CLI config setters: the value argument of `aws configure set <key> <value>`, `npm|pnpm|yarn config set <key> <value>` and `git config [options] [set] <key> <value>` is redacted when `<key>` names a secret under the existing key rule (`aws_secret_access_key`, `//host/:_authToken`, `npmAuthToken`, `github.token`).
- Non-breaking; only more text is redacted.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `ai-assist`: adds a requirement covering secret values introduced by a flag after a secret-named key, `gh secret set` bodies, and config-setter commands.

## Impact

- Code: `src/ai/redact.rs` (spaced branch of `key_assignments_with`, two new entries in `RULES`) and its unit tests.
- No config, CLI or output-format changes. Text that previously leaked these values (and the stray `<redacted>` that replaced the flag) changes in `--show-context` output.
- Out of scope: compose/exec array-form arguments, handled by the sibling change `redact-exec-form-secret-args`.
