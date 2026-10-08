# Proposal

## Why

devy's privileged apt calls (`sudo apt-get install`, `sudo systemctl start|stop`) run `/usr/bin/sudo <cmd>`, and sudo then caches the user's credential for the terminal (15 minutes by default on Debian and Ubuntu, one record per tty). devy goes on to run repository code in that same terminal: `after_install` straight after a fresh install, the implicit `post_setup` installs (npm, bundle, pip, …), `after_up` after `sudo systemctl start`, and `after_down` after `sudo systemctl stop`. Any of them can run `sudo -n <cmd>` and get root without a prompt. A repository that sets `package_manager: apt`, lists `jq`, and adds `after_install: "sudo -n …"` gets root as soon as the user types their password at devy's apt prompt. The trust model (see the archived `remove-project-trust` change) accepts that repository code runs as the user, like any script in the repo. It does not accept repository code running as root because the user trusted devy with their password.

## What Changes

- Every privileged call devy makes passes `-k` (`/usr/bin/sudo -k /usr/bin/apt-get …`, `/usr/bin/sudo -k /usr/bin/systemctl start|stop -- <name>`). With a command, `sudo -k` ignores any cached credential and does not create or update one. No credential is cached during or after the call, so a `before_up` hook that polls `sudo -n true` in the background gets nothing.
- These privileged calls run without any `SUDO_*` variable from devy's environment. An activated project environment can no longer set `SUDO_PROMPT` to reword the password prompt, or set `SUDO_ASKPASS` and the other `SUDO_*` variables.
- After the Nix or Homebrew bootstrap installer exits, whether it succeeded or failed, devy runs `/usr/bin/sudo -k`. This drops any credential the installer's own `sudo` calls cached before devy runs repository code. Homebrew's `install.sh` already does this, so for brew it is defence in depth.
- **Behaviour change (not breaking):** with apt, the user is asked for their password for each package install and each service start or stop. Before, one prompt covered 15 minutes. `NOPASSWD` sudoers rules still mean no prompt.
- README: the apt section says devy asks for the sudo password only to install packages and to start or stop system services, and never leaves the credential cached. A password prompt at any other time comes from the repository's own code.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `package-managers`: the apt install argv gains `-k` ("Apt backend" and the "apt receives a separator" scenario under "Package arguments end option parsing"). A new requirement, "Privileged calls leave no cached sudo credential", covers `-k`, the `SUDO_*` environment scrub and the `sudo -k` after a bootstrap.
- `service-management`: under "Apt (systemd) service backend", start and stop run `/usr/bin/sudo -k <systemctl> start|stop -- <name>`.

## Impact

- Code: `src/package_manager/apt.rs` (the argv builders for apt-get and systemctl, and the environment of privileged commands), `src/package_manager/nix.rs` and `src/package_manager/brew.rs` (`bootstrap`). A small shared helper for "drop the sudo credential" is added in `src/package_manager/mod.rs` or `installers.rs`.
- Tests: argv assertions in `apt.rs`, plus environment and bootstrap-ordering unit tests.
- Docs: the README apt section.
- Users on apt see more password prompts during `devy up` and `devy down`.
- No overlap with open changes. `match-service-names-to-packages` modifies "Backend service name" in service-management, not the apt service backend requirement. `derive-java-home-from-installed-jdk` only adds a Homebrew requirement to package-managers.
