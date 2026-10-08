# Proposal

## Why

devy tells users to build connection strings from the variables it injects, and generates such strings itself. `devy init --detect` rewrites `.env.example` values to `postgres://${POSTGRESQL_HOST}:${POSTGRESQL_PORT}/app`, the AI init prompt asks the model to do the same, and the README shows `DATABASE_URL: "mysql://root@${MYSQL_HOST}:${MYSQL_PORT}/myapp_dev"`. But nothing expands `${...}`:

- the project environment passes `environment` values through unchanged
- the shadowenv file writes them verbatim, and shadowenv's `env/set` sets the string literally
- `devy exec` passes them as they are
- `devy export` escapes `${` so Nix keeps it literal

Apps receive the literal `${POSTGRESQL_HOST}` and fail to connect. Hardcoding the value is no alternative either, because ports differ per worktree.

## What Changes

- `${NAME}` in a devy.yml `environment` value is replaced by the value of `NAME` in the project environment. `NAME` can be a module variable, a `<SERVICE>_HOST` / `<SERVICE>_PORT`, or another `environment` entry, which is expanded first. `$${` writes a literal `${`, and any other `$` is left as it is.
- Expansion happens once, when devy computes the project environment, so every consumer sees the same expanded values: the shadowenv file written by `devy up`, `devy exec`, `devy status` and `devy export`.
- **BREAKING**: a malformed reference fails config loading. That means `${` with no closing `}`, or an invalid name inside it. A reference to a name the project environment does not define, or a reference cycle, fails `devy up`, `devy exec`, `devy export` and `devy check` with an error naming the key and the reference. Configs that relied on a literal `${...}` must write `$${...}`.
- A reference to a service port that is not assigned yet (before the first `devy up`) is an error in `devy exec` and `devy export`, and the error says to run `devy up`. `devy check` does not count it.
- `devy export` writes the expanded values instead of the literal text. It resolves ports read-only from `devy.lock` / `.devy/worktree.yml`, as `devy exec` does.
- References are not resolved against devy's own process environment (`${HOME}`, `${PATH}`), so no host value is frozen into the environment file.
- The README documents the syntax, the escape and the error cases.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `project-config`: the environment map supports `${NAME}` references and the `$${` escape, and malformed references fail loading.
- `shell-environment`: the environment file contains expanded values.
- `environment-exec`: the program sees expanded values, and unresolvable references and unassigned ports are errors.
- `nix-export`: exported attributes hold expanded values, and a literal `${` needs `$${`.
- `environment-check`: undefined references and cycles are hard configuration errors.

## Impact

- Code:
  - `src/project_env.rs`: expansion in `resolve`, which becomes fallible.
  - Its callers: `src/commands/up.rs`, `src/commands/exec_env.rs`, `src/commands/status.rs`, `src/commands/check.rs`, and `src/commands/export.rs`, which now uses the project environment.
  - The config validation, for reference syntax.
- Docs: the README environment section.
- Users with a literal `${` in an `environment` value must escape it.
