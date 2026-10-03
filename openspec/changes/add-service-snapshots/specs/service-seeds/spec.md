# Spec Delta

## Purpose

Lets a project declare starter data for its services in `devy.yml`. devy loads that data once, after the service first becomes healthy, so every teammate starts from the same usable dataset. AI generation of seed data from a schema is optional.

## ADDED Requirements

### Requirement: Seed configuration
Every service module SHALL accept the extra key `seed`, in addition to its other keys. `seed` SHALL take one of three forms:
- **A file path string:** relative paths are resolved against the project root.
- **A non-empty list of file path strings:** devy applies them in order.
- **A command map:** `cmd` is required, and `shell` and `cwd` are optional, with the same meaning and defaults as in hook and project-command definitions.

Non-service modules SHALL keep rejecting `seed` through their existing key allowlists.

#### Scenario: File seed accepted
- **WHEN** `devy.yml` declares `- postgres: { seed: ./db/seed.sql }`
- **THEN** `devy check` does not report `seed` as an unrecognized key

#### Scenario: Seed on a non-service
- **WHEN** `devy.yml` declares `- node: { seed: ./x.sql }`
- **THEN** `devy check` reports `seed` as an unrecognized key for `node`

### Requirement: File seed loaders
File seeds SHALL be supported only by these modules. Each one loads the file over `127.0.0.1` on the service's resolved port, using the client tool named below from devy's PATH, which includes the project's nix profile:

| Module | Accepted file | How devy loads it |
|---|---|---|
| `postgresql` | `.sql` | `psql` against database `postgres`, stopping at the first error |
| `mysql`, `mariadb` | `.sql` | `mysql` as user `root`, with the file on stdin |
| `redis` | any extension | `redis-cli`, one command per line, with the file on stdin |
| `mongodb` | `.js` | `mongosh` against the server |

When a loader's client tool cannot be found, devy SHALL fail with `<dep>: seeding needs <tool> on PATH`. For `mongodb`, the message SHALL add `— add mongosh to dependencies`.

Every other service module SHALL support only the command form. A file seed on such a module, or a file whose extension its module doesn't accept, SHALL be a configuration issue (see "Seed validation in check").

#### Scenario: Postgres SQL seed
- **WHEN** `postgres` declares `seed: ./db/seed.sql` and the file creates a table
- **THEN** after `devy up` the table exists in database `postgres`

#### Scenario: Seed error stops the load
- **WHEN** a postgres seed file has a syntax error on its second statement
- **THEN** seeding fails and devy reports `Failed to seed postgresql` with psql's error output

#### Scenario: Mongo without mongosh
- **WHEN** `mongodb` declares `seed: ./seed.js` and `mongosh` is not on PATH
- **THEN** seeding fails with `mongodb: seeding needs mongosh on PATH — add mongosh to dependencies`

### Requirement: Command seeds
A command seed SHALL run with the project's resolved environment, including `<SERVICE>_HOST`, `<SERVICE>_PORT`, module variables such as `DATABASE_URL`, and the `environment:` map, all layered over devy's own environment. A non-zero exit SHALL fail seeding with an error that includes the exit status.

#### Scenario: Command seed sees service port
- **WHEN** `redis` declares `seed: { cmd: "redis-cli -p $REDIS_PORT SET hello world" }` and was assigned port 6400
- **THEN** after `devy up`, key `hello` exists in the redis listening on 6400

### Requirement: Seeds apply once per service data
devy SHALL record a seed as applied only after it succeeds, along with a fingerprint of the seed's definition and the contents of its files. It SHALL NOT apply a recorded seed again unless asked with `--force`. Where the record lives depends on who owns the data:
- **nix backend:** the record SHALL live inside the service's data directory (`.devy/data/<canonical-name>/`). Deleting the data directory, or restoring a snapshot, therefore carries the matching seed state with it.
- **Other backends:** the record SHALL live in `.devy/state/seeds/<canonical-name>.json`.

