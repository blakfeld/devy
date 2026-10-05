# Deferred review findings

Findings raised by the code-reviewer and security-analyst during implementation that the user chose to defer (2026-10-03). Each should become a follow-up issue.

## Pre-existing exposure
- **Medium:** under brew, MinIO runs via `brew services` on all interfaces with `minioadmin` credentials; devy's configured keys never reach it.
- `devy logs` (non-follow `tail_file`) has no line-length cap, so a log that is one huge line is read fully into memory.

## Races and hashing
- Config discovery checks ownership by path, then reopens by name (TOCTOU). Directories owned by the user but writable by others are trusted.
- `init_detect::read` has a TOCTOU window and does not detect hardlinks.
- The `sh.devy.config` docker label is an unsalted FNV hash that covers secret values.
- The docker `--env-file` (0600) is left in `$TMPDIR` if devy is killed during `docker run`.
- The config-discovery ownership check is a no-op on non-Unix platforms.

## Over-redaction (accepted trade-offs)
- `keywords`/`authors` lists, `--cert-dir <value>`, compose `secrets:` name lists and the `oauth2-proxy` subtree are redacted before AI context.

## Misc info
- Podman may parse quoted `--env-file` values differently from Docker (unverified).
- `default-authentication-plugin` is in the MySQL `cli_args` allowlist; on MySQL 8.4/MariaDB it can only stop the server starting.
- Service spec structs carrying secrets derive `Debug` without redaction.
- TAB is rejected along with other control characters in service credentials.
- Under brew, `my.cnf.d/devy.cnf` lives in the shared global prefix, so the last project to run `up` sets the config for the single brew MySQL/MariaDB server.

## Deferred after the final review (2026-10-03)
- **No-git data dirs:** without git (tarball/zip repos), a pre-populated `.devy/data/<svc>` (Elasticsearch `config/jvm.options`, `postgresql.conf`, Meilisearch `config.toml`) or a shipped `.venv` with its own `pip` is used as-is behind an innocuous trust summary. Follows from D5's no-git fallback.
- **Persistent side effects:** `rustup default` changes the user's global toolchain and `brew tap` leaves a third-party tap installed; the trust summary does not say these persist beyond the project.
- **Docker loopback limits:** on Linux, ports published on 127.0.0.1 are not a hard boundary (local users can reach bridge IPs; LAN neighbours can on Docker Engine < 28). Documentation only.
- **Second-stage downloads:** the pinned installer scripts download their own binaries (nix-installer, rustup-init, deno/bun release zips) without a digest devy knows.
