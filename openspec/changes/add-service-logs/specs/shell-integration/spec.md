## MODIFIED Requirements

### Requirement: Tab completion
Each snippet SHALL register completion for:
- the built-in subcommands `up`, `down`, `services`, `start`, `stop`, `restart`, `status`, `check`, `init`, `hook`, `pr`, `export`, `logs`, and `ask`
- project command names taken from `devy _commands`, with errors suppressed

It MUST complete:
- `--update`, `--dry-run`, and `--bootstrap` after `up`
- `--force` after `init`
- `--format` after `export`, and `shell flake` after `export --format`
- `zsh bash fish` after `hook`
- `--follow`, `--lines`, `--explain`, and `--show-context` after `logs`
- `--show-context` after `ask`

#### Scenario: Project commands appear in completion
- **WHEN** `devy.yml` defines a command `dev` and the user tab-completes `devy <TAB>`
- **THEN** the candidates include the built-ins, including `pr`, `export`, `logs` and `ask`, and `dev`

#### Scenario: Hook argument completion
- **WHEN** the user tab-completes `devy hook <TAB>`
- **THEN** the candidates are `zsh`, `bash` and `fish`

#### Scenario: Up flags
- **WHEN** the user tab-completes `devy up --<TAB>`
- **THEN** the candidates are `--update`, `--dry-run`, and `--bootstrap`

#### Scenario: Export format values
- **WHEN** the user tab-completes `devy export --format <TAB>`
- **THEN** the candidates are `shell` and `flake`

#### Scenario: Logs flags
- **WHEN** the user tab-completes `devy logs --<TAB>`
- **THEN** the candidates are `--follow`, `--lines`, `--explain`, and `--show-context`
