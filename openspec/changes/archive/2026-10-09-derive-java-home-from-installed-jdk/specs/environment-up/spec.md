# Spec Delta

## MODIFIED Requirements

### Requirement: Environment composition
`devy up` SHALL build the project environment from three sources: the variables and PATH entries each module contributes, `<NAME>_HOST`/`<NAME>_PORT` variables for each service, and the `environment` map in `devy.yml`. A user-defined `environment` value SHALL override a module-provided variable with the same name. The package manager's own PATH entries SHALL come before module PATH entries, and per-package directories (brew's `<brew prefix>/opt/<formula>/bin`) SHALL come after module PATH entries; a directory listed twice is kept only at its first position. The result SHALL be written through the shell environment manager. When there is anything to write and shadowenv is not available, devy SHALL first install it through the package manager, printing `→ Installing shadowenv` and failing with `Failed to install shadowenv` if that fails. After writing, devy SHALL print `✓ Environment configured (<N> variables)`, where PATH entries are not counted, followed by the hint `Activate with: <command>`, which loads devy's shell integration: `eval "$(devy hook <shell>)"` when `<shell>` is zsh or bash, and `devy hook fish | source` for fish. `<shell>` is the basename of `$SHELL` if it is zsh, bash or fish, and otherwise `zsh`.

#### Scenario: User value wins
- **WHEN** the postgresql module sets `DATABASE_URL` and `devy.yml` `environment` also sets `DATABASE_URL`
- **THEN** the written environment uses the value from `devy.yml`

#### Scenario: Environment cleared when nothing remains
- **WHEN** no PATH entries are contributed (apt or WinGet, or brew with no installed formula's `opt/<formula>/bin` directory), a previous run wrote environment variables, and the dependencies and `environment` that produced them have since been removed
- **THEN** `devy up` clears the devy environment file and prints `✓ Environment configuration cleared`

#### Scenario: Brew formula directories are written
- **WHEN** brew is the package manager, the project declares `jq` with no `environment`, and `<brew prefix>/opt/jq/bin` exists
- **THEN** `devy up` writes the environment file with that PATH entry and prints `✓ Environment configured (0 variables)`

#### Scenario: Nix always writes the environment
- **WHEN** nix is the package manager and the project has no dependencies and no `environment`
- **THEN** `devy up` still writes the environment file with the `.devy/nix-profile/bin` PATH entry, prints `✓ Environment configured (0 variables)`, and never clears the file
