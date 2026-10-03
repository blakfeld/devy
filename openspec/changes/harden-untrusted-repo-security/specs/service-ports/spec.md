# Spec Delta

## MODIFIED Requirements

### Requirement: MySQL cli_args sanitization
devy SHALL accept a whitespace-separated `cli_args` token for MySQL or MariaDB only if all of these hold:
- it has the form `--key=value`
- its key (with `_` treated as `-`) is on devy's allowlist of server tuning options
- its value contains no newline, carriage return or NUL

The allowlist SHALL contain:
- **buffers and caches:** `innodb-buffer-pool-size`, `innodb-log-file-size`, `innodb-redo-log-capacity`, `innodb-flush-log-at-trx-commit`, `innodb-file-per-table`, `key-buffer-size`, `table-open-cache`, `thread-cache-size`, `tmp-table-size`, `max-heap-table-size`, `sort-buffer-size`, `join-buffer-size`
- **limits:** `max-connections`, `max-allowed-packet`, `wait-timeout`, `interactive-timeout`
- **character sets and SQL behavior:** `character-set-server`, `collation-server`, `sql-mode`, `default-time-zone`, `lower-case-table-names`, `explicit-defaults-for-timestamp`, `default-authentication-plugin`
- **logging:** `log-bin-trust-function-creators`, `slow-query-log`, `long-query-time`, `general-log`
- **other:** `transaction-isolation`, `event-scheduler`, `performance-schema`, `skip-name-resolve`

Every other token SHALL be skipped with the warning `skipped cli_args token <token>: not an allowed server option`. Network, authentication, file-path, plugin and startup-script options (for example `bind-address`, `skip-grant-tables`, `init-file`, `plugin-load`, `plugin-dir`, `secure-file-priv`, `datadir`, `socket`, `user`, `general-log-file`) are therefore never passed. Accepted tokens SHALL be converted into `key = value` lines when devy writes a config file, or passed before devy's own forced arguments when launching directly. Under brew, devy SHALL write its settings to a devy-owned include file inside the formula's configuration directory, and SHALL NOT overwrite an existing `my.cnf` it did not create.

#### Scenario: Valid and invalid args
- **WHEN** `cli_args` is `--innodb-buffer-pool-size=256M bogus`
- **THEN** the config contains `innodb-buffer-pool-size = 256M` and devy warns that `bogus` was skipped

#### Scenario: Dangerous option skipped
- **WHEN** `cli_args` is `--bind-address=0.0.0.0 --skip-grant-tables=1`
- **THEN** both tokens are skipped with warnings and the server listens only on `127.0.0.1`
