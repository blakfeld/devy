# Design

## Context

`harden-untrusted-repo-security` added a per-user trust store and a gate in front of every command that runs project code. The user decided to drop that gate and keep the rest of the hardening.

## Decisions

- **Delete, don't disable.** `src/trust.rs` and `devy allow` are removed outright; there is no hidden flag or environment variable. Old records under `<state>/devy/trust/` are simply never read.
- **Keep the state directory.** The shadowenv guard still needs the per-user copies of `500_devy.lisp` in `<state>/devy/shadowenv/`. The location, permission and "not inside the project" logic that lived on the trust store moves to a small `state_dir` module, unchanged in behavior.
- **Shadowenv trust follows the directory check alone.** `EnvManager::setup` loses its `trust` flag: after writing `500_devy.lisp`, `devy up` runs `shadowenv trust` whenever the foreign-entry check passes. The guard, the managed-path refusal (which still removes `.trust-*`) and the nonce copy are unchanged. The gate-only checks (summary digest over `.shadowenv.d` entries, the "replaced `500_devy.lisp`" untrust on non-`up` commands) go with the gate; the per-prompt guard already covers a replaced file.
- **Keep the executable-entry listing.** `config_diff::summary`/`render_summary` (with secret masking) remain for AI `init` review and AI `doctor` diffs; `doctor --yes` still refuses fixes that change executable fields. The spec text moves from project-trust to project-config.
- **Ordering in `up`.** Without the gate, `up` loads the config, prints the header, detects the package manager, takes the process lock, then runs the managed-path checks before any hook, so a hostile checkout (committed `.shadowenv.d`, tracked `.venv`, symlinked `.devy`/`.devy-lock`) is still refused before hooks run.

## Risks

- A cloned repository's hooks now run on the first `devy up` without a prompt. Accepted by the user: equivalent to running the repository's scripts.
- `devy down` runs its `before_down` hook without the managed-path check that `up` and `exec` run first; that matches pre-hardening behavior.
