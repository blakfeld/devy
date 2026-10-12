# Design

## Context

`src/package_manager/apt.rs` builds the install argv in `sudo_apt_get_argv` as `[SUDO, APT_GET, args…]`, with `SUDO = "/usr/bin/sudo"`. `start_service` and `stop_service` build `Command::new(SUDO).arg(privileged_systemctl()).args([action, "--", name])` inline. All three inherit devy's full environment and stdio. Under an activated shadowenv that environment includes the project's `environment` entries, so `SUDO_PROMPT` can be set. It is not on the reserved-key list.

Repository code runs in the same terminal after these calls. In `install_binary` (src/commands/up.rs), `after_install` and `post_setup` run straight after `runner.install`. The `after_up` hook runs after the service-start phase. In src/commands/down.rs, `after_down` runs after `down_impl`. `before_up` runs before `pm.ensure_available`, which handles bootstrap, so a process it starts in the background is still alive while every privileged call runs.

According to sudo(8), `-k` given with a command makes sudo ignore cached credentials and leave them unchanged. Given alone, `-k` invalidates the current session's cached credential and needs no password. `-K` cannot be combined with a command.

Bootstrap: `nix.rs` `bootstrap` runs the Determinate Systems installer through `installers::run_script` with `install --no-confirm`. When it is not root, that installer re-executes itself with `sudo`, which can cache a credential for the tty. Homebrew's pinned `install.sh` already sets `trap '/usr/bin/sudo -k' EXIT` when no credential was active before it ran. Both run with the scrubbed installer environment (`installers::installer_command`), which already drops `SUDO_ASKPASS` and every other `SUDO_*` variable, because none is on the allowlist.

## Goals / Non-Goals

**Goals:**
- No sudo credential is cached at any moment by a privileged call devy makes, including while that call runs.
- Clear any credential a bootstrap installer leaves behind before repository code runs again.
- Privileged calls get no `SUDO_*` variable from the project's environment.

**Non-Goals:**
- Clearing a credential the user cached themselves before running devy. devy did not create it, and calling `sudo -k` at startup would only remove a cache the user chose to have.
- Defending against a credential the bootstrap installer caches while it is still running (see Risks).
- Batching several apt installs into one sudo call to reduce prompts.
- Adding `SUDO_*` to the reserved `environment` keys. Hooks and `devy exec` can legitimately set them for the user's own sudo calls. This change only keeps them away from devy's privileged calls.

## Decisions

1. **Per-command `sudo -k <cmd>` rather than `sudo -k` after the call.** Clearing the cache afterwards leaves a window: from the password prompt until the reset, a background `sudo -n` poller started by `before_up` (or a `post_setup` child left running from an earlier dependency) can take the credential. With `-k` on the command, sudo never writes a timestamp, so that window does not exist. Alternative considered: `sudo -K` after each call. It cannot be combined with a command, and it would also remove credentials for the user's other terminals.

2. **One argv builder per privileged command, both prefixing `[SUDO, "-k"]`.** `sudo_apt_get_argv` gains `-k`. A new `sudo_systemctl_argv(systemctl, action, name)` replaces the inline builder, so tests can assert the argv without running anything. The error messages (`` `sudo apt-get …` failed``, `` `systemctl start <name>` failed``) stay the same.

3. **A shared `privileged_command(argv, vars)` strips `SUDO_*`.** It builds the `Command` with inherited stdio and calls `env_remove` for every variable in `vars` whose name starts with `SUDO_`. Production code passes `std::env::vars_os()`; tests pass a fixed list and inspect `Command::get_envs()`. We chose `env_remove` of the `SUDO_*` prefix over `env_clear` plus an allowlist: apt-get under sudo already gets sudoers' `env_reset`, and an allowlist risks dropping `TERM`, `LANG` or proxy variables that users rely on in sudoers `env_keep`. We also considered passing a fixed `-p` prompt, which would defeat `SUDO_PROMPT` on its own. It would change sudo's familiar prompt and still leave `SUDO_ASKPASS` and the other variables, so stripping is simpler and covers both.

4. **`sudo -k` after bootstrap, always, for both Nix and Homebrew.** A shared `drop_sudo_credential()` runs `/usr/bin/sudo -k` with null stdin, stdout and stderr, and ignores failures. It does nothing when `/usr/bin/sudo` is missing. `bootstrap` calls it after `run_script` returns, on success and on failure, before turning a failed status into an error. It runs whether or not the installer actually cached anything, because `sudo -k` with no command needs no password and is cheap. For Homebrew this repeats what `install.sh` already does, as defence in depth against a future pin that changes it. For testability, `bootstrap` takes the order through a small seam (`run_then_drop(run, drop)`) so a unit test can assert that `drop` runs after `run` on both outcomes.

5. **Fixed paths only.** The reset uses `SUDO` (`/usr/bin/sudo`), never a PATH lookup, matching the filesystem-safety rule for `sudo`.

## Risks / Trade-offs

- [More password prompts on apt: one per package install and per service start or stop] → This is the intended cost. The README explains it. Users who want fewer prompts can configure `NOPASSWD` for `/usr/bin/apt-get` and `/usr/bin/systemctl` in sudoers. Batching installs is a possible follow-up.
- [The Nix installer's own `sudo` can cache a credential while it runs, and a `before_up` background poller could use it before devy's post-run `sudo -k`] → devy cannot change how the installer calls sudo. The window only exists with `--bootstrap` on a machine without Nix. Recorded under Open Questions. A later change could run `before_up` after bootstrap, or refuse to bootstrap while `before_up` children are alive.
- [Something that is not a hook can still reach root through the user's own existing cache] → Out of scope (see Non-Goals). The README note tells users that devy's prompts are limited to package and service operations.
- [Hosts where `timestamp_type` is `global` or `ppid`] → `-k` with a command ignores and does not update the record for every timestamp type, so the fix holds. The post-bootstrap `sudo -k` without a command clears only the current session's record. That is the record the installer, a child of devy on the same tty, would have written.

## Migration Plan

No data migration. The change only affects behaviour, and reverting the commit rolls it back.

## Open Questions

- Does the pinned Determinate Systems installer cache a sudo credential, which would confirm the bootstrap sub-finding? The design runs `sudo -k` afterwards either way, so the answer changes no spec or task. Task 3.1 records the check.
- Should `before_up` move after bootstrap, to close the residual Nix-installer window? This would change hook order for everyone, so it is left for a separate change.
