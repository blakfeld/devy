## MODIFIED Requirements

### Requirement: Environment variables exported as attributes
The system SHALL emit each `environment:` entry as an attribute of the mkShell, in key order, except a key named `packages` or `shellHook`. Those are attributes the export writes itself, so such a key SHALL be left out as a comment with a warning. `devy export` SHALL NOT check any trust record or comment entries out. Like `devy up`, the exported shell applies the project's environment, and reviewing `devy.yml` is the user's decision. (`mkShell` treats some names, such as `preHook`, as code, and others, such as `BASH_ENV`, change how the shell runs.)

Each value SHALL be the expanded value (project-config, Environment references). devy SHALL compute it from the project environment as `devy exec` does, with ports resolved read-only from `devy.lock` or the worktree port file, and SHALL NOT write any file other than the export. When a reference cannot be resolved, `devy export` SHALL print the error, write nothing and exit 1.

Keys that are not valid bare Nix identifiers MUST be quoted. Values MUST be emitted as Nix double-quoted strings with `\`, `"` and `${` escaped. Quoted keys use the same escapes.

#### Scenario: Value containing interpolation syntax
- **WHEN** `environment` has `GREETING: 'hi $${USER}'`
- **THEN** the export contains `GREETING = "hi \${USER}";`

#### Scenario: Reference expanded in the export
- **WHEN** `devy.lock` assigns redis port 52113 and `environment` has `REDIS_URL: "redis://${REDIS_HOST}:${REDIS_PORT}"`
- **THEN** the export contains `REDIS_URL = "redis://127.0.0.1:52113";`

#### Scenario: Undefined reference
- **WHEN** `environment` has `URL: "${NOPE}"` and nothing defines `NOPE`
- **THEN** `devy export` exits 1 with `environment.URL: ${NOPE} is not defined` and writes no file

#### Scenario: Key needing quotes
- **WHEN** `environment` has the key `rec`, a Nix keyword
- **THEN** the attribute name is emitted quoted as `"rec"`

#### Scenario: Untrusted project
- **WHEN** a freshly cloned project's `environment` has `preHook: "touch /tmp/p"` and `FOO: bar`
- **THEN** the export contains the attributes `preHook = "touch /tmp/p";` and `FOO = "bar";`, nothing is commented out, and no warning about allowing the project is printed

#### Scenario: Value that would end the comment
- **WHEN** an entry's value contains a newline followed by `shellHook = "touch /tmp/q";`
- **THEN** the quotes are escaped, so the whole value stays inside its Nix string and adds no `shellHook` attribute

#### Scenario: Trusted project
- **WHEN** `environment` has `BASH_ENV: /tmp/env.sh`
- **THEN** the export contains `BASH_ENV = "/tmp/env.sh";` and no warning is printed
