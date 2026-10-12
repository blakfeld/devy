# Tasks

## 1. Reproduce the leaks

- [ ] 1.1 Add failing unit tests in `src/ai/redact.rs` `mod tests` for every scenario in the delta spec (`gh secret set API_KEY --body hunter2`, `-b`, quoted body, `--env prod --body`, `deploy_key --body=`, `DB_PASSWORD --value`, `export API_KEY -n` unchanged, `--password -x` still redacted, the aws/npm/yarn/git setters, `git config --global user.name Alice` unchanged). Verify: `cargo test ai::redact` fails on the leak cases and passes on the unchanged/still-redacted cases before any fix

## 2. Key-assignment: flag after a secret-named key

- [ ] 2.1 In the spaced branch of `key_assignments_with`, for a non-flag key whose value starts with `-`: if the word is `--body`/`-b`/`--value`, keep it and redact the next word with `redact_value(…, ValueForm::Spaced, false)`; if it is `--body=`/`--value=`, redact after `=`; otherwise resume at the separator without redacting. Verify: the 1.1 tests for `API_KEY --body`, `-b`, quoted body, `--value` and `export API_KEY -n` pass
- [ ] 2.2 Update the doc comments on `key_assignments`/`spaced_assignment` to describe the dash-led rule. Verify: `cargo doc --no-deps` builds without warnings for the module

## 3. `gh secret set` body rule

- [ ] 3.1 Add `gh_secret_bodies` using `shell_words`, registered as `("gh-secret-body", …)` in `RULES` before `key-assignment`; handle `--body X`, `--body=X`, `-b X`, `-bX`, quoted values, and stop at `|;&` and newlines. Verify: the 1.1 tests for `--env prod --body` and `deploy_key --body=` pass, plus a test that `echo x | gh secret set API_KEY` and `gh secret list` are unchanged

## 4. Config-setter rule

- [ ] 4.1 Add `config_set_secrets` for `aws configure set`, `npm|pnpm|yarn config set` and `git config [options] [set]`, skipping options (and `--file`/`-f` arguments), redacting the value positional when the key names a secret; register `("config-set-secret", …)` before `key-assignment`. Verify: the 1.1 setter tests pass, and `git config user.name Alice` / `npm config set registry https://r.example` are unchanged

## 5. Spec and checks

- [ ] 5.1 Add an end-to-end case to `tests/cli.rs` (`devy init --show-context` on a project whose README contains `gh secret set API_KEY --body hunter2`) asserting `hunter2` is absent. Verify: `cargo test --test cli` passes
- [ ] 5.2 `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` pass; `openspec validate redact-value-after-secret-key-flag --strict` passes
- [ ] 5.3 Run `code-reviewer` and `security-analyst` on the diff and address every finding (per `.claude/rules/review-before-done.md`)
