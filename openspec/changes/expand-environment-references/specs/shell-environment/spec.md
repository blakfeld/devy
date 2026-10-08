## MODIFIED Requirements

### Requirement: User environment overrides module environment
When `devy up` merges environment variables, values from the `environment:` section of `devy.yml` SHALL take precedence over variables contributed by dependency modules. The environment file SHALL contain each `environment` value after reference expansion (project-config, Environment references), never the `${NAME}` text. shadowenv sets values literally.

#### Scenario: DATABASE_URL override
- **WHEN** the postgresql module contributes `DATABASE_URL` and `devy.yml` sets `DATABASE_URL: postgres://custom`
- **THEN** the shadowenv file contains `DATABASE_URL` set to `postgres://custom`

#### Scenario: References expanded in the environment file
- **WHEN** `devy.yml` lists `postgresql`, `devy up` assigns it port 54321, and `environment` sets `DATABASE_URL: "postgres://${POSTGRESQL_HOST}:${POSTGRESQL_PORT}/app"`
- **THEN** the shadowenv file contains `(env/set "DATABASE_URL" "postgres://127.0.0.1:54321/app")` and no `${`

#### Scenario: Escaped reference written literally
- **WHEN** `environment` sets `TEMPLATE: "$${X}"`
- **THEN** the shadowenv file contains `(env/set "TEMPLATE" "${X}")`
