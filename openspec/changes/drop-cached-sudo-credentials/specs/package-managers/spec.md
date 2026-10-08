## MODIFIED Requirements

### Requirement: Apt backend
The apt backend SHALL count as available when `/usr/bin/apt-get` exists. It SHALL treat a package as installed when `dpkg-query -W -f=${Status}|${Version} -- <name>` reports `install ok installed` (and, when a version is pinned, the installed version matches exactly), and SHALL install with `/usr/bin/sudo -k /usr/bin/apt-get -y install -- <name>[=<version>]`. The version is passed through as is, so partial versions such as `20` may not resolve. devy never runs `apt-get update` before installing. A failed install reports `` `sudo apt-get <args>` failed — check the output above for details``.

#### Scenario: Pinned version mismatch
- **WHEN** `redis-server` is installed at a different version than the pinned one
- **THEN** devy reports it as not installed and installs `redis-server=<version>`

#### Scenario: Local package file rejected
- **WHEN** `devy.yml` lists `./evil.deb` and the backend is apt
- **THEN** config loading fails and `sudo` is never invoked

### Requirement: Package arguments end option parsing
Every package-manager invocation that takes package names, versions, taps or IDs from configuration or the lock SHALL pass them after an end-of-options marker (`--`) where the tool supports one: `apt-get install`, `dpkg-query -W`, `brew install`, `brew tap`, `brew list`, `npm install -g`, `rustup target add`, `rustup component add`, and `gcloud components install`. For tools without such a marker (winget), values SHALL be passed only as the argument of a named option (`--id <id>`, `--version <version>`). `rbenv install` and `rbenv local` have no usable marker (`rbenv install` treats arguments after `--` as configure options, and `rbenv local` would record `--` itself), so a Ruby version passed to them SHALL be a plain name: an ASCII letter or digit followed only by letters, digits, `.`, `_` or `-`; anything else fails with `invalid Ruby version '<v>'` before rbenv runs. `rbenv install` and `rbenv prefix` SHALL run with `/` as their working directory, so a version name never resolves to a ruby-build definition file in the project or another writable directory. Values SHALL have already passed project-config validation.

#### Scenario: apt receives a separator
- **WHEN** devy installs `redis-server` at version `7.0.15-1` with apt
- **THEN** it runs `/usr/bin/sudo -k /usr/bin/apt-get -y install -- redis-server=7.0.15-1`

#### Scenario: Path-like Ruby version
- **WHEN** ruby is pinned to `./evil` (or `../x`, `-x`)
- **THEN** devy fails with `invalid Ruby version` and never runs `rbenv install`

#### Scenario: rbenv runs outside the project
- **WHEN** devy runs `rbenv install --skip-existing 3.3.6` or `rbenv prefix 3.3.6`
- **THEN** the command's working directory is `/`, so a `3.3.6` file in the project is never read as a ruby-build definition

## ADDED Requirements

### Requirement: Privileged calls leave no cached sudo credential
devy SHALL run a command under `sudo` only to install packages with apt (`apt-get install`) and to start or stop system services with apt (`systemctl start|stop`). Every such call SHALL pass `-k` before the command, so sudo ignores any cached credential and neither creates nor updates one. No credential is left cached during or after the call for repository code (hooks, `after_install`, `post_setup` installs) to reuse with `sudo -n`. These calls SHALL run without any environment variable whose name starts with `SUDO_` (such as `SUDO_PROMPT` or `SUDO_ASKPASS`), so a project's environment cannot reword the password prompt or change how sudo asks for the password. After the Nix or Homebrew bootstrap installer exits, whether it succeeded or failed, devy SHALL run `/usr/bin/sudo -k` (no command; this needs no password) when `/usr/bin/sudo` exists. This invalidates any credential the installer's own `sudo` calls cached for the terminal before devy runs repository code.

#### Scenario: after_install cannot reuse devy's sudo
- **WHEN** a project with `package_manager: apt` lists `jq` with `after_install: "sudo -n id -u"`, `jq` is not installed, sudo requires a password, and the user types it at devy's apt-get prompt
- **THEN** the `after_install` hook's `sudo -n` fails with "a password is required" instead of running as root

#### Scenario: Background poller during install
- **WHEN** a `before_up` hook starts a background loop that runs `sudo -n true` while devy runs `sudo -k apt-get install`
- **THEN** no iteration of the loop succeeds, during the install or after it

#### Scenario: after_up and after_down
- **WHEN** devy runs `sudo -k systemctl start <name>` (or `stop`) and then the `after_up` (or `after_down`) hook
- **THEN** the hook finds no cached sudo credential

#### Scenario: Project sets SUDO_PROMPT
- **WHEN** the activated project environment sets `SUDO_PROMPT="Enter your GitHub token: "` and devy installs a package with apt
- **THEN** sudo shows its own default prompt, because the variable is not passed to `/usr/bin/sudo`

#### Scenario: Nix bootstrap clears the installer's credential
- **WHEN** `devy up --bootstrap` installs Nix and the installer prompts for and caches a sudo credential
- **THEN** devy runs `/usr/bin/sudo -k` once the installer exits, before it installs dependencies or runs any hook after `before_up`

#### Scenario: No sudo installed
- **WHEN** a bootstrap installer exits on a machine without `/usr/bin/sudo`
- **THEN** devy skips the credential reset and carries on, without an error
