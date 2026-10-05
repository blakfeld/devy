<!-- devy:begin -->
## {bin}

This project's development environment is declared in `{bin}.yml` and managed by `{bin}`.

- Learn the project with `{bin} status --json` and `{bin} services --json` instead of guessing ports or environment variables.
- Run project tools through `{bin} exec -- <program> [args…]` so they get the project's environment, service ports and PATH. It runs without a shell; use `{bin} exec -- sh -c '…'` for pipes.
- Start a stopped service with `{bin} start <name>`. Inspect failures with `{bin} logs <name>` and `{bin} check --json`.
- Run project commands from `{bin}.yml` as `{bin} <command>`.
- Don't edit `{bin}.lock` or `.shadowenv.d/` by hand. Change `{bin}.yml` and run `{bin} up`.
<!-- devy:end -->
