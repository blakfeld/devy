# environment-up delta

## MODIFIED Requirements

### Requirement: Up phase ordering
`devy up` SHALL run these steps in order:
1. Print the header `devy up · <name>`, where `<name>` is the `name` from `devy.yml` with control characters stripped, or `project` when it is unset.
2. Check that devy-managed paths (`.devy/`, `.shadowenv.d/`, the virtualenv) are not symlinks or tracked by git, as defined in filesystem-safety.
3. Run the `before_up` hook.
4. Ensure the package manager is available.
5. Load `devy.lock`. This happens even with `--update`.
6. Validate each dependency's configuration.
7. Pin versions from the lock (unless `--update`), resolve service ports, and fail on port conflicts.
8. Install dependencies (phase 1).
9. Write `devy.lock`.
10. Write the shell environment.
11. Report orphaned lock entries.
12. Start services (phase 2).
13. Run the `after_up` hook.
14. Print `✓ <name> is ready`.

`devy up` SHALL NOT ask the user to allow the project or check any trust record: it runs the project's hooks and installs as `devy.yml` declares them.

A failure in any step SHALL abort the remaining steps. Each hook run SHALL be preceded by a `Hooks` header.

#### Scenario: Failing before_up hook
- **WHEN** the `before_up` hook exits non-zero
- **THEN** no dependency is installed and devy exits 1

#### Scenario: Successful run
- **WHEN** every step succeeds
- **THEN** the final line is `✓ <name> is ready`

#### Scenario: Untrusted project stops before hooks
- **WHEN** a cloned repository commits `.shadowenv.d/` and the user runs `devy up`
- **THEN** devy fails naming `.shadowenv.d` as tracked by git, the `before_up` hook does not run, and nothing is installed

#### Scenario: No trust prompt
- **WHEN** the user runs `devy up` for the first time in a freshly cloned project, with or without a terminal
- **THEN** devy prints no allow prompt and runs the `before_up` hook
