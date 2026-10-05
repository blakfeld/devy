# package-managers delta

## MODIFIED Requirements

### Requirement: Homebrew taps
When a dependency sets `tap`, the Homebrew backend SHALL validate it as `org/repo` (exactly one `/`, each part non-empty, starting with a letter or digit, limited to letters, digits, `-`, `_`, `.`, and not equal to `.` or `..`) and run `brew tap -- <tap>` before installing. Taps are listed in the executable entry listing (project-config). A dependency name can never select a tap, because dependency names cannot contain `/`.

#### Scenario: Invalid tap
- **WHEN** a dependency sets `tap: "evil; rm -rf /"`
- **THEN** config validation fails before anything is installed

#### Scenario: Valid tap
- **WHEN** `mongodb` sets `tap: mongodb/brew`
- **THEN** devy runs `brew tap -- mongodb/brew` and then installs `mongodb-community`

#### Scenario: Tap smuggled through the name
- **WHEN** a dependency is named `evilorg/tap/formula`
- **THEN** config loading fails with an invalid-name error and `brew` is never run
