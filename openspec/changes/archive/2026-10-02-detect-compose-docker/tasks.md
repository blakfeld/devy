# Tasks

## 1. Draft and renderer

- [x] 1.1 Add `docker: bool` to `DraftDep` and render a flagged dependency as a configured entry with `service_manager: docker` (after `version` when present). Verify with a unit test that the rendered draft parses, `normalized_dependencies` marks the dependency docker-managed, and `static_issues` is empty.

## 2. Compose detector

- [x] 2.1 Mark compose-detected services as docker-managed when the module's `docker_spec` returns `Some`, the service was not already in the draft, and the project shows no native-tooling evidence. Verify with unit tests:
  - `postgres:16` → docker
  - with `flake.nix`, `Brewfile` or an `.envrc` containing `use flake` → no docker, plus one `# TODO:` mentioning `service_manager: docker`
  - `postgres` already added from `.tool-versions` → no docker, version unchanged
- [x] 2.2 Update the `tests/cli.rs` Node + compose fixture's expected `devy.yml` to include `service_manager: docker`, and add a case with `flake.nix` that has no `service_manager`.

## 3. Docs and checks

- [x] 3.1 Update the README `init --detect` section to say compose services become docker-managed, and when they don't.
- [x] 3.2 Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` and `openspec validate --specs`. Verify all pass.
