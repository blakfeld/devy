# Spec Delta

## MODIFIED Requirements

### Requirement: Flake structure and shell hook
The flake export SHALL set `description = "<name> development environment"`, take `nixpkgs` from `github:NixOS/nixpkgs/nixpkgs-unstable`, and define `devShells.<system>.default` for `x86_64-linux`, `aarch64-linux`, `x86_64-darwin` and `aarch64-darwin`. Both formats MUST include a `shellHook` that echoes `Entered <name> dev shell`, with `<name>` defaulting to `project`.

`<name>` SHALL be escaped for every context it appears in:
- in a Nix `"…"` string, `\`, `"` and `${` are escaped
- in the `''…''` shellHook string, `''` and `${` are escaped
- inside the shellHook, the shell receives `<name>` as a single-quoted word, so `$`, backticks and `\` are not interpreted

Attribute names derived from configuration SHALL be escaped with the same Nix string rules.

#### Scenario: Unnamed project
- **WHEN** `devy.yml` has no `name` and the user runs `devy export`
- **THEN** the flake description is `project development environment` and the shellHook echoes `Entered project dev shell`

#### Scenario: Hostile project name
- **WHEN** `name` is `x"; ${builtins.abort "p"} $(touch /tmp/p) ''`
- **THEN** the exported file evaluates without error, entering the shell creates no `/tmp/p`, and the shellHook echoes the name literally

### Requirement: Environment variables exported as attributes
`devy export` SHALL NOT require project trust, but its output MUST NOT let `environment` entries of an untrusted project run code when the exported shell starts (`mkShell` treats some names, such as `preHook` or `shellHook`, as code, and others, such as `BASH_ENV`, change how the shell runs). Entries SHALL be emitted in key order, and whether the project is trusted SHALL be decided by the trust store with the current `devy.yml` and `devy.lock`, as defined in project-trust:
- When the project is trusted, the system SHALL emit each `environment:` entry as an attribute of the mkShell, except a key named `packages` or `shellHook` (attributes the export writes itself), which SHALL be left out as a comment with a warning.
- When the project is not trusted, the system SHALL write every entry as a `#` comment line (`# <key> = "<value>";`), preceded by a comment saying the project is not allowed, and SHALL print one warning telling the user to review `devy.yml`, run `devy allow` and export again. In a commented-out value, newlines and carriage returns MUST be written as `\n` and `\r` escapes and other control characters dropped, so a value cannot end the comment.

Keys that are not valid bare Nix identifiers MUST be quoted. Values MUST be emitted as Nix double-quoted strings with `\`, `"` and `${` escaped. Quoted keys use the same escapes.

#### Scenario: Value containing interpolation syntax
- **WHEN** the project is trusted and `environment` has `GREETING: 'hi ${USER}'`
- **THEN** the export contains `GREETING = "hi \${USER}";`

#### Scenario: Key needing quotes
- **WHEN** the project is trusted and `environment` has the key `rec`, a Nix keyword
- **THEN** the attribute name is emitted quoted as `"rec"`

#### Scenario: Untrusted project
- **WHEN** the project was never allowed and `environment` has `preHook: "touch /tmp/p"` and `FOO: bar`
- **THEN** the export contains `# preHook = "touch /tmp/p";` and `# FOO = "bar";` and no `preHook` or `FOO` attribute, and devy warns to run `devy allow` and export again

#### Scenario: Value that would end the comment
- **WHEN** the project is not trusted and an entry's value contains a newline followed by `shellHook = "touch /tmp/q";`
- **THEN** the whole entry stays on one comment line, with the newline written as `\n`

#### Scenario: Trusted project
- **WHEN** the project is allowed with its current files and `environment` has `BASH_ENV: /tmp/env.sh`
- **THEN** the export contains `BASH_ENV = "/tmp/env.sh";` and no warning is printed
