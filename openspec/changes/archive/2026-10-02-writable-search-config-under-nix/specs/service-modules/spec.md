## ADDED Requirements

### Requirement: Writable search server config under nix
When devy starts `elasticsearch` or `opensearch` with the nix backend, it SHALL run the server with a project-local, writable config directory at `<project_root>/.devy/data/<canonical-name>/config/`, set as `ES_PATH_CONF` or `OPENSEARCH_PATH_CONF` respectively.
- **First start:** when that directory does not exist, devy SHALL create it as a writable copy of the installed package's `config/` directory. Later starts SHALL reuse it unchanged, keeping user edits.
- **Relative JVM paths:** in the copied `jvm.options`, devy SHALL rewrite the relative log and heap-dump paths to absolute paths under `.devy/data/<canonical-name>/logs/` and `.devy/data/<canonical-name>/data/`, because the package's start scripts run from the read-only package directory. This happens only when the directory is first created.
- **Read-only store:** devy MUST NOT write into the Nix store.
- **Elasticsearch** SHALL additionally run with `-E xpack.ml.enabled=false`, and with `ES_HOME` set to the installed package's directory.
- **OpenSearch:** when its package bundles the security plugin, it SHALL run with that plugin disabled, so it serves plain HTTP on `127.0.0.1:<port>`.

#### Scenario: First start seeds the config
- **WHEN** `devy up` starts `opensearch` under nix for the first time
- **THEN** `.devy/data/opensearch/config/` exists with the package's `opensearch.yml` and `jvm.options`, owner-writable, and the server's keystore is created there
- **AND** the copied `jvm.options` writes its GC log under `.devy/data/opensearch/logs/`

#### Scenario: User edits survive restarts
- **WHEN** the user edits `.devy/data/opensearch/config/jvm.options` and runs `devy restart opensearch`
- **THEN** devy does not overwrite the file, and the server starts with the edited options

#### Scenario: Elasticsearch becomes healthy
- **WHEN** `devy up` starts `elasticsearch` under nix with unfree installs allowed
- **THEN** the Elasticsearch health check passes on the resolved port, and nothing under `/nix/store` is modified
