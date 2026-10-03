# Spec Delta

## MODIFIED Requirements

### Requirement: Up phase ordering
`devy up` SHALL run these steps in order:
1. Print the header `devy up · <name>`, where `<name>` is the `name` from `devy.yml` with control characters stripped, or `project` when it is unset.
2. Check project trust, prompting or failing as defined in project-trust.
3. Check that devy-managed paths (`.devy/`, `.shadowenv.d/`, the virtualenv) are not symlinks or tracked by git, as defined in filesystem-safety.
4. Run the `before_up` hook.
5. Ensure the package manager is available.
6. Load `devy.lock`. This happens even with `--update`.
7. Validate each dependency's configuration.
8. Pin versions from the lock (unless `--update`), resolve service ports, and fail on port conflicts.
9. Install dependencies (phase 1).
10. Write `devy.lock`, and update the trust record to match it.
11. Write the shell environment.
12. Report orphaned lock entries.
13. Start services (phase 2).
14. Run the `after_up` hook.
15. Print `✓ <name> is ready`.

A failure in any step SHALL abort the remaining steps. Each hook run SHALL be preceded by a `Hooks` header.

#### Scenario: Failing before_up hook
- **WHEN** the `before_up` hook exits non-zero
- **THEN** no dependency is installed and devy exits 1

#### Scenario: Successful run
- **WHEN** every step succeeds
- **THEN** the final line is `✓ <name> is ready`

#### Scenario: Untrusted project stops before hooks
- **WHEN** the project is not trusted and the user declines the prompt
- **THEN** the `before_up` hook does not run and nothing is installed
