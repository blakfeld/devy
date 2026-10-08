# Tasks

## 1. apt-get under `sudo -k`

- [ ] 1.1 Write the failing tests first. In `src/package_manager/apt.rs`, update `apt_install_uses_absolute_sudo_and_separator` to expect `["/usr/bin/sudo", "-k", "/usr/bin/apt-get", "-y", "install", "--", "redis-server=7.0.15-1"]`, and update `planted_sudo_on_project_path_is_not_used` to assert `argv[..3] == [SUDO, "-k", APT_GET]`. Verify that `cargo test apt_install_uses_absolute_sudo` fails on the current code.
- [ ] 1.2 Make `sudo_apt_get_argv` emit `[SUDO, "-k", APT_GET, args…]` and update its doc comment. Verify that the tests from 1.1 pass.

## 2. systemctl under `sudo -k`, and the SUDO_* scrub

- [ ] 2.1 Add a failing test `sudo_systemctl_argv_passes_k` asserting that `sudo_systemctl_argv("/usr/bin/systemctl", "start", "redis-server")` is `["/usr/bin/sudo", "-k", "/usr/bin/systemctl", "start", "--", "redis-server"]`, and the same for `stop`. Verify that it fails (does not compile, or fails) before 2.2.
- [ ] 2.2 Add `sudo_systemctl_argv` and use it in `start_service` and `stop_service` in place of the inline builders, keeping the existing error messages. Verify that 2.1 passes and `privileged_systemctl_is_a_fixed_absolute_path` still passes.
- [ ] 2.3 Add a failing test `privileged_command_strips_sudo_vars`. Build a command with `privileged_command(&argv, vars)`, where `vars` holds `SUDO_PROMPT`, `SUDO_ASKPASS`, `SUDO_EDITOR`, `PATH` and `LANG`. Assert that `get_envs()` reports every `SUDO_*` name as removed (`None`) and does not touch `PATH` or `LANG`. Then add `privileged_command` (inherited stdio, `env_remove` for every `SUDO_`-prefixed name) and route `run_apt_interactive`, `start_service` and `stop_service` through it with `std::env::vars_os()`. Verify that the test passes.

## 3. Drop the credential after bootstrap installers

- [ ] 3.1 Needs verification. Check the Determinate Systems installer pinned in `installers::NIX`: when it runs as a non-root user, does it re-execute through `sudo` and leave a tty timestamp? Check its source at the pinned tag, or run it in a throwaway VM followed by `sudo -n true`. Record the result in this task's PR description. The fix in 3.3 applies regardless of the answer.
- [ ] 3.2 Add a failing unit test for a `run_then_drop(run, drop)` seam in `src/package_manager/mod.rs` (or `installers.rs`). Use counters or recorded call order to assert that `drop` runs exactly once after `run`, both when `run` returns a non-success status and when it returns `Err`, and that the original result or error is returned unchanged.
- [ ] 3.3 Implement `run_then_drop` and `drop_sudo_credential()`. The latter runs `/usr/bin/sudo -k` with null stdio, ignores its result, and does nothing when `/usr/bin/sudo` is not a file. Wrap the `run_script` calls in `NixPackageManager::bootstrap` (src/package_manager/nix.rs) and the brew `bootstrap` (src/package_manager/brew.rs) with it. Verify that 3.2 passes, and that the existing `Nix installation failed` and `Homebrew installation failed` tests still pass.

## 4. Docs

- [ ] 4.1 In README.md's "Ubuntu/Debian (apt)" paragraph, say that devy runs `sudo -k`, so it asks for your password for each package install and each service start or stop, and never leaves the credential cached. Say that devy's only sudo prompts are for those package and service operations, so a prompt at any other time comes from the repository's own hooks or scripts. Mention `NOPASSWD` for `/usr/bin/apt-get` and `/usr/bin/systemctl` as the way to get fewer prompts. Update the `README.md:188` comment if needed. Verify by reading the rendered section.

## 5. Integration check

- [ ] 5.1 Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. All pass.
- [ ] 5.2 Manual check on an Ubuntu VM or container with sudo, as a non-root user whose sudo needs a password. Use a `devy.yml` with `package_manager: apt`, `jq` (with `after_install: "sudo -n id -u || echo NO-ROOT"`), `redis-server`, and `after_up: "sudo -n true && echo CACHED || echo CLEAN"`. Run `devy up` and confirm `NO-ROOT` and `CLEAN`. Run it with `SUDO_PROMPT=pwned: ` exported and confirm sudo's default prompt appears.
