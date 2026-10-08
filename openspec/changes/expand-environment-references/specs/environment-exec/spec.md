## MODIFIED Requirements

### Requirement: Project environment for exec
The program SHALL inherit devy's own environment, overlaid with the project environment that `devy up` writes to the environment file:
- module environment variables
- `<SERVICE>_HOST` and `<SERVICE>_PORT` for each service
- `devy.yml` `environment`, with references expanded (project-config, Environment references), which overrides module values for the same name

PATH SHALL be the module PATH entries, including the package manager's own entry, in the order `devy up` writes them, followed by the inherited PATH. A `PATH` set in devy.yml `environment` SHALL replace this value, as it does in the environment file.

devy SHALL compute these values from `devy.yml` and `devy.lock` on each run, whether or not the environment file exists or is current. Ports SHALL be resolved read-only, so `devy exec` SHALL NOT assign ports or write `devy.lock`, the environment file or any other file. A service whose port is unassigned SHALL get `<SERVICE>_HOST` but no `<SERVICE>_PORT`. The program SHALL be looked up on the computed PATH.

When a reference cannot be resolved, devy SHALL print the error and exit 1 without running the program. This covers an undefined name, a cycle, and a port that is not assigned yet, where the error says to run `devy up`.

#### Scenario: Locked port exported
- **WHEN** `devy.lock` assigns redis port 52113 and the user runs `devy exec env`
- **THEN** the output contains `REDIS_HOST=127.0.0.1` and `REDIS_PORT=52113`

#### Scenario: Config environment wins
- **WHEN** a module sets `DATABASE_URL` and `devy.yml` `environment` also sets `DATABASE_URL`
- **THEN** the program sees the `devy.yml` value

#### Scenario: Project-local binary found
- **WHEN** the project uses the nix profile and `jq` is installed only in `.devy/nix-profile/bin`, and the user runs `devy exec jq --version` from a shell without shadowenv
- **THEN** the project's `jq` runs

#### Scenario: Works before the environment file exists
- **WHEN** `devy.yml` sets `LOG_LEVEL=debug` and `.shadowenv.d` does not exist
- **THEN** `devy exec env` prints `LOG_LEVEL=debug` and no file is created

#### Scenario: Reference expanded for the program
- **WHEN** `devy.lock` assigns redis port 52113 and `environment` sets `REDIS_URL: "redis://${REDIS_HOST}:${REDIS_PORT}"`
- **THEN** `devy exec env` prints `REDIS_URL=redis://127.0.0.1:52113`

#### Scenario: Port not yet assigned
- **WHEN** the backend assigns ports, `devy up` has never run, and `environment` references `${REDIS_PORT}`
- **THEN** `devy exec env` exits 1 with an error that names `REDIS_PORT` and suggests `devy up`, and runs nothing
