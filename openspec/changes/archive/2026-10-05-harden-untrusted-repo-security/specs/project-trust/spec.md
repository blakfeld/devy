# Spec Delta

## Purpose

Makes running repository-defined code an explicit user decision, so cloning a repo and running devy cannot execute that repo's hooks, install scripts or shell environment until the user has reviewed and allowed them.

## ADDED Requirements

### Requirement: Trust records
devy SHALL keep a per-user trust store outside any project, at `$XDG_STATE_HOME/devy/trust/` (default `~/.local/state/devy/trust/`) on macOS and Linux and `%LOCALAPPDATA%\devy\trust\` on Windows. The store directory and its files SHALL be created with mode 0700 and 0600 respectively. A trust record SHALL hold the canonical project root and a SHA-256 digest of the bytes of `devy.yml` and of `devy.lock`. An absent `devy.lock`, and one that is not a regular file, SHALL each be recorded as a distinct value that no file digest can equal, so creating an empty lock changes trust. The record SHALL also hold a digest of the trust summary the user reviewed and of the content of the wrapper scripts devy runs directly (`gradlew`, `mvnw`, `gradle/wrapper/gradle-wrapper.properties`, `gradle/wrapper/gradle-wrapper.jar`, `.mvn/wrapper/maven-wrapper.properties`, `.mvn/wrapper/maven-wrapper.jar`, `.mvn/wrapper/MavenWrapperDownloader.java`), because files outside `devy.yml` decide what runs (a `package.json` added later makes devy run `npm install`). The digest SHALL also cover the names and kinds of the entries in the project's `.shadowenv.d/` other than the regular files devy and `shadowenv trust` write: `500_devy.lisp`, and `.gitignore`, `.trust-*` and the `.error-*` files shadowenv's hook writes (one per shell that meets the directory untrusted, so counting them would untrust the project again after every `devy allow`) unless their name ends in `.lisp` (compared case-insensitively), because shadowenv signs the directory, not its content: a pull that only adds lisp there (`.trust-x.lisp` included) makes the project untrusted, and the gate then removes shadowenv's trust. A `500_devy.lisp` that is not a regular file is such an entry. The content of a regular `500_devy.lisp` cannot be part of the recorded digest, since devy rewrites that file on every `up` and never reads it to decide what runs; instead, when a trusted project's `.shadowenv.d/500_devy.lisp` is not the file devy last wrote (not identical to the per-user copy its first line names, as shell-environment defines), the trust gate of every command SHALL remove shadowenv's trust files before continuing, and a command other than `devy up` SHALL say `devy removed shadowenv's trust: .shadowenv.d/500_devy.lisp is not the file devy wrote. The shell environment stays off until you run devy up.` `devy up` rewrites the file and trusts the directory again. The contents of package manifests are not covered. A project SHALL be trusted only when a record exists for its canonical root and the digests of both files and of the current trust summary match. The digests recorded on allowing SHALL be taken before the summary is shown; if `devy.yml` or `devy.lock` changed in between, allowing SHALL fail with `devy.yml or devy.lock changed while you were reviewing it — run the command again`. devy SHALL refuse a trust store inside the project, judged by where the store is or, before it exists, where it would be created (its nearest existing ancestor, resolved), unless it is the platform default location (for example below a project at `$HOME`, a dotfiles repository) and git does not track it. The platform default SHALL be decided by the resolved path (`$HOME/.local/state/devy`, or `%LOCALAPPDATA%\devy`), not by whether `XDG_STATE_HOME` is set.

#### Scenario: Edited config invalidates trust
- **WHEN** a project was allowed and `devy.yml` is then changed by `git pull`
- **THEN** the project is no longer trusted until the user allows it again

#### Scenario: Same content at another path
- **WHEN** an allowed project's files are copied to a different directory
- **THEN** the copy is not trusted

#### Scenario: Store planned inside the project
- **WHEN** `XDG_STATE_HOME` names a directory inside the project that does not exist yet
- **THEN** gated commands and `devy allow` fail naming the trust store as inside the project, and nothing is created there

