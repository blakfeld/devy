# Spec Delta

## ADDED Requirements

### Requirement: Completion treats project data literally
Shell snippets SHALL treat names from `devy _commands` and `devy _services` as literal strings. They SHALL never be subject to parameter expansion, command substitution, arithmetic expansion or globbing. The bash snippet SHALL NOT pass them through `compgen -W` or any other construct that re-evaluates words. It SHALL build `COMPREPLY` by a literal prefix match over the lines read from those commands. Names containing whitespace or control characters SHALL be skipped.

#### Scenario: Substitution in a command name is not run
- **WHEN** `devy _commands` prints `$(touch /tmp/p)` (for example from an older devy or a crafted config) and the user tab-completes `devy <TAB>` in bash
- **THEN** `/tmp/p` is not created

#### Scenario: Normal completion still works
- **WHEN** `devy.yml` defines a command `dev` and the user tab-completes `devy d<TAB>` in bash
- **THEN** the candidates include `dev`, `down` and `doctor`

## MODIFIED Requirements

### Requirement: Tab completion
Each snippet SHALL register completion for:
- the built-in subcommands `up`, `down`, `services`, `start`, `stop`, `restart`, `status`, `check`, `doctor`, `logs`, `ask`, `init`, `hook`, `pr`, `export`, and `allow`
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
- `--revoke` after `allow`

#### Scenario: Project commands appear in completion
- **WHEN** `devy.yml` defines a command `dev` and the user tab-completes `devy <TAB>`
- **THEN** the candidates include the built-ins, including `doctor`, `pr`, `export`, `logs`, `ask` and `allow`, and `dev`

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

#### Scenario: Allow flags
- **WHEN** the user tab-completes `devy allow --<TAB>`
- **THEN** the candidates are `--revoke`
