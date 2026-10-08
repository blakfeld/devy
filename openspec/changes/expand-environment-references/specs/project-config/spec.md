## MODIFIED Requirements

### Requirement: Environment map
`environment` SHALL be a map of string variable names to string values that devy exports into the project's shell environment. Each value SHALL be expanded as described in Environment references before devy exports it.

#### Scenario: Environment variables declared
- **WHEN** `devy.yml` sets `environment: { LOG_LEVEL: debug }`
- **THEN** `LOG_LEVEL=debug` is part of the project environment that `devy up` writes

#### Scenario: Connection string built from injected variables
- **WHEN** `devy.yml` lists `mysql`, `devy.lock` assigns it port 52114, and `environment` sets `DATABASE_URL: "mysql://root@${MYSQL_HOST}:${MYSQL_PORT}/app"`
- **THEN** `DATABASE_URL` in the project environment is `mysql://root@127.0.0.1:52114/app`

## ADDED Requirements

### Requirement: Environment references
In an `environment` value, `${NAME}` SHALL be replaced by the value of `NAME` in the project environment, where `NAME` matches `^[A-Za-z_][A-Za-z0-9_]*$`. The project environment means the module variables, the `<SERVICE>_HOST` and `<SERVICE>_PORT` variables, and the other `environment` entries, with `environment` winning for a name both define. The following rules apply:
- A referenced `environment` entry SHALL be expanded before its value is substituted.
- Module variable values SHALL be used as they are.
- Substituted text SHALL NOT be scanned again.
- `$${` SHALL produce a literal `${`.
- Any `$` that does not start `${` or `$${` SHALL be kept as it is.
- References SHALL NOT be resolved against the environment devy itself runs in.

Loading `devy.yml` SHALL fail when a value contains a `${` that is not followed by a valid name and `}`. The error SHALL be ``environment.<KEY>: invalid reference "<text>"; write $${ for a literal ${``, with control characters escaped.

Computing the project environment SHALL fail in these cases:
- A reference names something the project environment does not define. The error SHALL be ``environment.<KEY>: ${<NAME>} is not defined``.
- References form a cycle, including a self-reference. The error SHALL name the keys in the cycle.
- A reference names the `<SERVICE>_PORT` of a service whose port is not assigned yet. The error SHALL say to run `devy up`.

#### Scenario: Reference to another environment entry
- **WHEN** `environment` sets `DB_HOST: db.local` and `DATABASE_URL: "postgres://${DB_HOST}/app"`
- **THEN** `DATABASE_URL` is `postgres://db.local/app`

#### Scenario: Escaped reference
- **WHEN** `environment` sets `TEMPLATE: "hi $${USER}"`
- **THEN** `TEMPLATE` is `hi ${USER}`

#### Scenario: Bare dollar kept
- **WHEN** `environment` sets `PASSWORD: "pa$word"`
- **THEN** `PASSWORD` is `pa$word`

#### Scenario: Malformed reference
- **WHEN** `environment` sets `URL: "http://${HOST"`
- **THEN** loading `devy.yml` fails with an error naming `environment.URL` and suggesting `$${`

#### Scenario: Undefined reference
- **WHEN** `environment` sets `URL: "http://${NOPE}/"` and nothing defines `NOPE`
- **THEN** `devy up` fails with `environment.URL: ${NOPE} is not defined` and writes no environment file

#### Scenario: Host environment not consulted
- **WHEN** devy runs with `HOME=/Users/me` and `environment` sets `CACHE: "${HOME}/.cache"`
- **THEN** computing the project environment fails with `environment.CACHE: ${HOME} is not defined`

#### Scenario: Reference cycle
- **WHEN** `environment` sets `A: "${B}"` and `B: "${A}"`
- **THEN** computing the project environment fails with an error naming `A` and `B`