### Requirement: Allow command
devy SHALL provide `devy allow`, which loads `devy.yml`, prints the trust summary, and records trust for the current project without prompting. On success it SHALL print `✓ allowed <project_root>`. `devy allow --revoke` SHALL delete the record and print `✓ revoked trust for <project_root>`, and SHALL succeed when no record exists. Because shadowenv has no `untrust` command, `--revoke` SHALL also remove shadowenv's trust files (`.shadowenv.d/.trust-*`) when `.shadowenv.d` is a real directory in the project, so the shell stops applying `devy.yml`'s environment; a failure to remove them SHALL be a warning. It SHALL also remove the per-user copy of the project's `500_devy.lisp` (shell-environment), so the shell hook's guard refuses a `shadowenv trust` run by hand until the next `devy up`. `devy allow` SHALL fail like other commands when `devy.yml` is missing or invalid. `devy allow --revoke` SHALL only locate `devy.yml`, without parsing it, so a project whose `devy.yml` no longer parses can still be revoked.

#### Scenario: Allow then up in CI
- **WHEN** a CI job runs `devy allow` and then `devy up` with stdin not a terminal
- **THEN** `devy up` runs without prompting

#### Scenario: Revoke
- **WHEN** the user runs `devy allow --revoke` in an allowed project
- **THEN** the next `devy up` prompts for trust again

#### Scenario: Revoke also untrusts shadowenv
- **WHEN** an allowed project has `.shadowenv.d/.trust-<fingerprint>` written by `shadowenv trust` and the user runs `devy allow --revoke`
- **THEN** the trust file is removed, `.shadowenv.d/500_devy.lisp` is kept, its copy in `~/.local/state/devy/shadowenv/` is removed, and shadowenv no longer applies the project's environment

