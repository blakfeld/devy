# Tasks

## 1. Reproduce the bug

- [ ] 1.1 In `src/commands/up.rs` tests, add `start_service_if_needed_recreates_running_container_with_changed_config`: a `DockerRunner` over a `FakeRunner` whose inspect reports a running, project-owned container with a stale `sh.devy.config` label; assert the recorded calls include `rm -f <name>` and `run`. Verify it FAILS on the current code (the service is skipped as already running)
- [ ] 1.2 In `src/commands/service.rs` tests, add `start_impl_recreates_running_container_with_changed_config` with the same setup for `start_impl`; verify it FAILS on the current code
- [ ] 1.3 Add a counterpart test for each command where the running container's label matches the current hash; assert no `rm`, `run` or `start` call is recorded (only inspect). Verify these pass before and after the fix

## 2. Runner hook

- [ ] 2.1 Factor the label-vs-spec comparison out of `DockerRunner::start` into a private helper (`config_matches(&ContainerState, &RunSpec)`), treating a missing label as a mismatch; verify the existing `start_*` tests in `src/service_runner/tests.rs` still pass
- [ ] 2.2 Add `fn needs_recreate(&self, dep) -> Result<bool>` to `ServiceRunner` with a default of `Ok(false)` and a doc comment (read-only; true only for a running, owned container whose configuration differs); verify `PackageRunner` and test runners compile unchanged
- [ ] 2.3 Implement it for `DockerRunner` using the helper: `false` for no container, a stopped one or a foreign one; otherwise compare against `run_spec(dep)`. Add unit tests in `src/service_runner/tests.rs` for: running + same hash → false; running + different port → true; running + no label → true; stopped + different hash → false; foreign running → false; inspect failure → error. Verify `cargo test service_runner` passes

## 3. Use the hook in `up` and `start`

- [ ] 3.1 Change `start_service_if_needed` to skip only when `is_running && !needs_migration && !needs_recreate`, printing `Recreating <name> container (configuration changed)` before `start` when `needs_recreate` is true; verify task 1.1 now passes and the existing `start_service_if_needed_*` tests still pass
- [ ] 3.2 Make the same change in `service::start_impl`; verify task 1.2 now passes and the existing `start_impl_*` and `restart_*` tests still pass
- [ ] 3.3 Add a test that `needs_recreate` errors propagate from both commands with the service name in the message; verify it passes
- [ ] 3.4 Confirm `list_impl`, `stop_impl`, `restart_impl` and `down` do not call `needs_recreate` (add a test that `devy services` listing with a stale running container reports it running and records no `rm`/`run`); verify it passes

## 4. Docs and integration

- [ ] 4.1 Update README's docker "Ports" paragraph to mention the `Recreating <name> container (configuration changed)` message on `devy up` and `devy start`; verify the README statement matches the spec scenarios
- [ ] 4.2 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`; verify all pass
- [ ] 4.3 If docker is available locally, manually check: `devy up` with redis, change `port` in `devy.yml`, `devy up` again; verify the container is recreated on the new port with its data and `REDIS_PORT` matches `docker port`
