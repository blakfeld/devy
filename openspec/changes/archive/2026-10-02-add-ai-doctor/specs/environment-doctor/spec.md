# Spec Delta

## Purpose

Defines `devy doctor`, which diagnoses a project's environment and its most recent `devy up` failure. It combines devy's deterministic checks with an optional AI explanation and can apply a suggested `devy.yml` fix the user confirms.

## ADDED Requirements

### Requirement: Doctor invocation
`devy doctor` SHALL accept the flags `--yes`, `--no-ai` and `--show-context`. It SHALL fail with `error: devy.yml not found — are you inside a devy project?` and exit 1 when run outside a devy project. It SHALL begin by printing the header `devy doctor · <name>`, where `<name>` is the `devy.yml` `name`, or `project` when it is unset.

#### Scenario: Header
- **WHEN** `devy.yml` has `name: shop` and the user runs `devy doctor`
- **THEN** the output begins with `devy doctor · shop`

#### Scenario: Outside a project
- **WHEN** `devy doctor` runs in a directory with no `devy.yml` above it
- **THEN** it exits 1 and stderr mentions `devy.yml`

### Requirement: Doctor does not change the environment
`devy doctor` SHALL NOT install dependencies, start or stop services, run hooks or project commands, or write `devy.lock`, the environment file or the failure record. The only file it MAY write is `devy.yml`, and only through an accepted fix.

#### Scenario: Stopped service is left stopped
- **WHEN** a declared service is installed but stopped and the user runs `devy doctor`
- **THEN** the service is reported as stopped and is still stopped afterwards

### Requirement: Deterministic findings
`devy doctor` SHALL always evaluate the same issues and warnings that `devy check` evaluates, and print them under a `Checks` header. If a failure record exists, it SHALL also print, under a `Last devy up failure` header:
- the record's timestamp
- the failed step
- the dependency, when present
- the error chain

A hard configuration error that would make `devy check` fail SHALL be reported as a finding rather than ending `devy doctor`. Examples are an unloadable `devy.yml` or a port conflict.

#### Scenario: Recorded failure is shown
- **WHEN** `.devy/last-up-failure.json` records that `redis` failed its readiness check
- **THEN** `devy doctor` prints the `Last devy up failure` section naming `redis` and the error chain

#### Scenario: Invalid YAML is a finding
- **WHEN** `devy.yml` exists but contains invalid YAML
- **THEN** `devy doctor` reports the parse error as a finding and continues to the diagnosis step, instead of exiting with only an `error:` line

#### Scenario: Healthy project
- **WHEN** no failure record exists and `devy check` would report no issues
- **THEN** `devy doctor` prints `✓ no problems found`, makes no AI request and exits 0

### Requirement: AI diagnosis
When at least one finding or a failure record exists, AI assistance is available as defined by `ai-assist`, and `--no-ai` is not given, `devy doctor` SHALL request a diagnosis from the model. The request SHALL include only:
- the failure record
- the deterministic findings
- the contents of `devy.yml` and `devy.lock`
- the platform, backend and devy version
- up to the last 50 log lines of each service that is named in the failure record or reported stopped or not ready, as retrieved through `service-logs`

All content sent SHALL pass through the redaction and size caps of `ai-assist`. Logs for services with none available are noted as `no logs available` rather than failing the request. The diagnosis SHALL be printed under a `Diagnosis` header labelled as AI-generated with the model `claude` reports having used, as `ai-assist` requires. It SHALL contain a one-paragraph summary, the likely cause, and a numbered list of suggested steps. Suggested shell commands SHALL be printed for the user to run and SHALL NOT be executed by devy.

#### Scenario: Diagnosis for a failed up
- **WHEN** a failure record exists, the `claude` CLI is on `PATH` and the user runs `devy doctor`
- **THEN** devy prints the deterministic findings followed by a `Diagnosis` section, labelled with the model, with a summary, a likely cause and numbered steps

#### Scenario: Suggested commands are not run
- **WHEN** the diagnosis suggests running `nix-collect-garbage`
- **THEN** devy prints the command and does not execute it

#### Scenario: Logs come only from affected services
- **WHEN** `postgres` failed and `redis` is running normally
- **THEN** the request includes recent `postgres` log lines and no `redis` log lines

### Requirement: Doctor context preview
`devy doctor --show-context` SHALL print the deterministic findings and then the complete, already-redacted request that a diagnosis would send, as defined by `ai-assist`'s context preview, and exit 0. It SHALL NOT require or run `claude`, prompt, or write any file. When there is nothing to diagnose it SHALL print `✓ no problems found` and no request.

#### Scenario: Preview without claude
- **WHEN** a failure record exists, `claude` is not on `PATH` and the user runs `devy doctor --show-context`
- **THEN** devy prints the request content, exits 0 and does not run `claude`

### Requirement: Doctor without AI
When AI assistance is unavailable or `--no-ai` is given, `devy doctor` SHALL print the deterministic findings and then `· AI diagnosis unavailable — <reason>`. `<reason>` is `disabled with --no-ai` or the `ai-assist` explanation of how to enable it (the `claude` CLI was not found on `PATH`). Unlike other AI commands, a missing `claude` CLI SHALL NOT make `devy doctor` exit 1. The command SHALL make no network request. If an AI request is attempted and fails, devy SHALL warn `AI diagnosis failed: <cause>` and continue as if AI were unavailable.

#### Scenario: No claude CLI
- **WHEN** `claude` is not on `PATH` and a failure record exists
- **THEN** devy prints the findings and an `AI diagnosis unavailable` line explaining how to enable it, makes no network request, and exits 0

#### Scenario: Request error
- **WHEN** the AI request times out
- **THEN** devy warns `AI diagnosis failed: …` on stderr, still prints the deterministic findings, and exits 0

### Requirement: Suggested configuration fix
When the diagnosis proposes a change to `devy.yml`, devy SHALL validate the proposed file before offering it. The file must load as a `devy.yml` and pass the configuration validation that `devy check` treats as hard errors, without counting install, service or environment state. A proposal that fails validation SHALL be discarded with the warning `suggested devy.yml change was invalid and was not offered: <cause>`.

A valid proposal SHALL be printed as a unified diff against the current `devy.yml` under a `Suggested fix` header. devy SHALL then:
- with `--yes`, write it
- when stdin is a terminal, ask `Apply this change to devy.yml? [y/N]` and write it only on `y` or `yes`, case-insensitively
- otherwise, print `· not applied — re-run with --yes to apply` and leave `devy.yml` unchanged

An applied change SHALL replace `devy.yml` atomically and print `✓ updated devy.yml — run devy up to apply it`. devy SHALL NOT run `devy up` itself.

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

### Requirement: Doctor exit codes
`devy doctor` SHALL exit 0 when it completes, whether or not problems were found or a fix was applied. It SHALL exit 1 with an `error:` line only when it cannot run at all, for example outside a devy project or when writing an accepted `devy.yml` fails. Usage errors SHALL exit 2.

#### Scenario: Problems found still exit 0
- **WHEN** `devy doctor` reports two issues and no fix is applied
- **THEN** the exit code is 0

#### Scenario: Write failure
- **WHEN** the user accepts a fix and `devy.yml` is not writable
- **THEN** devy prints `error: …` naming `devy.yml` and exits 1