The seed record MUST NOT be written to `devy.lock`.

#### Scenario: Second up skips seeding
- **WHEN** postgres was seeded during a previous `devy up`
- **THEN** the next `devy up` prints `○ postgresql already seeded` and does not run `psql`

#### Scenario: Wiping data re-seeds under nix
- **WHEN** the user stops postgres, deletes `.devy/data/postgresql/` and runs `devy up`
- **THEN** devy initializes a new cluster and applies the seed again

### Requirement: Seed drift warning
When a seed has already been applied but its current fingerprint differs from the recorded one, devy SHALL print `<dep>: seed changed since it was applied — run devy seed <dep> --force to re-apply` and SHALL NOT re-apply it.

#### Scenario: Seed file edited
- **WHEN** postgres was seeded and the user then edits `db/seed.sql` and runs `devy up`
- **THEN** devy prints the seed-changed warning and leaves the database unchanged

### Requirement: Seed command
`devy seed [<service>]... [--force]` SHALL apply the seeds of the named services, or of every service dependency that declares `seed` when no names are given.
- It SHALL apply only seeds that are not yet recorded. With `--force`, it SHALL also apply recorded ones, and record them again on success.
- Each target service MUST be running and pass a health check. Otherwise, devy SHALL fail with `<dep> is not running — start it with devy start <dep>`.
- Naming a service that declares no seed SHALL fail with `<dep> has no seed configured`.
- When no service declares a seed, devy SHALL print `○ no seeds configured`.
- The command SHALL take the same `.devy-lock` advisory lock as `devy up`.

#### Scenario: Force re-seed
- **WHEN** postgres is running and already seeded, and the user runs `devy seed postgres --force`
- **THEN** devy runs the seed again and records the new fingerprint

#### Scenario: Seed a stopped service
- **WHEN** postgres is stopped and the user runs `devy seed postgres`
- **THEN** devy fails with `postgresql is not running — start it with devy start postgresql`

### Requirement: Seed validation in check
`devy check` SHALL count each of these as an issue:
- a `seed` value that matches none of the accepted forms
- a seed file that does not exist
- a file seed on a module that supports only commands, reported as `<dep>: seed files are not supported — use seed: { cmd: … }`
- a file extension the module does not accept, reported as `<dep>: seed file <path> must end in <ext>`

#### Scenario: Missing seed file
- **WHEN** `postgres` declares `seed: ./missing.sql` and the file does not exist
- **THEN** `devy check` reports the missing file as an issue and exits 1

#### Scenario: File seed on kafka
- **WHEN** `kafka` declares `seed: ./topics.txt`
- **THEN** `devy check` reports `kafka: seed files are not supported — use seed: { cmd: … }`

### Requirement: AI-generated seed data
`devy seed --generate <service> [--out <path>]` SHALL be supported for `postgresql`, `mysql` and `mariadb`.
- devy SHALL read the running service's schema without row data, then request INSERT statements that fit that schema through the shared `ai-assist` capability, under that capability's opt-in, credential and redaction rules.
- devy SHALL write the result to `<path>`, or to `db/seed.generated.sql` when `--out` is absent. It SHALL fail rather than overwrite an existing file.
- devy SHALL print the `seed:` setting that would use the file. It MUST NOT execute the generated SQL.
- Data from table rows MUST NOT be sent.
- `--generate` on any other module SHALL fail with `<dep>: --generate supports postgresql, mysql and mariadb`.

#### Scenario: Generate seed for postgres
- **WHEN** AI assistance is enabled, postgres is running with tables `users` and `orders`, and the user runs `devy seed --generate postgres`
- **THEN** devy writes `db/seed.generated.sql` with INSERT statements for those tables
- **AND** the database is unchanged

#### Scenario: AI assistance disabled
- **WHEN** AI assistance is not enabled and the user runs `devy seed --generate postgres`
- **THEN** devy fails with the `ai-assist` capability's not-enabled message and sends nothing
