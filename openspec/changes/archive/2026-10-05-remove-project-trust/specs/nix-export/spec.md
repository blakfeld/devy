# nix-export delta

## MODIFIED Requirements

### Requirement: Environment variables exported as attributes
The system SHALL emit each `environment:` entry as an attribute of the mkShell, in key order, except a key named `packages` or `shellHook` (attributes the export writes itself), which SHALL be left out as a comment with a warning. `devy export` SHALL NOT check any trust record or comment entries out: like `devy up`, the exported shell applies the project's environment (`mkShell` treats some names, such as `preHook`, as code, and others, such as `BASH_ENV`, change how the shell runs), and reviewing `devy.yml` is the user's decision.

Keys that are not valid bare Nix identifiers MUST be quoted. Values MUST be emitted as Nix double-quoted strings with `\`, `"` and `${` escaped. Quoted keys use the same escapes.

#### Scenario: Value containing interpolation syntax
- **WHEN** `environment` has `GREETING: 'hi ${USER}'`
- **THEN** the export contains `GREETING = "hi \${USER}";`

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
