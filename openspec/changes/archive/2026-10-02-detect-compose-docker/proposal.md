# Proposal

## Why

`devy init --detect` turns `docker-compose.yml` services into service dependencies. Since `add-docker-service-manager` landed, devy can run those services as per-project containers, which is what a project with a compose file already does today. Emitting them as plain dependencies makes `devy up` install postgres or redis natively through Nix or brew instead, a different setup from the one the project already uses. The `add-ai-init` design (D7) left this adjustment to whichever of the two changes merged second.

## What Changes

- In `devy init --detect`, a service dependency detected from a compose image is written with `service_manager: docker`, unless:
  - the project shows evidence of native tooling: `flake.nix`, `shell.nix`, `default.nix`, `devbox.json`, `Brewfile`, or an `.envrc` with `use nix` / `use flake`
  - the same service was already detected from a non-compose source, such as `.tool-versions`
  - its module cannot run as a container
- Under the native-tooling exception, compose services keep the package manager as before, and devy adds a `# TODO:` noting that they could run as containers.
- `devy init --ai` is unchanged: its prompt already describes `service_manager`.

## Capabilities

### New Capabilities
None.

### Modified Capabilities
- `project-config`: the Detected init requirement marks compose-detected services as docker-managed.

## Impact

- **Code:**
  - `src/init_detect/` — `DraftDep` gains a docker flag, which the renderer emits as `service_manager: docker`.
  - The compose detector sets the flag and checks for native-tooling evidence.
- **Behavior:** `devy init --detect` output changes only for projects with a compose file. Plain `devy init` and `--ai` are unaffected.
- **Docs:** the README `init --detect` table.
