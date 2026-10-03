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