### Requirement: Trust summary
Before trust is granted, interactively or by `devy allow`, devy SHALL print what the project will run, grouped under `Hooks`, `Install commands`, `Project setup`, `System packages`, `Package sources` and `Environment`:
- every hook command
- every `after_install` and `install_cmd`
- each implicit project setup step and the file that drives it (for example `npm install (package.json lifecycle scripts)`, `bundle install (Gemfile)`, `pip install -e . (pyproject.toml)`, `./gradlew (repository script)`, and for rust `cargo and rustc use toolchain and cargo settings from the project or its parent directories (<files>)` when `rust-toolchain`, `rust-toolchain.toml`, `.cargo/config` or `.cargo/config.toml` exists in the project or a parent directory below the user's home (listed relative to the project, e.g. `../.cargo/config.toml`), since they can name a path toolchain, `rustc-wrapper`, linkers or runners)
- every dependency installed through the package manager (every dependency that is not docker-managed), as its name, `@<version>` when one is set, and the backend `devy up` uses: `(nix)`, `(brew)`, `(sudo apt-get)` or `(winget)`, from `package_manager` and, when it is unset, the platform default; a dependency whose module installs another way is labelled with the route `devy up` really takes, including `sudo`: a module that always bypasses the package manager names its installer (`rust (rustup)`, `bun (bun-installer)`, `deno (deno-installer)`), and a module whose route depends on the backend names it for that backend (`ruby (rbenv via sudo apt-get)`, since apt installs rbenv and ruby-build before rbenv builds Ruby, `ruby (rbenv via nix)`, `ruby (rbenv via brew)` or `ruby (winget)`; `gcloud (brew)` or `gcloud (winget)`, and `gcloud (gcloud-installer)` on other backends). Because these entries are part of the summary, they are part of its digest, and the config diff (doctor fixes, AI init) counts an added or changed package as an executable change
- every `tap` and `image`
- every `environment` entry, because shadowenv, `devy exec` and a trusted export apply them all; this includes every key that changes how programs run, such as `PATH`, any key starting with `LD_`, `DYLD_`, `GIT_CONFIG`, `npm_config_`, `NPM_CONFIG_`, `NIX_` or `__`, `NODE_OPTIONS`, `NODE_PATH`, `RUBYOPT`, `RUBYLIB`, `PYTHONPATH`, `PYTHONSTARTUP`, `PYTHONHOME`, `PERL5OPT`, `PERL5LIB`, `JAVA_TOOL_OPTIONS`, `_JAVA_OPTIONS`, `JDK_JAVA_OPTIONS`, `GIT_SSH_COMMAND`, `GIT_SSH`, `GIT_EXEC_PATH`, `GIT_PROXY_COMMAND`, `GIT_ASKPASS`, `GIT_EDITOR`, `GIT_PAGER`, `SSH_ASKPASS`, `SUDO_ASKPASS`, `BASH_ENV`, `ENV`, `fish_user_paths`, `EDITOR`, `VISUAL`, `PAGER`, `MANPAGER`, `LESSOPEN`, `LESSCLOSE`, `BROWSER`, `SHELL`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `LOCALAPPDATA`, `RUSTC`, `RUSTC_WRAPPER`, `RUSTDOC`, `CARGO`, `CC`, `CXX`, `LD`, and the names `mkShell`/stdenv evaluate in an export (`builder`, `args`, `stdenv`, keys ending in `Hook`, `Hooks` or `Phase`, and `pre`/`post` followed by an upper-case letter). Keys reserved for the interactive shell (such as `PROMPT_COMMAND`, `PS1`, `IFS`, `HOME`, `XDG_STATE_HOME` or `HISTFILE`) never reach the summary: loading the config refuses them first (project-config)

Values SHALL be printed in full, with control characters stripped and whitespace runs collapsed. In `environment` values, the credential-looking parts the ai-assist value redaction recognizes (URL userinfo passwords, well-known token prefixes, secret-named assignments) SHALL always be replaced by `<redacted>`, so the summary never prints a secret to a terminal or CI log. A secret-named assignment inside a value SHALL hide only its own value, up to the next whitespace or quote, never the rest of the value (so `GIT_SSH_COMMAND="TOKEN=1 sh -c '…'"` keeps the command visible), and a value whose parts were hidden SHALL be followed by `(partly hidden; review it in devy.yml)`. (An unquoted spaced value of such an assignment, as in `password=correct horse`, therefore shows the words after the first; that note says the value needs review.) For a key that names a secret under the ai-assist key rule, the whole value SHALL be replaced by `<redacted>`, except as follows (keys are matched on whole words, split at `_`, `-`, `.` and camelCase):
- a switch value (`0`, `1`, `true`, `false`, `yes`, `no`, `on`, `off`) is shown for any key (`NODE_TLS_REJECT_UNAUTHORIZED=0`), and a mode word (`never`, `prefer`, `force`, `always`, `auto`) for any key that does not name a credential outright (`SSH_ASKPASS_REQUIRE=force`);
- `GIT_CONFIG_KEY_<n>` names a git config key, not a secret, and is shown;
- a key that names a credential outright (a word `PASSWORD`, `PASSWD`, `PWD`, `TOKEN`, `SECRET`, `PRIVATE`, `KEY`, `APIKEY`, `AUTH`, `COOKIE`, `SESSION`, `DSN`, `SIG`, `SIGNATURE`, `SALT`, `PAT`, `PIN`, `CREDS` or `PW`, or a plural; a word `CREDENTIAL`, `KEYSTORE`, `KEYRING` or `KEYCHAIN`; `PASS`, `TOKEN` or `SECRET` inside a word; or a word ending in `KEY` or `AUTH`; not `*ASKPASS`, the `AUTH` of `*_AUTH_SOCK` or the `SESSION` of `DBUS_*`) shows only a reference to a program: a path when its last word is `FILE`, `DIR` or `PATH` (`ANSIBLE_VAULT_PASSWORD_FILE=./x` or `scripts/vault.sh`), or, when its last word is `OPTIONS` or `OPTS`, a value whose first word starts with `-` or like a path (`PASSWORD_STORE_GPG_OPTS=--no-throw-keyids`), or, when its last word is a command word (`COMMAND`, `HELPER`, `CMD`, `PROVIDER`, `PROVIDERS`, or ending in `COMMAND` as in `BORG_PASSCOMMAND`; so `CARGO_REGISTRY_CREDENTIAL_PROVIDER`, `CARGO_REGISTRIES_<name>_CREDENTIAL_PROVIDER` and `CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS` are command keys), a bare program name (letters, `-`, `_` and `.` only, as in `GIT_CREDENTIAL_HELPER=osxkeychain`) or a command whose first word starts like a path or is a common secret-printing program (`sh`, `bash`, `op`, `pass`, `gpg`, `security`, `vault`, `aws`, `curl`, …), any of whose words starts like a path, or that contains whitespace and a shell operator (`|`, `;`, `&&`, `$(`, a backtick, `<`, `>`), so a passphrase such as `correct horse` stays hidden (a command matching none of these, such as `npx pkg`, is shown as `<redacted> (a command; review it in devy.yml)`; the digest still covers it);
- a key whose last word ends in `ASKPASS` names the program that asks for a password, never the password, and its value is always shown;
- a program key (last word a command word, `OPTIONS`, `OPTS` or `PATH`; first word `LD` or `DYLD`) shows: for a command word, a command as above or a bare program name (letters, `-`, `_` and `.` only); for `OPTIONS` or `OPTS`, a value whose first word starts with `-` or like a path; otherwise a path or a value that contains whitespace;
- a file or socket key (a word `CERT`, `CERTS`, `CA` or `CREDENTIALS`, last word `SOCK`, first word `DBUS`, or `XAUTHORITY`) shows a value without whitespace that starts like a path, or is a relative path of plain segments ending in a certificate, key, credentials or config extension (`pem`, `crt`, `cer`, `der`, `key`, `p12`, `pfx`, `json`, `jks`, `ini`, `toml`, `yaml`, `yml`, `sock`, `conf`), a relative path of plain segments with a `/` or `\` (`certs/evil`; for a `*CREDENTIALS` key only when its last word is `FILE`, `DIR` or `PATH`, since `user/password` is a credential), or, for a `DBUS_*` or `*SOCK` key, a `unix:path=`/`unix:abstract=` address.

A path, here, is a value without whitespace that starts like a path (`/`, `./`, `../`, `~/`, `\` or a drive letter), is a relative path of plain segments (letters, digits, `.`, `-`, `_`) separated by `/` or `\` (`scripts/askpass.sh`), or is a bare file name with an extension (`vault.sh`); a bare word without an extension (`hunter8`) is not, since it is as likely a password. Whenever a whole value is replaced by `<redacted>`, it SHALL be followed by `(may name a program or file; review it in devy.yml)` (or, under a command word, `(a command; review it in devy.yml)`), unless the key's last word names the secret itself (`PASSWORD`, `PASSWD`, `PASS`, `PASSPHRASE`, `PWD`, `TOKEN`, `SECRET`, `KEY`, `APIKEY`, `PIN`, `AUTH`, `SALT`, `SIG`, `SIGNATURE` or `CREDENTIAL`, or a plural) and the key is not also a program or file key. So `API_TOKEN=<redacted>` has no note, while a hidden `PASSWORD_STORE_GPG_OPTS`, `MY_PASSWORD_MODE` or `SONATYPE_CREDENTIALS` (a file key) value has one.

A value's shape alone never reveals a value under a credential name: passwords can contain spaces, `&`, `:` or `/`. So `GIT_ASKPASS=./scripts/x.sh`, `GIT_ASKPASS=scripts/askpass.sh`, `NODE_EXTRA_CA_CERTS=certs/evil` and `SSL_CERT_FILE=/tmp/ca.pem` are shown, while `API_TOKEN=ghp_…`, `DB_PASSWORD="correct horse battery staple"` and `AWS_SECRET_ACCESS_KEY=wJal…/K7MD…` are not. The trust record SHALL cover the unredacted values. Groups with no entries SHALL be omitted.

#### Scenario: Secret environment value masked
- **WHEN** `devy.yml` sets `environment.API_TOKEN: hunter2` and the user runs `devy allow`
- **THEN** the summary shows `API_TOKEN=<redacted>` and not `hunter2`, and changing the value later makes the project untrusted again

#### Scenario: Execution-affecting values are shown
- **WHEN** `devy.yml` sets `environment.GIT_ASKPASS: ./scripts/x.sh` and `environment.SSL_CERT_FILE: /tmp/evil.pem`
- **THEN** the summary shows `GIT_ASKPASS=./scripts/x.sh` and `SSL_CERT_FILE=/tmp/evil.pem` in full

#### Scenario: Relative program paths are shown
- **WHEN** `devy.yml` sets `environment.GIT_ASKPASS: scripts/askpass.sh`, `environment.ANSIBLE_VAULT_PASSWORD_FILE: scripts/vault.sh` and `environment.CREDENTIALS_PATH: hunter8`
- **THEN** the summary shows the first two in full and `CREDENTIALS_PATH=<redacted> (may name a program or file; review it in devy.yml)`

#### Scenario: Summary lists hooks and setup
- **WHEN** `devy.yml` declares `node` with a `package.json` present and `hooks.after_up: "make seed"`
- **THEN** the summary lists `make seed` under `Hooks` and `npm install (package.json lifecycle scripts)` under `Project setup`

#### Scenario: Summary lists system packages
- **WHEN** `devy.yml` sets `package_manager: apt` and declares `jq` and `node` with version `20`
- **THEN** the summary lists `jq (sudo apt-get)` and `node@20 (sudo apt-get)` under `System packages`

### Requirement: Trust gate on commands that run project code
`devy up`, `devy down`, `devy start`, `devy restart` and `devy exec` SHALL check trust after loading `devy.yml` and before running any hook, installing anything, starting any service or writing any file. When the project is not trusted:
- if stdin and stderr are terminals, devy SHALL print (to stderr) `This project has not been allowed` (or `devy.yml, devy.lock or the project files devy runs changed since this project was allowed`), print the trust summary, and ask `Allow and continue? [y/N]`. On `y` or `yes` (case-insensitive) it SHALL record trust and continue; otherwise it SHALL exit 1 without side effects. Any failure of the trust check itself (for example an unreadable store) SHALL also exit 1 without writing anything to the project, including the `devy up` failure record.
- otherwise devy SHALL fail with `project is not allowed — review devy.yml and run devy allow` and exit 1.

Because `shadowenv trust` signs only the directory, not its content, a project that is not trusted (no record, a stale one, a trust check that fails, a trust store that cannot be located, or a `devy.yml` that no longer loads) SHALL also lose shadowenv's trust files (`.shadowenv.d/.trust-*`, removed as `devy allow --revoke` removes them) before devy prompts or fails, so lisp pulled into `.shadowenv.d` after an earlier trusted `up` stops loading on every prompt. A failure to remove them SHALL be a warning. When the user then allows the project at the prompt of a command other than `devy up`, devy SHALL say that the shell environment stays off until `devy up` runs. Before any devy command runs, the shell hook's guard (shell-integration) removes the same trust files at the prompt, ahead of shadowenv's own hook, when `.shadowenv.d` holds files devy did not write.

`devy check`, `devy status`, `devy services`, `devy logs`, `devy ask`, `devy doctor`, `devy export`, `devy init`, `devy hook`, `devy pr`, `devy stop`, `_commands`, `_services`, and explicitly invoked project commands SHALL NOT require trust.

#### Scenario: Untrusted repo in a terminal, declined
- **WHEN** the user clones a repo with `hooks.before_up: "touch /tmp/pwned"`, runs `devy up` in a terminal and presses Enter
- **THEN** `/tmp/pwned` does not exist, nothing is installed, and devy exits 1

#### Scenario: Untrusted repo without a terminal
- **WHEN** `devy up` runs with stdin not a terminal in a project that was never allowed
- **THEN** devy prints `project is not allowed — review devy.yml and run devy allow` and exits 1

#### Scenario: Stale trust untrusts shadowenv
- **WHEN** a project was allowed and `devy up` ran `shadowenv trust`, then `git pull` changes `devy.yml`, and the user runs `devy up` without a terminal
- **THEN** devy fails with the not-allowed message and `.shadowenv.d/.trust-*` no longer exists

#### Scenario: Pulled shadowenv lisp untrusts the project
- **WHEN** a project was allowed and `devy up` ran `shadowenv trust`, then a pull adds only `.shadowenv.d/000_x.lisp`, and the user runs a gated command
- **THEN** the project is untrusted and `.shadowenv.d/.trust-*` is removed before devy prompts or fails

#### Scenario: shadowenv's error files keep trust
- **WHEN** an allowed project's `.shadowenv.d` gains `.error-0-123`, written by shadowenv's hook in a shell that met it untrusted
- **THEN** the project stays trusted

#### Scenario: Trusted project runs silently
- **WHEN** the project was allowed and neither file changed
- **THEN** `devy up` shows no trust prompt

#### Scenario: Read-only commands unaffected
- **WHEN** the project was never allowed and the user runs `devy check`
- **THEN** the check runs normally

### Requirement: devy keeps trust current for its own writes
When devy itself rewrites `devy.lock` (in `devy up`) in a project that was trusted when the command started, it SHALL update the record's lock digest to the bytes it wrote. When an accepted `devy doctor` fix rewrites `devy.yml` in such a project, devy SHALL update the record's `devy.yml` and summary digests only when the fix adds or changes no executable entry compared with the config trusted at start (as compared by the config diff), and the record, `devy.lock` and the files that decide the summary are unchanged since the command started; otherwise the next gated command asks again. A project that was not trusted at command start SHALL never become trusted through these updates. `devy init` SHALL record trust for the file it writes only when the user confirmed it interactively.

#### Scenario: Lock rewrite keeps trust
- **WHEN** a trusted `devy up --update` writes a new `devy.lock`
- **THEN** the next `devy up` does not prompt
