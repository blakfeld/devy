## ADDED Requirements

### Requirement: Insecure packages allowed in exports
When the export lists packages whose modules declare them insecure, the generated file SHALL also set an `allowInsecurePredicate` that permits exactly those packages by name, imported the same way as `allowUnfreePredicate`. When no listed package is insecure, the output MUST NOT contain an insecure predicate.

#### Scenario: Flake with Elasticsearch
- **WHEN** `dependencies` contains `elasticsearch` and the user runs `devy export`
- **THEN** `flake.nix` imports nixpkgs with an `allowUnfreePredicate` and an `allowInsecurePredicate` that each allow `elasticsearch` and no other package

#### Scenario: No insecure packages
- **WHEN** `dependencies` contains `mongodb` but not `elasticsearch`
- **THEN** the export contains no `allowInsecurePredicate`
