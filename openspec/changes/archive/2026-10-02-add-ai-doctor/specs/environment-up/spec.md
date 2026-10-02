## ADDED Requirements

### Requirement: Failure record
When `devy up` (without `--dry-run`) fails after `devy.yml` has been located, devy SHALL write a failure record to `<project root>/.devy/last-up-failure.json`, creating `.devy/` if needed. The record SHALL contain:
- the full error chain, as printed on the `error:` line
- the `devy up` step that failed, when known
- the dependency being processed, when the failure is tied to one
- the platform, the selected backend and the devy version
- a UTC timestamp

Each failure SHALL replace any previous record. A `devy up` that completes successfully SHALL delete an existing record. Failing to write or delete the record SHALL NOT change `devy up`'s output or exit code beyond a single warning. Writing the record SHALL NOT make any network request. On Unix the file SHALL be readable and writable only by its owner.

#### Scenario: Failed service readiness is recorded
- **WHEN** `devy up` fails because `postgres` does not become ready
- **THEN** `.devy/last-up-failure.json` exists and names `postgres`, the service start step and the error chain

#### Scenario: Success clears the record
- **WHEN** a failure record exists and the next `devy up` prints `✓ <name> is ready`
- **THEN** `.devy/last-up-failure.json` no longer exists

#### Scenario: No project, no record
- **WHEN** `devy up` fails because no `devy.yml` is found
- **THEN** no failure record is written anywhere

#### Scenario: Dry run never records
- **WHEN** `devy up --dry-run` finds issues and exits 1
- **THEN** no failure record is written and an existing record is left unchanged

### Requirement: Doctor hint on failure
When `devy up` fails and a failure record was written, devy SHALL print `  · run devy doctor to diagnose this failure` to stderr after the `error:` line. The exit code SHALL remain 1. Printing the hint SHALL NOT make any network request or load AI configuration.

#### Scenario: Hint follows the error
- **WHEN** `devy up` fails with a hook exiting non-zero
- **THEN** stderr shows the `error:` line followed by the `devy doctor` hint, and devy exits 1

#### Scenario: Offline failure stays offline
- **WHEN** `devy up` fails on a machine with no network access and no AI configuration
- **THEN** the hint is printed and no network request is attempted
