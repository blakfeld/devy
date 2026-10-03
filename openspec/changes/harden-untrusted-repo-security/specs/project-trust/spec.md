# Spec Delta

## Purpose

Makes running repository-defined code an explicit user decision, so cloning a repo and running devy cannot execute that repo's hooks, install scripts or shell environment until the user has reviewed and allowed them.

## ADDED Requirements

### Requirement: Trust records
devy SHALL keep a per-user trust store outside any project, at `$XDG_STATE_HOME/devy/trust/` (default `~/.local/state/devy/trust/`) on macOS and Linux and `%LOCALAPPDATA%\devy\trust\` on Windows. The store directory and its files SHALL be created with mode 0700 and 0600 respectively. A trust record SHALL hold the canonical project root and a SHA-256 digest of the bytes of `devy.yml` and of `devy.lock` (an absent lock hashes as empty). A project SHALL be trusted only when a record exists for its canonical root and both digests match the current files.

#### Scenario: Edited config invalidates trust
- **WHEN** a project was allowed and `devy.yml` is then changed by `git pull`
- **THEN** the project is no longer trusted until the user allows it again

#### Scenario: Same content at another path
- **WHEN** an allowed project's files are copied to a different directory
- **THEN** the copy is not trusted

### Requirement: Allow command
devy SHALL provide `devy allow`, which loads `devy.yml`, prints the trust summary, and records trust for the current project without prompting. On success it SHALL print `✓ allowed <project_root>`. `devy allow --revoke` SHALL delete the record and print `✓ revoked trust for <project_root>`, and SHALL succeed when no record exists. `devy allow` SHALL fail like other commands when `devy.yml` is missing or invalid.

#### Scenario: Allow then up in CI
- **WHEN** a CI job runs `devy allow` and then `devy up` with stdin not a terminal
- **THEN** `devy up` runs without prompting

#### Scenario: Revoke
- **WHEN** the user runs `devy allow --revoke` in an allowed project
- **THEN** the next `devy up` prompts for trust again

### Requirement: Trust summary
Before trust is granted, interactively or by `devy allow`, devy SHALL print what the project will run, grouped under `Hooks`, `Install commands`, `Project setup`, `Package sources` and `Environment`:
- every hook command
- every `after_install` and `install_cmd`
- each implicit project setup step and the file that drives it (for example `npm install (package.json lifecycle scripts)`, `bundle install (Gemfile)`, `pip install -e . (pyproject.toml)`, `./gradlew (repository script)`)
- every `tap` and `image`
- every `environment` key that changes how programs run: `PATH`, any key starting with `LD_`, `DYLD_`, `NODE_OPTIONS`, `RUBYOPT`, `PYTHONPATH`, `PYTHONSTARTUP`, `PERL5OPT`, `GIT_SSH_COMMAND`, `GIT_EXEC_PATH`, `BASH_ENV`, `ENV`, `PROMPT_COMMAND`, `EDITOR`, `PAGER`

Values SHALL be printed with control characters stripped. Groups with no entries SHALL be omitted.

#### Scenario: Summary lists hooks and setup
- **WHEN** `devy.yml` declares `node` with a `package.json` present and `hooks.after_up: "make seed"`
- **THEN** the summary lists `make seed` under `Hooks` and `npm install (package.json lifecycle scripts)` under `Project setup`

### Requirement: Trust gate on commands that run project code
`devy up`, `devy down`, `devy start` and `devy restart` SHALL check trust after loading `devy.yml` and before running any hook, installing anything, starting any service or writing any file. When the project is not trusted:
- if stdin and stdout are terminals, devy SHALL print `This project has not been allowed` (or `devy.yml or devy.lock changed since this project was allowed`), print the trust summary, and ask `Allow and continue? [y/N]`. On `y` or `yes` (case-insensitive) it SHALL record trust and continue; otherwise it SHALL exit 1 without side effects.
- otherwise devy SHALL fail with `project is not allowed — review devy.yml and run devy allow` and exit 1.

`devy check`, `devy status`, `devy services`, `devy logs`, `devy ask`, `devy doctor`, `devy export`, `devy init`, `devy hook`, `devy pr`, `devy stop`, `_commands`, `_services`, and explicitly invoked project commands SHALL NOT require trust.

#### Scenario: Untrusted repo in a terminal, declined
- **WHEN** the user clones a repo with `hooks.before_up: "touch /tmp/pwned"`, runs `devy up` in a terminal and presses Enter
- **THEN** `/tmp/pwned` does not exist, nothing is installed, and devy exits 1

#### Scenario: Untrusted repo without a terminal
- **WHEN** `devy up` runs with stdin not a terminal in a project that was never allowed
- **THEN** devy prints `project is not allowed — review devy.yml and run devy allow` and exits 1

#### Scenario: Trusted project runs silently
- **WHEN** the project was allowed and neither file changed
- **THEN** `devy up` shows no trust prompt

#### Scenario: Read-only commands unaffected
- **WHEN** the project was never allowed and the user runs `devy check`
- **THEN** the check runs normally

### Requirement: devy keeps trust current for its own writes
When devy itself rewrites `devy.lock` (in `devy up`) or `devy.yml` (an accepted `devy doctor` fix) in a trusted project, it SHALL update the trust record's digests to match the written content. `devy init` SHALL record trust for the file it writes only when the user confirmed it interactively.

#### Scenario: Lock rewrite keeps trust
- **WHEN** a trusted `devy up --update` writes a new `devy.lock`
- **THEN** the next `devy up` does not prompt
