# shell-integration Specification

## Purpose
`devy hook <shell>` prints a snippet for the user's rc file. The snippet activates the shadowenv environment after `devy up` and adds tab completion for built-in subcommands and project commands.

## Requirements

### Requirement: Supported shells
The system SHALL print a shell integration snippet to stdout for `devy hook zsh`, `devy hook bash` and `devy hook fish`, exiting 0. No `devy.yml` is required. Any other shell MUST fail with `Unsupported shell '<shell>'. Supported shells: zsh, bash, fish` and a non-zero exit status.

#### Scenario: zsh snippet
- **WHEN** the user runs `devy hook zsh`
- **THEN** the zsh snippet is printed to stdout and devy exits 0

#### Scenario: Unsupported shell
- **WHEN** the user runs `devy hook powershell`
- **THEN** devy exits non-zero and stderr names `powershell`

### Requirement: Snippet uses the binary name
The system SHALL substitute the crate's binary name (`devy`) for every `{bin}` placeholder in the snippet. The output MUST NOT contain a literal `{bin}`.

#### Scenario: No placeholders remain
- **WHEN** any supported hook snippet is printed
- **THEN** the output contains no `{bin}` text

### Requirement: Wrapper activates shadowenv after up
Each snippet SHALL define a `devy` shell function that wraps the real binary. After a successful `devy up …` it MUST evaluate `shadowenv hook <shell>` in the current shell: `eval "$(shadowenv hook zsh|bash)"` for zsh and bash, `shadowenv hook fish | source` for fish. `devy hook …` MUST print the snippet for the shell it was generated for. All other invocations are passed through unchanged.

#### Scenario: Successful up activates environment
- **WHEN** the zsh snippet is loaded and `devy up` succeeds
- **THEN** the wrapper runs `eval "$(shadowenv hook zsh)"` in the current shell

#### Scenario: Failed up does not activate
- **WHEN** `devy up` exits non-zero
- **THEN** the shadowenv hook is not evaluated

### Requirement: Tab completion
Each snippet SHALL register completion for:
- the built-in subcommands `up`, `down`, `services`, `start`, `stop`, `restart`, `status`, `check`, `init`, `hook`, `pr`, and `export`
- project command names taken from `devy _commands`, with errors suppressed

It MUST complete:
- `--update`, `--dry-run`, and `--bootstrap` after `up`
- `--force` after `init`
- `--format` after `export`, and `shell flake` after `export --format`
- `zsh bash fish` after `hook`

#### Scenario: Project commands appear in completion
- **WHEN** `devy.yml` defines a command `dev` and the user tab-completes `devy <TAB>`
- **THEN** the candidates include the built-ins, including `pr` and `export`, and `dev`

#### Scenario: Hook argument completion
- **WHEN** the user tab-completes `devy hook <TAB>`
- **THEN** the candidates are `zsh`, `bash` and `fish`

#### Scenario: Up flags
- **WHEN** the user tab-completes `devy up --<TAB>`
- **THEN** the candidates are `--update`, `--dry-run`, and `--bootstrap`

#### Scenario: Export format values
- **WHEN** the user tab-completes `devy export --format <TAB>`
- **THEN** the candidates are `shell` and `flake`
