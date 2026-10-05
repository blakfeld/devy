# Spec Delta

## MODIFIED Requirements

### Requirement: Tab completion
Each snippet SHALL register completion for:
- the built-in subcommands `up`, `down`, `services`, `start`, `stop`, `restart`, `status`, `check`, `doctor`, `logs`, `ask`, `init`, `hook`, `pr`, `export`, `exec` and `agent-setup`
- project command names taken from `devy _commands`, with errors suppressed

It MUST complete:
- `--update`, `--dry-run`, and `--bootstrap` after `up`
- `--force` after `init`
- `--volumes` after `down`
- `--yes`, `--no-ai` and `--show-context` after `doctor`
- `--format` after `export`, and `shell flake` after `export --format`
- `zsh bash fish` after `hook`
- `--follow`, `--lines`, `--explain`, and `--show-context` after `logs`, and service names taken from `devy _services`, with errors suppressed
- `--show-context` after `ask`
- `--json` after `status`, `services` and `check`
- `--force`, `--agents-md` and `--print` after `agent-setup`
- command names from the shell's own command completion after `exec`

#### Scenario: Project commands appear in completion
- **WHEN** `devy.yml` defines a command `dev` and the user tab-completes `devy <TAB>`
- **THEN** the candidates include the built-ins, including `doctor`, `pr`, `export`, `logs`, `ask`, `exec` and `agent-setup`, and `dev`

#### Scenario: Hook argument completion
- **WHEN** the user tab-completes `devy hook <TAB>`
- **THEN** the candidates are `zsh`, `bash` and `fish`

#### Scenario: Up flags
- **WHEN** the user tab-completes `devy up --<TAB>`
- **THEN** the candidates are `--update`, `--dry-run`, and `--bootstrap`

#### Scenario: Doctor flags
- **WHEN** the user tab-completes `devy doctor --<TAB>`
- **THEN** the candidates are `--yes`, `--no-ai` and `--show-context`

#### Scenario: Export format values
- **WHEN** the user tab-completes `devy export --format <TAB>`
- **THEN** the candidates are `shell` and `flake`

#### Scenario: Down flags
- **WHEN** the user tab-completes `devy down --<TAB>`
- **THEN** the candidates include `--volumes`

#### Scenario: Logs flags
- **WHEN** the user tab-completes `devy logs --<TAB>`
- **THEN** the candidates are `--follow`, `--lines`, `--explain`, and `--show-context`

#### Scenario: Logs service names
- **WHEN** `devy.yml` declares `redis` and `node`, and the user tab-completes `devy logs <TAB>`
- **THEN** the candidates include `redis` and not `node`

#### Scenario: JSON flag
- **WHEN** the user tab-completes `devy status --<TAB>`
- **THEN** the candidates include `--json`

#### Scenario: Agent setup flags
- **WHEN** the user tab-completes `devy agent-setup --<TAB>`
- **THEN** the candidates are `--force`, `--agents-md` and `--print`
