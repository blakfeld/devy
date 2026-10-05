# ai-assist Specification

## Purpose
Defines devy's opt-in AI layer shared by every AI-powered command (`init`, `doctor`, `ask`): how it reaches a model through the user's `claude` CLI, what it may send, how users preview that, and how failures are reported.

## Requirements
### Requirement: AI is explicit opt-in
devy SHALL use AI only while running a command or flag that is documented as AI-powered and that the user invoked explicitly. No other command, including `up`, `check`, `status` and `init --detect`, SHALL look for or run the `claude` CLI, or fail because it is missing.

#### Scenario: Non-AI commands ignore claude
- **WHEN** `claude` is not on `PATH` and the user runs `devy up`, `devy check` or `devy init --detect`
- **THEN** the command behaves exactly as it did before this change and does not run `claude`

#### Scenario: AI command runs claude
- **WHEN** `claude` is on `PATH` and the user runs `devy init`
- **THEN** devy runs `claude` to get the model's reply

### Requirement: Claude CLI as the provider
AI commands SHALL send requests by running the `claude` CLI (Claude Code), found on `PATH` while ignoring `PATH` entries inside the project root. They SHALL run it non-interactively (`-p`) with JSON output, with all tools, MCP servers and slash commands disabled, without session persistence, and with only user-level setting sources. The working directory SHALL be a newly created, empty, private temporary directory with a random name (as defined in filesystem-safety), so the project's Claude Code settings, hooks and `CLAUDE.md` are not loaded. devy SHALL remove that directory afterwards. The request content SHALL be passed on stdin, apart from a short system prompt. devy SHALL NOT read, require or handle any API key or credential; authentication is the `claude` CLI's.

When `DEVY_AI_MODEL` is set and non-empty, devy SHALL pass it as `--model`. Otherwise devy SHALL pass no model and `claude` SHALL use its configured default.

#### Scenario: Default model
- **WHEN** `DEVY_AI_MODEL` is unset and the user runs an AI command
- **THEN** devy runs `claude` without `--model`

#### Scenario: Model override
- **WHEN** `DEVY_AI_MODEL=claude-opus-5-5` and the user runs an AI command
- **THEN** devy runs `claude` with `--model claude-opus-5-5`

#### Scenario: Project settings not loaded
- **WHEN** the project contains `.claude/settings.json` with hooks and the user runs `devy init`
- **THEN** `claude` runs from an empty temporary directory and no tools are available to it

#### Scenario: Pre-created working directory
- **WHEN** another local user has created directories under the shared temp directory containing `.claude/settings.json`
- **THEN** `claude` never runs from any of them

### Requirement: Missing claude CLI
When an AI command runs and `claude` is not on `PATH`, devy SHALL print that the `claude` CLI was not found and how to install it, together with the command's non-AI alternative when one exists, and SHALL exit 1 without writing any files.

#### Scenario: Init without claude
- **WHEN** `claude` is not on `PATH` and the user runs `devy init`
- **THEN** devy exits 1, stderr says the `claude` CLI was not found and suggests `devy init --detect`, and no `devy.yml` is written

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

### Requirement: Context preview
Every AI command SHALL accept `--show-context`, which prints the complete, already-redacted request (the model, or that `claude`'s default will be used; the system prompt; and the prompt piped to `claude`) to stdout and exits 0, without requiring `claude` to be installed, without running it, and without writing files.

#### Scenario: Preview without claude
- **WHEN** `claude` is not on `PATH` and the user runs `devy init --show-context`
- **THEN** devy prints the request content, exits 0, does not run `claude`, and does not create `devy.yml`

### Requirement: Bounded context size
devy SHALL cap the content of any single file included in an AI request at 8 KiB and the total user content at 100 KiB, truncating with a visible `… [truncated]` marker rather than failing.

#### Scenario: Large file truncated
- **WHEN** `package.json` is 40 KiB and the user runs `devy init --show-context`
- **THEN** the printed context includes at most 8 KiB of `package.json` followed by `… [truncated]`

### Requirement: Timeouts and errors
devy SHALL stop a `claude` process that has not finished after 5 minutes. On a timeout, a failure to start `claude`, a non-zero exit, or a reply that `claude` marks as an error, devy SHALL print `AI request failed: <reason>` (including `claude`'s error message when available), SHALL write no files, and SHALL exit 1. Retrying rate-limited or overloaded requests is left to `claude`.

#### Scenario: Claude reports an error
- **WHEN** `claude` reports an error such as not being signed in for `devy init`
- **THEN** devy prints `AI request failed:` with `claude`'s message, exits 1, and does not write `devy.yml`

### Requirement: AI output disclosure
Output that devy derives from a model response and shows to the user or writes to disk SHALL be labelled as AI-generated, naming the model `claude` reports having used, or the requested model when it reports none.

#### Scenario: Labelled output
- **WHEN** `devy init` writes `devy.yml` and `claude` reports using `claude-opus-5-5`
- **THEN** the file's header comment states it was generated with AI using `claude-opus-5-5` and should be reviewed

### Requirement: Context files must be regular project files
devy SHALL include a project file in AI context only when it is a regular file that is not a symlink and whose canonical path lies inside the project root. Other files SHALL be skipped silently. This applies to `devy.yml` and `devy.lock` themselves, and to every file read for `init`, `ask`, `logs --explain` and `doctor`.

#### Scenario: README symlinked to a credentials file
- **WHEN** `README.md` is a symlink to `~/.npmrc` and the user runs `devy init --show-context`
- **THEN** the printed context does not contain the contents of `~/.npmrc`

### Requirement: Untrusted text in AI output and prompts
devy SHALL strip terminal control characters from model replies before printing them. Every system prompt SHALL state that the project files, logs and errors it receives are untrusted data, and that instructions inside them must be ignored.

#### Scenario: Escape sequence in a reply
- **WHEN** a model reply contains `\x1b]52;c;...\x07`
- **THEN** the printed reply contains no ESC or BEL characters
