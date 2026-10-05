# Hostile fixture repository

Used by the `hostile_repo_*` tests in `tests/cli.rs` (OpenSpec change
`harden-untrusted-repo-security`, task 18.1). The tests copy `tree/` into a temp project,
replace `@OUT@` with a test-owned directory outside the project, and create the symlinks
listed in `symlinks.txt` (git cannot commit symlinks portably).

`tree/devy.yml` is invalid and must be rejected at load. `valid.devy.yml` passes
validation, so `up` reaches the trust gate and the managed-path checks. Every hook
touches a marker and then exits 1, so a hook that runs by mistake is detected and still
stops `up` before any real dependency install.

| Audit payload | Where | Exercised end to end |
|---|---|---|
| Hooks that touch a marker outside the project | both configs | yes: refused untrusted, declined, and (after allow) stopped by the managed-path checks |
| `.deb` dependency name | `tree/devy.yml` (`./evil.deb`) | yes: `check`, `up`, `allow` |
| Hostile command name | `tree/devy.yml`, `tree/package.json` script names | yes: `check`, `up`, `init --detect`, bash completion (also fed directly by a stub `devy`) |
| `.nvmrc` newline + YAML injection | `tree/.nvmrc` | yes: `init --detect` |
| Symlinked README / `.env.example` to credentials | `symlinks.txt` | yes: `init --show-context`, `init --detect` |
| Committed `.shadowenv.d` lisp | `tree/.shadowenv.d/000_evil.lisp` | yes: tracked-by-git refusal, then the "files devy did not write" refusal |
| Committed `.venv` | `tree/.venv` | yes: tracked-by-git refusal |
| Planted `.venv/bin` tools | `tree/.venv/bin/*` | `git`, `shadowenv` and `claude` are looked up; `nix`, `brew`, `sudo` and `dpkg-query` are planted but no CLI phase reaches a package-manager lookup (unit tests for task 5.2 cover them) |
| Symlinked `.devy`, `.devy-lock`, `devy.lock`, `500_devy.lisp` | `symlinks.txt` and the test | yes: each is refused and its target left untouched |
| Symlinked lock temp names | `symlinks.txt`, plus links at devy's own pid planted by the test | yes: the lock is written past them |
| Symlinked stamps | `symlinks.txt` | planted only: no CLI phase installs a dependency, so stamp writers are covered by `helpers::tests::write_stamp_refuses_symlinked_stamp` and the bun stamp tests (task 4.2) |
| MySQL `cli_args` with `--init-file`, `--plugin-load` | both configs | present only: the allowlist runs at service launch, covered by `mysql_family_launch_skips_dangerous_options`, `mysql_family_launch_forced_bind_wins` and `write_mysql_config_dangerous_options_skipped` (task 9.1) |
| `BASH_ENV` | both configs | listed in the trust summary; its marker script is defence in depth |
| OSC 52 escape in `name` | both configs | yes: no `ESC ]` in any output |
