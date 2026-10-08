# Tasks

## 1. Reproduce the bug

- [ ] 1.1 Add failing unit tests that call each module's `start`/`is_running`/`stop` with `MockPackageManager` (named `brew`, `apt` or `nix`) and assert the recorded service name: `postgresql` with `version: "16"` under brew expects `postgresql@16`; `mysql` with `version: "8.4"` expects `mysql@8.4`; `mariadb` with `version: "11.4"` expects `mariadb@11.4`; `elastic`, `meili` and `hashicorp-vault` under brew expect `elasticsearch-full`, `meilisearch` and `vault`; `rabbitmq` under apt expects `rabbitmq-server`. Verify they fail on the current code with the raw or unversioned name
- [ ] 1.2 Add guard tests that pass today and must keep passing: `mysql` under apt expects `mysql`; `mysql` with `version: "8.4.2"` and `version_from_lock: true` under brew expects `mysql`; `meili` under nix expects `meili`; `postgres` under nix expects `postgresql`; mongodb under nix keeps its `cfg!(target_os)` name

## 2. Backend-aware service names

- [ ] 2.1 Add `PackageManager::service_name_for(&self, dep) -> String` (default `dep.name`), and implement it for Homebrew via `brew_formula_name`. Verify with brew unit tests for a pin, a lock-injected version and no version
- [ ] 2.2 Change `Module::service_name` to take `pm` and add the `service_package`/`apt_unit` hooks with the defaults from design D2. Update every caller (`start_via_pm`, `log_source`, `legacy_service_note` in `src/commands/check.rs`, and each module's `is_running`/`stop`). Verify with `cargo build` and the updated `default_service_name_*` and `log_source_uses_the_backend_service_name` tests
- [ ] 2.3 Replace the postgres, mariadb and mongodb `service_name` overrides with the hooks, and add overrides for elasticsearch (brew package), rabbitmq (apt unit `rabbitmq-server`) and mongodb (apt unit `mongod`). Verify the section 1 tests now pass, along with `mongodb::service_name_ignores_dep_name` (adapted to take `pm`)
- [ ] 2.4 Add a test that mysql `post_setup` under brew with `version: "8.4"` still asks for the global prefix (`config_prefix_args("mysql")`), so config writing is unaffected. Verify it passes
- [ ] 2.5 Add a `devy logs` test: under brew, `postgresql` with `version: "16"` queries `postgresql@16`. Verify it passes

## 3. Docs and spec

- [ ] 3.1 Update README line ~419 (`<name>` is the backend's service name) with a brew pin example (`postgresql@16`), and add a brew note that devy doesn't stop services an older devy started under an unversioned name (design D4). Verify the README examples match the spec scenarios

## 4. Integration

- [ ] 4.1 Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` and `openspec validate match-service-names-to-packages --strict`, all clean
- [ ] 4.2 On macOS with brew, run `devy up` with `postgresql` `version: "16"` and confirm that `brew services list` shows `postgresql@16` started and that `devy services` reports it running
