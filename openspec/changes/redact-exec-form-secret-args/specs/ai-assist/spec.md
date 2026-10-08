# Spec Delta

## MODIFIED Requirements

### Requirement: Redaction before sending
Before any content leaves the machine, devy SHALL replace secrets with `<redacted>`:
- **Key-based values:** the value of every `key=value`, `key: value`, `"key": "value"` or `key value` assignment, anywhere in a line, whose key contains `KEY`, `SECRET`, `TOKEN`, `PASSWORD`, `PASSWD`, `PASS`, `PWD`, `CREDENTIAL`, `PRIVATE`, `AUTH`, `COOKIE`, `SESSION`, `DSN` or `CERT` (case-insensitive), or has `SIG`, `SIGNATURE`, `SALT`, `PAT` or `PW` (or their plural) as a whole word (also after a `-D` flag, as in `-Dpw=`), split at `_`, `-`, `.` and camelCase (so `sig`, `X-Amz-Signature`, `PASSWORD_SALT`, `GITHUB_PAT` and `githubPat` match and `PATH`, `myPath` and `SIGNAL` do not). Whitespace or tabs may surround the separator, and the key may be quoted.
- **Indented blocks:** the indented body of a YAML block scalar (`|` or `>`) under such a key.
- **Credential shapes:**
  - a URL with an embedded userinfo password (the userinfo ends at the last `@` before the host)
  - URL query parameters whose name matches the key rule
  - `Authorization:` header values
  - `Bearer <token>`
  - JWTs (three dot-separated base64url segments starting with `eyJ`)
  - PEM blocks
  - `.npmrc` `_authToken`/`_auth`
  - `.netrc` `password` fields
  - Docker config `auth` values
  - `mysql -p<password>`
  - Slack webhook URLs
- **Exec-form command arguments:** in a flow sequence (`["a", "b"]`, with quoted or plain elements, possibly over several lines) or a block sequence (consecutive `- item` lines at one indent), devy SHALL redact:
  - the element after a flag whose name matches the key rule (`--requirepass`, `--password`, `--token`)
  - the element after `-a` or `--pass` once a `redis-cli` element has appeared
  - the element after `-P` once a `sqlcmd` element has appeared
  - the element after `-p` once a `<tool> login`, `sshpass` or MongoDB tool element has appeared
  - a value glued inside one element (`--password=x`, or `-px` after a MySQL/MariaDB client or `sshpass`)

  Each element's quotes and the separators between elements SHALL be kept.
- **Token prefixes:** a token with a well-known prefix: `sk-`, `sk_live_`, `sk_test_`, `rk_live_`, `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_`, `glpat-`, `xox`, `AKIA`, `ASIA`, `AIza`, `npm_`, `hf_`, `xapp-`, `GOCSPX-`, `ya29.`, `whsec_`, `pypi-`, `hvs.`, `shpat_`, `dop_v1_`, `lin_api_`, `AGE-SECRET-KEY-1`; SendGrid keys (`SG.<id>.<secret>`); and Telegram bot tokens (`<digits>:AA<30 or more characters>`). Prefixes that also begin ordinary identifiers SHALL require the token's own length and alphabet, so `hf_hub_download` is kept.

devy MUST NOT read or send `.env`, `.env.local` or other non-example dotenv files, `devy.lock` secrets, SSH keys, or the contents of `.git`.

#### Scenario: Secret-looking key redacted
- **WHEN** `.env.example` contains `STRIPE_SECRET_KEY=sk_test_abc` and the user runs `devy init --show-context`
- **THEN** the printed context contains `STRIPE_SECRET_KEY=<redacted>` and does not contain `sk_test_abc`

#### Scenario: URL password redacted
- **WHEN** a sent value is `postgres://app:hunter2@localhost/app`
- **THEN** the sent text contains `postgres://app:<redacted>@localhost/app`

#### Scenario: Real dotenv ignored
- **WHEN** the project contains both `.env` and `.env.example`
- **THEN** only `.env.example` contributes to the context

#### Scenario: Mid-line assignment in a log
- **WHEN** a log line is `2026-10-02 INFO connecting with password=hunter2`
- **THEN** the sent text does not contain `hunter2`

#### Scenario: Bearer token in a log
- **WHEN** a log line contains `Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig`
- **THEN** the sent text does not contain the token

#### Scenario: URL password containing a slash
- **WHEN** a sent value is `postgres://u:hun/ter2@db/app`
- **THEN** the sent text does not contain `hun/ter2`

#### Scenario: Minified JSON
- **WHEN** a sent line is `{"user":"a","password":"x9"}`
- **THEN** the sent text does not contain `x9`

#### Scenario: Exec-form compose command
- **WHEN** `compose.yaml` has `command: ["redis-server", "--requirepass", "hunter2"]` and the user runs `devy init --show-context`
- **THEN** the printed context contains `command: ["redis-server", "--requirepass", "<redacted>"]` and does not contain `hunter2`

#### Scenario: Exec-form healthcheck
- **WHEN** a sent compose file has `test: ["CMD", "redis-cli", "-a", "hunter2", "ping"]`
- **THEN** the sent text contains `test: ["CMD", "redis-cli", "-a", "<redacted>", "ping"]`

#### Scenario: Block-list command
- **WHEN** a sent compose file has `command:` followed by the items `- redis-server`, `- --requirepass` and `- hunter2` on separate lines
- **THEN** the sent text keeps the first two items and has `- <redacted>` in place of `- hunter2`

#### Scenario: Glued value keeps the sequence intact
- **WHEN** a sent line is `args: ["--api-key=abc123", "--port", "80"]`
- **THEN** the sent text is `args: ["--api-key=<redacted>", "--port", "80"]`

#### Scenario: Non-secret flags in exec form kept
- **WHEN** a sent line is `command: ["redis-server", "--port", "6380", "--appendonly", "yes"]`
- **THEN** the sent text is unchanged
