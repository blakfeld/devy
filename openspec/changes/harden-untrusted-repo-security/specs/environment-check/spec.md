# Spec Delta

## MODIFIED Requirements

### Requirement: Status report
`devy status` SHALL print the header `devy status · <name>`. It SHALL then print a dependency table showing installed state and service running state, and an environment table showing each configured variable's written value, `(not set)` if it is missing from the environment file, or `(not configured)` if no environment file exists. A written value whose key matches the ai-assist redaction key rule SHALL be shown as `<redacted>`. When modules contribute PATH entries, it SHALL also print a PATH table that marks each entry `✓` if written, `(not set)` if missing, or `(not configured)` if no environment file exists. As in `devy check`, the package manager's own PATH entry is not listed. Missing or stopped items SHALL NOT make `devy status` fail. It SHALL exit 0 unless the configuration, package manager or a state query fails, and it SHALL NOT write any files.

#### Scenario: Status before up
- **WHEN** `devy status` runs before any `devy up`
- **THEN** configured variables show `(not configured)` and the command exits 0

#### Scenario: Status after up
- **WHEN** `devy up` has written `LOG_LEVEL=debug`
- **THEN** `devy status` shows `LOG_LEVEL` with the value `debug`

#### Scenario: Secret values masked
- **WHEN** `devy up` has written `API_TOKEN=abc`
- **THEN** `devy status` shows `API_TOKEN` as `<redacted>` and does not print `abc`
