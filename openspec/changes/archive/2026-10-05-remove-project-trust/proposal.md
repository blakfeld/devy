# Proposal

## Why

The user decided to remove project trust (`devy allow` and the trust gate). Running `devy up` in a repository is no different from running a shell script in it: users vet the repositories they run code from, and a devy-only allow prompt adds friction (CI needs `devy allow`, every pulled `devy.yml` change re-prompts) without protecting anyone who would run the repository's own scripts anyway.

## What Changes

- Remove the `devy allow [--revoke]` subcommand, its help text and its shell-completion entries (bash, zsh, fish).
- Remove the per-user trust store (`$XDG_STATE_HOME/devy/trust/`) and the trust gate on `devy up`, `devy down`, `devy start`, `devy restart` and `devy exec`: they no longer prompt or fail with `project is not allowed`.
- Remove trust bookkeeping: refreshing the record after devy's own `devy.lock` and `devy doctor` writes, and AI `init` recording trust on an interactive confirm.
- `devy up` runs `shadowenv trust` whenever `.shadowenv.d` holds only devy's own files, without the "project is trusted" condition.
- `devy export` writes `environment` entries as escaped attributes again instead of commenting them out for an unallowed project.
- Keep all other hardening: config and lock validation, reserved environment keys, safe writes and managed-path checks, the per-prompt shadowenv guard (nonce and per-user copy of `500_devy.lisp`), loopback service binds, pinned installers, redaction, AI init/doctor review of executable fields (the listing of what a config runs moves from the trust summary to project-config), export escaping and output sanitization.
- Not breaking for users who never adopted `devy allow`; scripts that call `devy allow` must drop the call (the subcommand no longer exists).

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `project-trust`: retired; every requirement is removed (`retire_capabilities: true`).
- `cli`: `allow` is no longer a built-in subcommand.
- `environment-up`: no trust step in the phase order; no trust-record update after writing the lock.
- `shell-environment`: `shadowenv trust` no longer depends on project trust; the state-directory placement rules move here from the trust store; error text drops `devy allow`.
- `shell-integration`: completion drops `allow`/`--revoke`; guard text no longer refers to project trust.
- `nix-export`: `environment` entries are always written as escaped attributes.
- `project-config`: AI init review no longer records trust; gains the "Executable entry listing" requirement (formerly the trust summary); value-validation wording updated.
- `environment-doctor`: accepted fixes no longer refresh trust; `--yes` still refuses executable-field changes.
- `filesystem-safety`, `package-managers`: wording that referred to allowed projects or the trust summary.

## Impact

- Code: `src/trust.rs` and `src/commands/allow.rs` deleted; new `src/state_dir.rs` keeps the per-user state directory used for the shadowenv copies; `up`, `down`, `service`, `exec_env`, `export`, `init`, `doctor`, `hook`, `env_manager`, `config_diff` and `lock` updated.
- CI: `devy allow` removed from `.github/workflows/integration.yml` and `.github/scripts/doctor-e2e.sh`.
- Docs: README "Trusting a project" section removed; agent skill/AGENTS.md text no longer mentions `devy allow`.
- Existing trust records on users' machines are ignored and can be deleted.
