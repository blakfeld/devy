# pull-request Specification

## Purpose
`devy pr` opens the GitHub pull request for the current branch in the browser. It opens the existing PR when one exists, and otherwise the "compare & create" page.

## Requirements

### Requirement: Branch preconditions
The system SHALL determine the current branch with `git rev-parse --abbrev-ref HEAD` and MUST refuse to continue when the branch is exactly `main` or `master`. No `devy.yml` is required. On a detached HEAD, git reports `HEAD` as the branch and devy continues with it.

#### Scenario: On the default branch
- **WHEN** the user runs `devy pr` on `main`
- **THEN** devy fails with `already on the default branch — create a feature branch first`

#### Scenario: Detached HEAD
- **WHEN** HEAD is detached, `gh` finds no PR, and the repo is `acme/app`
- **THEN** devy opens `https://github.com/acme/app/compare/HEAD?expand=1`

#### Scenario: Outside a git repository
- **WHEN** `git rev-parse` fails
- **THEN** devy fails with `not inside a git repository`

### Requirement: GitHub remote parsing
The system SHALL read `git remote get-url origin` and extract `<owner>/<repo>` from `git@github.com:` or `https://github.com/` URLs, stripping a trailing `.git`. A missing origin MUST fail with `no remote named 'origin' found`. A non-GitHub URL MUST fail with `could not parse GitHub repo from remote URL: <url>`.

#### Scenario: SSH remote
- **WHEN** origin is `git@github.com:acme/app.git`
- **THEN** the repository path is `acme/app`

#### Scenario: Non-GitHub remote
- **WHEN** origin is `https://gitlab.com/acme/app.git`
- **THEN** devy fails with `could not parse GitHub repo from remote URL: https://gitlab.com/acme/app.git`

### Requirement: Target URL selection
The system SHALL open the existing PR URL when `gh pr view --json url --jq .url` succeeds with non-empty output. Otherwise it MUST open `https://github.com/<repo>/compare/<branch>?expand=1`, with the branch percent-encoded except for `A–Z a–z 0–9 - _ . ~`.

#### Scenario: No existing PR
- **WHEN** the branch is `feat/login`, `gh` finds no PR, and the repo is `acme/app`
- **THEN** devy opens `https://github.com/acme/app/compare/feat%2Flogin?expand=1`

#### Scenario: Existing PR
- **WHEN** `gh pr view` returns `https://github.com/acme/app/pull/42`
- **THEN** devy opens that URL

### Requirement: Platform browser opener
The system SHALL open the URL with `open` on macOS, `xdg-open` on Linux, or `cmd /c start` on Windows. It MUST fail with `failed to open browser` only if the opener cannot be spawned; the opener's exit status is ignored. devy prints nothing on success. Other platforms MUST fail with `opening a browser is not supported on this platform`.

#### Scenario: macOS
- **WHEN** `devy pr` runs on macOS with a resolvable URL
- **THEN** devy runs `open <url>` and exits 0
