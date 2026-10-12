# Design

## Context

`key_assignments_with` (`src/ai/redact.rs`) matches a secret-named key followed by a separator. For the whitespace separator, `spaced_assignment` decides whether `KEY value` is an assignment: always for a flag key, and for an env-style (all upper-case) key such as `API_KEY`. `redact_value` with `ValueForm::Spaced` then redacts one word, without looking at what that word is. So `gh secret set API_KEY --body hunter2` redacts `--body`. The `alone` directive check already refuses a value starting with `-`; the env-style path does not.

Lower-case and dotted config keys (`aws_secret_access_key`, `github.token`, `npmAuthToken`) fail `spaced_assignment` mid-line by design, to keep prose such as `the monkey business` intact, so config-setter commands leak their values.

`text` applies `RULES` in order. Command-specific rules (`redis-cli-password`, `htpasswd`, `cf-auth`) sit before `key-assignment` and use `shell_words` or a command-anchored regex.

## Goals / Non-Goals

**Goals:** never send the value after `--body`/`-b`/`--value` following a secret-named key; cover `gh secret set` bodies in any argument position; cover the four config setters named in the review.

**Non-Goals:** a general argv parser for arbitrary CLIs; compose/exec array-form args (sibling change `redact-exec-form-secret-args`); `credential_parts`/`credential_parts_inline` (they call `key_assignments_with(_, false)`, which never takes the spaced form, so they are unaffected).

## Decisions

- **Dash-led spaced values are not values (non-flag keys only).** In the spaced branch, when the key does not start with `-` and the value starts with `-`, the match is not an assignment of that word. Alternative considered: skip leading flags and redact the next non-flag word. Rejected: `export API_KEY -n` or `chmod KEY -R` would then redact unrelated words, and a flag that takes a non-secret value (`--env prod`) would be redacted while the secret is not. Flag keys (`--password -x`) keep redacting a dash-led value, since a password can start with `-`.
- **A short allow-list of value-carrying flags.** When the dash-led word is `--body`, `-b`, `--value`, `--body=…` or `--value=…`, redact the next word (or the `=` part) and keep the flag. These are the flags that carry a secret value after a secret name in common tools (`gh secret set`, `gh variable set`, various vault/secret CLIs). Kept short on purpose: `-b` is generic, but only counts directly after a secret-named key.
- **Dedicated `gh-secret-body` rule.** The key-assignment fix only handles a flag immediately after the key. `gh secret set NAME --env prod --body X` and `gh secret set deploy_key --body X` (name not env-style) need a command-anchored rule. It walks `shell_words` of the `gh secret set` command and redacts the word after `--body`/`-b`, or the tail of `--body=`/`-b<glued>`. Name-independent, because any `gh secret` body is a secret. Placed before `key-assignment`, whose later pass then sees `<redacted>` and keeps it (`redact_plain_value` already leaves an existing `<redacted>` alone).
- **The MEDIUM config-setter leak is included, as a separate `config-set-secret` rule.** It is the same root shape (`<secret key><space><value>` that the spaced rule rejects), the fix is small, and leaving it would keep the same class of leak in the same files (Makefiles, READMEs, CI). It is a separate rule rather than a widening of `spaced_assignment`, because relaxing the lower-case key check globally would redact prose. The rule anchors on the command (`aws configure set`, `npm|pnpm|yarn config set`, `git config`), skips options (`--global`, `--file <path>`, `-f <path>`, the `set` subcommand of git 2.46+), takes the first positional as the key and redacts the next positional when `names_a_secret(key)` (or `is_secret_key`) holds. `npm config set key=value` is already covered by the `=` form.
- **ADDED requirement, not MODIFIED "Redaction before sending".** The sibling change also extends redaction. A separate requirement avoids two changes rewriting the same requirement block and conflicting at archive.

## Risks / Trade-offs

- [`-b` after a secret-named key in a non-gh tool takes a non-secret value] → over-redaction of one word, which is the safe direction.
- [A real secret that starts with `-` after an env-style key, e.g. `API_KEY -abc123`] → no longer redacted. Rare; token-prefix rules still apply. Accepted to fix the common leak.
- [Other config setters (`dotnet user-secrets set`, `heroku config:set` without `=`, `gcloud … --set-…`)] → not covered; see Open Questions.

## Open Questions

- Should `dotnet user-secrets set <key> <value>` (always a secret) and `pnpm`/`npm set` (alias of `config set`) be added to the config-setter rule? They fit the same rule and can be added later without changing the approach.
