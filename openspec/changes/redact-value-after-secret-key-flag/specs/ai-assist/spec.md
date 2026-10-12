# Spec Delta

## ADDED Requirements

### Requirement: Redaction of flag-introduced and command-set secret values
In addition to the rules of "Redaction before sending", devy SHALL redact secret values that a command line sets through a flag or a config-setter argument:
- **Flag after a secret-named key:** in a space-separated `key value` assignment whose key names a secret and is not itself a flag, a word starting with `-` SHALL NOT be treated as the value. When that word is `--body`, `-b` or `--value`, the following word (quoted words up to their closing quote) SHALL be redacted; when it is `--body=<value>` or `--value=<value>`, the part after `=` SHALL be redacted. The flag itself SHALL be kept. Any other flag SHALL be left as is. A key that is itself a flag (`--password -x`) keeps its existing behavior.
- **`gh secret set`:** within a `gh secret set` command (up to an unquoted `|`, `;`, `&` or the end of the line), the value of `--body` or `-b` SHALL be redacted in any position and in the spaced, `--body=<value>` and glued `-b<value>` forms, whatever the secret's name.
- **Config setters:** the value argument of `aws configure set <key> <value>`, `npm config set`, `pnpm config set` and `yarn config set <key> <value>`, and `git config [options] [set] <key> <value>` SHALL be redacted when `<key>` names a secret under the key rule (for example `aws_secret_access_key`, `//registry.npmjs.org/:_authToken`, `npmAuthToken`, `github.token`). Options before the key SHALL NOT be taken as the key or the value. A value whose key does not name a secret (`git config user.name Alice`) SHALL be kept.

#### Scenario: gh secret body after a secret-named key
- **WHEN** a sent line is `gh secret set API_KEY --body hunter2`
- **THEN** the sent text is `gh secret set API_KEY --body <redacted>` and does not contain `hunter2`

#### Scenario: Short body flag
- **WHEN** a sent line is `gh secret set API_KEY -b hunter2`
- **THEN** the sent text is `gh secret set API_KEY -b <redacted>`

#### Scenario: Quoted body
- **WHEN** a sent line is `gh secret set MY_TOKEN --body "hunter 2"`
- **THEN** the sent text contains neither `hunter` nor `2"`

#### Scenario: Body after another flag
- **WHEN** a sent line is `gh secret set API_KEY --env prod --body hunter2`
- **THEN** the sent text contains `--env prod` and does not contain `hunter2`

#### Scenario: Body for a secret whose name is not secret-shaped
- **WHEN** a sent line is `gh secret set deploy_key --body=hunter2`
- **THEN** the sent text does not contain `hunter2`

#### Scenario: Value flag after a secret-named key
- **WHEN** a sent line is `vault-tool put DB_PASSWORD --value hunter2`
- **THEN** the sent text does not contain `hunter2` and still contains `--value`

#### Scenario: Unrelated flag after a secret-named key
- **WHEN** a sent line is `export API_KEY -n`
- **THEN** the sent text is unchanged

#### Scenario: Flag key keeps its value
- **WHEN** a sent line is `mytool --password -x`
- **THEN** the sent text does not contain `-x`

#### Scenario: AWS config setter
- **WHEN** a sent line is `aws configure set aws_secret_access_key hunter2`
- **THEN** the sent text does not contain `hunter2`

#### Scenario: npm and yarn config setters
- **WHEN** sent lines are `npm config set //registry.npmjs.org/:_authToken hunter2` and `yarn config set npmAuthToken hunter3`
- **THEN** the sent text contains neither `hunter2` nor `hunter3`

#### Scenario: git config with options
- **WHEN** a sent line is `git config --global github.token hunter2`
- **THEN** the sent text does not contain `hunter2`

#### Scenario: Non-secret config key kept
- **WHEN** a sent line is `git config --global user.name Alice`
- **THEN** the sent text is unchanged
