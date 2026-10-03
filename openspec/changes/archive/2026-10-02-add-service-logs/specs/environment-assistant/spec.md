# Spec Delta

## Purpose

Lets users ask Claude questions about their devy environment, and get a diagnosis of a failing service's logs, from a known, bounded and redacted snapshot of the project's configuration, status and service logs.

## ADDED Requirements

### Requirement: Asking about the environment
`devy ask "<question>"` SHALL send the question, together with the environment context, to Claude through the `ai-assist` capability, and SHALL print the answer to stdout, exiting 0. A missing or empty question MUST be a usage error with exit status 2. `devy ask` SHALL require a `devy.yml`, and its lookup and its error when none is found SHALL be the same as `devy status`. `devy ask` MUST NOT install, start, stop or modify anything.

#### Scenario: Question answered
- **WHEN** AI assistance is configured and the user runs `devy ask "why can't my app reach redis?"`
- **THEN** devy sends the question and the environment context, prints the answer, and exits 0

#### Scenario: Missing question
- **WHEN** the user runs `devy ask` with no argument
- **THEN** devy reports a usage error and exits 2

#### Scenario: AI assistance not configured
- **WHEN** the `ai-assist` prerequisites, such as the API key or opt-in, are not met and the user runs `devy ask "…"`
- **THEN** devy fails with the `ai-assist` error explaining how to enable it, exits 1, and makes no network request

### Requirement: Environment context contents
The environment context SHALL contain only:
- the contents of `devy.yml` and of `devy.lock` if it exists
- the operating system and the active package manager
- for each declared dependency, whether it is installed and, for services, whether it is running
- for each service, the last 50 lines of its logs, collected the same way as `devy logs`

Every item SHALL pass through the `ai-assist` redaction before it is sent. Log collection failures, such as the winget backend or an unreadable journal, SHALL be recorded in the context as a short note and MUST NOT fail the command. devy MUST NOT include any other files, environment variables of the user's shell, or shadowenv contents.

#### Scenario: Secrets are redacted
- **WHEN** `devy.yml` sets `environment.API_TOKEN: abc123` and the user runs `devy ask --show-context "…"`
- **THEN** the printed context does not contain `abc123`

#### Scenario: Logs unavailable
- **WHEN** the package manager is winget and the user runs `devy ask "…"`
- **THEN** the context notes that service logs are unavailable, and the question is still sent

### Requirement: Previewing the context
`devy ask --show-context "<question>"` and `devy logs <name> --explain --show-context` SHALL print the exact redacted context and prompt that would be sent, and exit 0, without making any network request and without requiring AI assistance to be configured.

#### Scenario: Preview without an API key
- **WHEN** no API key is configured and the user runs `devy ask --show-context "is postgres ok?"`
- **THEN** devy prints the context, including the question, and exits 0 without contacting the API

### Requirement: Explaining service logs
`devy logs <name> --explain` SHALL send the service's recent logs, using the same line count as `--lines` (default 100), along with the context for that service, to Claude through `ai-assist`. The service context is:
- its `devy.yml` entry
- its lock entry
- its running state
- its resolved port

devy SHALL print a diagnosis that names the most likely cause and the concrete steps to fix it, and exit 0. `--explain` SHALL require a service name, and MUST NOT be combined with `--follow`. Either violation MUST be a usage error with exit status 2. When the service has no log output, devy SHALL print `· No logs yet for <name>` and exit 0 without contacting the API.

#### Scenario: Explaining a crash
- **WHEN** postgres's log ends with a data directory version mismatch and the user runs `devy logs postgres --explain`
- **THEN** devy prints an explanation of the mismatch and steps to resolve it, and exits 0

#### Scenario: Explain with follow
- **WHEN** the user runs `devy logs redis --explain -f`
- **THEN** devy reports a usage error and exits 2

#### Scenario: Explain without a service
- **WHEN** the user runs `devy logs --explain`
- **THEN** devy reports a usage error and exits 2

#### Scenario: Nothing to explain
- **WHEN** redis has no log output and the user runs `devy logs redis --explain`
- **THEN** devy prints `· No logs yet for redis` and exits 0 without contacting the API

### Requirement: Assistant failures
When the request to Claude fails (network error, an authentication failure, or an API error), devy SHALL print `error: <message chain>`, where the message chain includes the `ai-assist` error, and SHALL exit 1. When the request is in progress and stdout is a terminal, devy SHALL show a progress indicator, and it MUST write nothing but the answer to stdout when stdout is not a terminal.

#### Scenario: API unreachable
- **WHEN** AI assistance is configured but the API cannot be reached and the user runs `devy ask "…"`
- **THEN** devy prints an `error:` line that describes the failed request and exits 1

#### Scenario: Piped output
- **WHEN** the user runs `devy ask "…" > answer.md`
- **THEN** `answer.md` contains only the answer text
