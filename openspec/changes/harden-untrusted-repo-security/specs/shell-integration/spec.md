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
