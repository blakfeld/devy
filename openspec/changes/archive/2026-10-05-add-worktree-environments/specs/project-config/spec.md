# Spec Delta

## MODIFIED Requirements

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
  - names starting with `_devy_`, `__devy_`, `__shadowenv_`, `__hookbook_` (the hook's and shadowenv's state), `__fish_` (fish's state; its theme hook sources a file under `$__fish_config_dir`) or `BASH_FUNC_` (shell functions);
  - the variables devy reads this machine's identity from for the `sh.devy.host` container label (docker-services): `WSL_DISTRO_NAME` on every platform, and on Windows only, `SystemRoot`, `windir`, `COMPUTERNAME`, `USERNAME` and `USERDOMAIN`.

  Names are compared case-sensitively and reserved only in the case a shell uses them (`histchars` and `HISTCHARS` both; `ps1` or `pwd` stay allowed). The Windows identity names are the exception: on Windows they are reserved in any case (`username`, `SYSTEMROOT`), because Windows matches environment names case-insensitively. On macOS and Linux they stay allowed, and `wsl_distro_name` stays allowed everywhere. Such a key SHALL fail with ``environment: invalid key "<KEY>" (<reason>); remove it from `environment` in devy.yml``, the reason naming its category. Variables that only change the programs the shell starts, or shells started later, stay allowed and are listed in the executable entry listing like every entry: `PATH`, zsh's `path`, fish's `fish_user_paths` (from which fish rebuilds `PATH`), `CDPATH`, `BASH_ENV`, `ENV`, `INPUTRC`, `TMPDIR`, the locale, the dynamic loader's variables (`LD_PRELOAD`, `LD_LIBRARY_PATH`, `LD_AUDIT`, `GCONV_PATH`, `LOCPATH`, `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH`, `DYLD_FRAMEWORK_PATH`, `DYLD_FALLBACK_LIBRARY_PATH`, `DYLD_FALLBACK_FRAMEWORK_PATH`, `DYLD_VERSIONED_LIBRARY_PATH`, `DYLD_VERSIONED_FRAMEWORK_PATH`, `DYLD_ROOT_PATH`), which projects use for their own programs (the shell hook's guard empties the ones that load code for its own utilities, shell-integration), and settings with no such effect such as `FIGNORE`, `HISTIGNORE`, `POSTEDIT` (printed, not expanded), `STTY` or `WORDCHARS`.

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

#### Scenario: Machine identity environment key
- **WHEN** `devy.yml` sets `environment: { WSL_DISTRO_NAME: Ubuntu }`
- **THEN** loading fails on every platform with ``environment: invalid key "WSL_DISTRO_NAME" (devy reads this machine's identity from it, to tell its containers from other machines'); remove it from `environment` in devy.yml``

#### Scenario: Windows identity key in any case
- **WHEN** `devy.yml` sets `environment: { username: app }`
- **THEN** loading fails on Windows with the machine identity reason, and succeeds on macOS and Linux

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
