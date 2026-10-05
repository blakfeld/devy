# Spec Delta

## MODIFIED Requirements

### Requirement: Suggested configuration fix
When the diagnosis proposes a change to `devy.yml`, devy SHALL validate the proposed file before offering it. The file must load as a `devy.yml` and pass the configuration validation that `devy check` treats as hard errors, without counting install, service or environment state. A proposal that fails validation, or that contains terminal control characters, SHALL be discarded with the warning `suggested devy.yml change was invalid and was not offered: <cause>`.

A valid proposal SHALL be printed as a unified diff against the current `devy.yml` under a `Suggested fix` header. If it adds or changes any executable field (any `hooks` entry, `after_install`, `install_cmd`, `tap`, `image`, `commands` entry, a dependency installed through the package manager (added, or with a changed version), or an execution-affecting `environment` key as listed in project-trust), devy SHALL list those entries under `This change adds or alters commands devy will run`. devy SHALL then:
- with `--yes`, write it only if it changes no executable field; otherwise print `· not applied — this fix changes commands devy runs; review it and re-run without --yes` and leave `devy.yml` unchanged
- when stdin is a terminal, ask `Apply this change to devy.yml? [y/N]` and write it only on `y` or `yes`, case-insensitively
- otherwise, print `· not applied — re-run with --yes to apply` and leave `devy.yml` unchanged

An applied change SHALL replace `devy.yml` atomically without following symlinks and print `✓ updated devy.yml — run devy up to apply it`. Whether the project stays trusted SHALL follow project-trust ("devy keeps trust current for its own writes"): the record is refreshed only when the project was trusted at command start and the fix adds or changes no executable entry (including system packages); otherwise the next gated command asks again. devy SHALL NOT run `devy up` itself.

A `devy.yml` that is a symlink or not a regular file in the project SHALL still get the offline checks (read as other commands read it); only sending it to claude SHALL be refused, with an error naming `devy doctor --no-ai`.

#### Scenario: User accepts the fix
- **WHEN** the diagnosis proposes changing the `mysql` port, the change validates, and the user answers `y`
- **THEN** `devy.yml` contains the new port and devy prints `✓ updated devy.yml — run devy up to apply it`

#### Scenario: Default answer declines
- **WHEN** the user presses Enter at the prompt
- **THEN** `devy.yml` is unchanged

#### Scenario: Non-interactive stdin
- **WHEN** `devy doctor` runs with stdin not a terminal and without `--yes`
- **THEN** the diff is printed, `devy.yml` is unchanged, and devy prints the `--yes` hint

#### Scenario: Invalid suggestion is discarded
- **WHEN** the proposed `devy.yml` lists a dependency entry with two keys
- **THEN** devy warns that the suggestion was invalid, shows no diff and does not prompt

#### Scenario: --yes never adds a hook
- **WHEN** the proposal adds `hooks.before_up` and the user runs `devy doctor --yes`
- **THEN** `devy.yml` is unchanged and devy prints the review hint
