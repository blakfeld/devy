# Design

## Context

See proposal.md (Why) for the bug. The current state:

- `project_env::resolve` builds the module and service variables and then calls `merge_env`, which overlays `config.environment` verbatim.
- Three callers use `resolve`:
  - `devy up` (`src/commands/up.rs`), which hands the vars to `ShadowenvManager::write_env`. That writes `(env/set "K" "V")`.
  - `devy exec` (`exec_env::project_environment`).
  - `devy status`, which uses only `path_prepends`.
- shadowenv's `env/set` (Shopify/shadowenv `src/lang.rs`) stores its string argument as is, with no expansion.
- `devy export` (`src/commands/export.rs`) reads `config.environment` directly, with no deps, ports or package manager, and escapes `${` to `\${`, so Nix keeps the text literal.
- `devy check` puts `config.environment` keys into `EnvState` and only compares keys against the written file. It never needs values.
- Producers of `${...}` text: `init_detect::env_example::rewrite`, the AI init schema comment and rules (`src/ai/init_prompt.rs`), and the README.

## Goals / Non-Goals

**Goals:**
- One expansion routine, applied inside the project environment computation, so the shadowenv file, `devy exec`, `devy status` and `devy export` cannot disagree.
- Deterministic output that depends only on `devy.yml`, `devy.lock`, the worktree port file and the modules.
- Clear errors for typos, undefined names and cycles.

**Non-Goals:**
- Shell features such as `$NAME`, `${NAME:-default}`, `${NAME%...}` or command substitution.
- Expanding module-supplied values.
- Expansion in other fields (`commands`, `hooks`).
- Extending `PATH` via `${PATH}`. devy already handles PATH through `path_prepends`.

## Decisions

1. **Syntax: `${NAME}` only, `NAME` = `[A-Za-z_][A-Za-z0-9_]*`.** This matches what devy already generates and documents. Bare `$NAME` stays literal, because `$` is common in passwords and regexes, and expanding it would silently change existing values. *Alternative:* POSIX-like `$NAME` plus `${NAME}`, which was rejected as too likely to corrupt existing literal values.

2. **Escape: `$${` → literal `${`.** This is the same convention docker compose uses, and it only consumes a `$` when a brace follows, so `$$` elsewhere is untouched. *Alternative:* `\${`, which was rejected because YAML double-quoted strings already process `\`, and users would need `\\${`.

3. **Malformed `${` is a load-time validation error** in `DevyConfig::validate`. Examples are an unclosed brace and an invalid name such as `${FOO:-x}`. This makes AI init and `init --detect` validation (`check::static_issues` → `validate`) catch it too. The error suggests `$${`. This is the one breaking change: a config that had a literal `${...}` must now escape it. Any such config was previously shipping that text unexpanded to the shell, which is almost certainly not what was meant.

4. **Resolution scope is the project environment only.** That covers module vars, `<SERVICE>_HOST`/`_PORT`, and `environment` entries, with `environment` winning for a name both define, as `merge_env` already does. The host process environment is not consulted, so the shadowenv file never freezes a value from whatever shell ran `devy up` (`HOME`, tokens, `PATH`). This also keeps secrets in the user's environment out of a committed or shared file such as an export. *Alternative:* fall back to `std::env`, which was rejected for determinism and leakage. It is recorded as an open question.

5. **Undefined name → error, not empty string.** An empty substitution produces URLs like `postgres://:/app` that fail far from the cause. The error is `environment.<KEY>: ${<NAME>} is not defined`.

6. **Recursive expansion of `environment` entries, with cycle detection.** An `environment` value can reference another `environment` entry, which is expanded first (DFS with an in-progress set). A cycle, including a self-reference such as `PATH: "${PATH}:x"`, is an error naming the keys. Substituted text is not rescanned, so a module value containing `${` stays literal.

7. **Unassigned port is a distinct error.** Under `PortMode::ReadOnly` before the first `devy up`, `resolve` omits `<SERVICE>_PORT`. The resolver keeps the set of `<SERVICE>_PORT` names that are known but unassigned. A reference to one gives "`<SERVICE>_PORT` has no port yet; run `devy up`":
   - In `devy exec` and `devy export` this is an error.
   - `devy check` treats it as fine, since it is the expected pre-`up` state and `check` already reports what `up` will do.
   - `devy up` resolves in assign mode before writing, so it never hits this case.

8. **`resolve` becomes fallible (`Result<ProjectEnv>`).** Expansion lives in `project_env`, as a pure function `expand(config_env, base_env, pending) -> Result<HashMap>`, unit-testable without a package manager. Callers propagate the error:
   - `devy up` fails before writing the environment file.
   - `devy exec` exits 1 before spawning.
   - `devy status` records it as the report error instead of panicking.
   - `devy check` calls the same expansion with the module and service names it already resolves, and maps the result per Decision 7.

9. **`devy export` uses the project environment.** It calls `exec_env::project_environment` (read-only ports, no writes) and emits only the `config.environment` keys, with their expanded values. Module variables keep their current behaviour of not being exported. The existing Nix escaping still applies to the expanded value. *Alternative:* emit `${...}` in a `shellHook` for runtime expansion, which was rejected because the export defines no `<SERVICE>_*` variables, so there is nothing for it to expand against.

## Risks / Trade-offs

- [Existing configs with a literal `${`] → They now fail to load, with a message that names the key and the `$${` fix. Document this in the README and mark it BREAKING in the proposal.
- [`devy export` now needs package-manager detection and `devy.lock`] → It uses the same read-only path as `devy exec` and writes nothing else. A project with no deps is unaffected.
- [Exported values freeze the current ports] → This is acceptable. The export is a snapshot, and re-running `devy export` refreshes it. Note this in the README.
- [`devy check` resolves module vars to validate references] → It reuses the dependency resolution `check` already performs, with no extra installs or writes.

## Migration Plan

No data migration. Users with literal `${` in `environment` escape it as `$${`. Rollback is reverting the change. Environment files written by the new version contain plain values, which older versions also write correctly.

## Open Questions

- Should a later change allow opt-in host references (for example `${env:HOME}`) or `PATH` extension? This is deferrable: neither changes the syntax or errors defined here, since `env:` is not a valid `NAME` today.
