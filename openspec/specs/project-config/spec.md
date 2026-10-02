# project-config Specification

## Purpose
Defines how devy finds and parses the project's `devy.yml`, the schema of that file, and how `devy init` creates a starter config.

## Requirements
### Requirement: Config discovery
devy SHALL find `devy.yml` by starting in the current directory and walking up through parent directories. It SHALL return the first `devy.yml` it finds, and SHALL stop searching after checking a directory that contains `.git` or that is the user's home directory. The directory containing the found `devy.yml` SHALL be the project root.

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
Each entry in `commands` SHALL be either a string command line or a map with a required `cmd` and optional `cwd` and `shell`. When `shell` is omitted it SHALL default to `sh`, or to `cmd` on Windows. Unknown keys inside a command map SHALL be silently ignored, not rejected. The same applies to configured hook entries.

#### Scenario: String command
- **WHEN** `commands.dev` is `"npm run dev"`
- **THEN** `devy dev` runs `npm run dev` with the default shell

#### Scenario: Configured command
- **WHEN** `commands.migrate` is `{ cmd: "rails db:migrate", cwd: ./api, shell: bash }`
- **THEN** `devy migrate` runs the command with `bash` in `./api`

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
`devy init` SHALL write `devy.yml` in the current directory and print `✓ wrote devy.yml`. With no mode flag it SHALL write `name: my-project` and `dependencies: []`, exactly as before. With `--detect` or `--ai` it SHALL write a generated config as defined by the detected-init and AI-init requirements. `--detect` and `--ai` SHALL be mutually exclusive, and `--show-context` SHALL only be accepted together with `--ai`. Passing either invalid combination SHALL be a usage error that exits 2. If `devy.yml` already exists there, every mode SHALL fail with `devy.yml already exists. Use --force to overwrite.` before scanning the project or running `claude`, unless `--force` is given, in which case it SHALL overwrite the file. `init` SHALL NOT search parent directories.

#### Scenario: Fresh init
- **WHEN** the user runs `devy init` in a directory without `devy.yml`
- **THEN** `devy.yml` is created with a `dependencies` key and the command exits 0

#### Scenario: Existing file without force
- **WHEN** `devy.yml` already exists and the user runs `devy init`
- **THEN** the command exits non-zero, stderr mentions that the file already exists and `--force`, and the file is unchanged

#### Scenario: Existing file with force
- **WHEN** `devy.yml` already exists and the user runs `devy init --force`
- **THEN** the file is replaced with the starter content

#### Scenario: Existing file blocks AI before any request
- **WHEN** `devy.yml` already exists and the user runs `devy init --ai` without `--force`
- **THEN** the command fails with the already-exists error, does not run `claude`, and the file is unchanged

#### Scenario: Conflicting modes
- **WHEN** the user runs `devy init --detect --ai`
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
- Service dependencies from `docker-compose.yml` / `compose.yaml` services whose image names a devy service module (for example `postgres`, `mysql`, `mariadb`, `redis`, `mongo`, `rabbitmq`, `elasticsearch`, `opensearch`, `memcached`, `minio`, `mailhog`, `meilisearch`, `vault`, `kafka`). The version comes from the image tag when it is a version.
- `commands` from `package.json` `scripts`, as `<pm> run <script>` using the package manager its lockfile indicates.
- `environment` from `.env.example` keys. A value that names the host or port of a detected service SHALL be rewritten to use that service's injected `*_HOST` / `*_PORT` variables. Any other value SHALL be kept only if it is not secret-looking under the ai-assist redaction rule, and otherwise left empty with a `# TODO: set` comment.

Each dependency SHALL use devy's canonical module name, or an alias that resolves to it. The written file SHALL begin with a comment `# Generated by devy init --detect — review before committing`. Detected inputs that could not be mapped SHALL appear as `# TODO:` comments, and the file SHALL load without error under the existing config rules.

#### Scenario: Node project with compose services
- **WHEN** the directory has `.nvmrc` containing `22`, a `package.json` with a `dev` script and `package-lock.json`, and a `docker-compose.yml` with `postgres:16` and `redis:7` services
- **THEN** `devy.yml` lists `node` with version `22`, `postgresql` with version `16` and `redis` with version `7`, defines `commands.dev` as `npm run dev`, and `devy check` reports no config parse errors or unrecognized keys

#### Scenario: Env example rewritten to injected vars
- **WHEN** `.env.example` contains `DATABASE_URL=postgres://localhost:5432/app` and postgres was detected
- **THEN** `environment.DATABASE_URL` is `postgres://${POSTGRESQL_HOST}:${POSTGRESQL_PORT}/app`

#### Scenario: Nothing detected
- **WHEN** the directory contains no recognized files
- **THEN** devy writes the same content as plain `devy init`, preceded by the generated-by comment, and exits 0

#### Scenario: Works offline
- **WHEN** the machine has no network access and the user runs `devy init --detect`
- **THEN** the command succeeds

### Requirement: AI init
`devy init --ai` SHALL run the detected-init scan, then send to the model, under the ai-assist rules: the detected draft, the redacted contents of the recognized project files (plus `Makefile`, `Procfile`, `README.md` and `.github/workflows/*.yml`), and a catalog of devy's dependency modules (canonical names, aliases, whether each is a service, default port, injected variables and accepted extra keys). It SHALL ask for a complete `devy.yml`.

Before writing, devy SHALL validate the reply without installing anything. The reply must:
- parse under the existing config rules
- produce no unrecognized-extra-key issues
- produce no invalid-shell issues
- produce no conflicts between explicit ports

If validation fails, devy SHALL send exactly one follow-up request that includes the validation errors. If that reply also fails validation, devy SHALL write nothing, print the remaining validation errors, suggest `devy init --detect`, and exit 1.

On success, devy SHALL write the validated YAML with the header comment `# Generated by devy init --ai (<model>) — AI-generated, review before committing`, and SHALL print any module config warnings for the result.

#### Scenario: Valid first reply
- **WHEN** the model's first reply is a valid config
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
