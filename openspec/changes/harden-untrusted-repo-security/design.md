# Design

## Context

The audit (see proposal.md) found three root causes behind most issues:
1. **No consent step.** Nothing stands between a cloned repo and the code it declares.
2. **No value validation.** Values from `devy.yml` and `devy.lock` reach `Command` argv, `sh -c` strings, Nix strings and file paths unchecked.
3. **Unsafe writes.** File writes use `std::fs::write`, `create_dir_all` and predictable names, so committed symlinks and shared `/tmp` paths redirect them.

The fix is shaped by three facts about the current code:
- Process calls already use argv for almost everything. The exceptions are the installer pipelines in `deno.rs`, `bun.rs`, `gcloud.rs`, `brew.rs` and `nix.rs`.
- Config parsing is centralized in `src/config.rs` (`normalized_dependencies`, `validate_image`), and lock parsing in `src/lock.rs`.
- Every write site is a direct `std::fs` call spread across about 20 files. There is no shared helper to retrofit.

## Goals / Non-Goals

**Goals:**
- Fix each root cause once, centrally, so new modules inherit the protections.
- Keep a trusted project's `devy up` exactly as fast and quiet as today.
- Make every finding from the audit either impossible or visible in the trust summary.

**Non-Goals:**
- **Sandboxing trusted code.** Once the user allows a project, its hooks and installs run with full user rights, as `direnv allow` and `mise trust` do.
- **Making services secure for multi-user hosts.** Passwordless local databases stay the default. The changes close LAN exposure and shared-`/tmp` hijacks only.
- **Verifying packages from nix, brew, apt or winget beyond what those tools already do.**
- **Windows ACL hardening for the trust store.** The file is placed under `%LOCALAPPDATA%` and relies on its default ACL.

## Decisions

### D1. Trust store keyed on root + content digest, not a per-entry allowlist
A trust record is `{root, sha256(devy.yml), sha256(devy.lock)}` in one file per project, named by `sha256(canonical_root)`, under `$XDG_STATE_HOME/devy/trust/`. Any byte change re-prompts.
- **Alternative: hash only the executable fields.** This would re-prompt less often. But it requires deciding what counts as "executable" correctly forever, and lock-driven attacks (deno/ruby versions, brew `@` pins) live in non-obvious fields. Hashing whole files is simple and obviously safe.
- **Lock churn:** `devy up --update` rewrites the lock, but devy refreshes the record for its own writes (D1a). Teammates' lock changes pulled from git *should* re-prompt.
- **Alternative: reuse shadowenv's trust.** Its trust only covers `.shadowenv.d`, not file contents, and it is not available on Windows.
- **D1a.** Refreshing the record after devy's own writes happens only if the project was trusted when the command started. That prevents a write from ever upgrading an untrusted project.

### D2. Gate at command entry, with an explicit list of gated commands
`up`, `down`, `start` and `restart` call `trust::require(&config, interactive)` immediately after loading the config. That is the earliest point where hooks or setup could run.
- `stop` is not gated. It only stops processes devy started.
- Explicitly invoked project commands (`devy dev`) are not gated. The user typed the command name, which matches `npm run dev`; gating them would add friction without stopping anything a user would not already do.
- Non-interactive runs fail instead of auto-allowing. CI adds one `devy allow` line.
- An environment variable bypass (`DEVY_TRUST_ALL`) was rejected, because a repo can influence the environment through `.envrc` or shadowenv.

### D3. Validation lives in the config and lock loaders, with one rules module
A new `src/validate.rs` holds the regexes and helpers (`dep_name`, `version`, `list_entry`, `command_name`, `rel_path_inside`, `env_key`). `config.rs` calls it from `normalized_dependencies` and command and hook parsing. `lock.rs` calls it on load. Module `extra` lists are validated where each module reads them, through a shared `helpers::extra_list` that applies `list_entry`.
- **Alternative: validate in each package manager.** That spreads the rules across nine call sites and is what let `validate_tap` drift from the actual install path.
- Validation happens at load, so `devy check`, `_commands` and `_services` all reject a hostile config the same way. `_commands` then prints nothing, which already fixes the completion bug at the source. The bash snippet change (D6) is defense in depth for older binaries and future fields.
- The `--` separators are added in each backend's argv builder. For tools without `--` support, devy relies on named options plus validation.

### D4. One safe-write primitive and one private-dir primitive
`src/fs_safe.rs` provides two primitives:
- **`write_atomic(path, bytes, mode)`:**
  1. `lstat` the destination and every component devy created under the project. Refuse symlinks.
  2. Create `<dir>/.<name>.<random>.tmp` with `create_new(true)` plus `O_NOFOLLOW` (`custom_flags` on Unix).
  3. Write, `fsync`, set the mode, then `rename`.
  4. On Windows, use `create_new` and check `symlink_metadata` before and after.
- **`private_dir(base, prefix, random: bool)`:** `DirBuilder` with mode 0700 and no `create_dir_all`. If the directory already exists, `lstat` it and require a directory with the current uid and mode 0700, or fail. The socket fallback keeps a deterministic name, so it survives restarts, but is now owner-checked under a per-user base. The AI working directory uses a random name.

`tempfile` is added for randomness and Windows behavior. Every existing `fs::write`/`create_dir_all` site in the audit list moves to these primitives. A `clippy.toml` `disallowed-methods` entry for `std::fs::write` keeps new code from regressing; the few legitimate uses get `#[allow]` with a comment.

### D5. Devy-managed directory checks via `git ls-files`
"Tracked by git" is checked with `git ls-files -z -- .devy .shadowenv.d <venv>` (argv, no shell), run only when `.git` exists and `git` is on PATH. If git is unavailable, devy falls back to the symlink and owner checks alone.
- **Alternative: parse the git index ourselves.** Too complex for the value it adds.

The nix profile check uses `fs::read_link` plus `canonicalize`, then `starts_with("/nix/store/")`.

### D6. Bash completion without `compgen -W`
The snippet reads `devy _commands` with `mapfile -t`, then loops with `[[ $c == "$cur"* ]] && COMPREPLY+=("$c")`. Built-ins stay in a static array inside the snippet, so only project data takes the literal path. zsh and fish are already safe and only gain `allow`.

### D7. Installer verification with pinned digests compiled in
`src/installers.rs` holds a table of `{name, url, sha256}`, with URLs pinned to exact versions or commits. Downloads use the existing `ureq`, which is HTTPS via rustls with redirects restricted to https, instead of `curl | sh`. devy writes the script to `private_dir`, compares `sha2::Sha256`, and runs `sh <file> args…` or `bash <file> args…` with argv.
- **Alternative: download the installer's published `.sha256`.** That gives no protection against a compromised origin.
- **Cost:** a devy release is needed to bump an installer. That is acceptable; a stale pin fails closed with the mismatch error, which points users at the manual install URL.
- Bun's version moves from `BUN_INSTALL_VERSION` (which the installer may ignore) to its positional `bun-v<version>` argument.

### D8. AI-written and doctor-written config: diff executable fields structurally
`src/config_diff.rs` loads old and new configs and compares the sets of executable entries: hooks, `after_install`, `install_cmd`, tap, image, commands, and the execution-affecting environment keys. Matching on diff text was rejected because YAML formatting changes would defeat it.
- `init` uses the result to prompt, or to strip entries into TODO comments.
- `doctor` uses it to refuse `--yes`.
- The trust summary (D1) uses the same enumeration, so "what counts as executable" is defined once.

### D9. Redactor rewrite as an ordered rule list with fixture tests
`redact.rs` becomes a list of `(name, Regex, replacement)` rules applied in order: PEM and block scalars first, then URL userinfo, query parameters, headers, Bearer tokens, JWTs, key-based assignments anywhere in a line, token prefixes. `regex` is already a transitive dependency; it is promoted to a direct dependency. Each leak the audit found becomes a fixture test with a "must not contain" assertion.

### D10. Terminal sanitization at the output layer
`output.rs` gains `clean(&str) -> Cow<str>`, applied inside `step`, `info`, `warn`, `success` and the table printers. Log streaming and AI reply printing call it explicitly. Centralizing it means config values can't bypass it by being formatted into a message.

### D11. MySQL `cli_args` as an allowlist; brew config through an include file
The allowlist is a `const` slice in `helpers.rs`. Under nix and docker, forced arguments are appended *after* user tokens, so last-wins semantics favor devy. Under brew, devy writes `$(brew --prefix)/etc/my.cnf.d/devy.cnf`. If the formula's `my.cnf` lacks `!includedir`, devy warns once and does not edit it, instead of overwriting the user's global `my.cnf`.

### D12. YAML crate replacement
`serde_yml` is replaced with `serde_norway`, a maintained fork with the same serde API. The 60 `serde_yml::` call sites become a crate alias (`use serde_norway as yaml`). A test with a nested-alias document checks that alias expansion is bounded.

### D13. CI hardening
- Actions are pinned to full SHAs with a version comment, and Dependabot (`github-actions` ecosystem) keeps them current.
- Each workflow gets top-level `permissions: contents: read`. Only the release job gets `contents: write`.
- `CARGO_REGISTRY_TOKEN` and `github.ref_name` are passed through `env:`.
- The release job uploads `SHA256SUMS` and uses `actions/attest-build-provenance`.
- `cargo audit` runs in CI and fails on vulnerabilities, not on unmaintained warnings.
- Tag builds skip saving to the rust cache.

## Risks / Trade-offs

- **Every existing project prompts once after upgrade, and CI breaks until `devy allow` is added.** → Release notes, and an error that names the fix (`run devy allow`).
- **Stricter name and version regexes reject legitimate but unusual names**, such as apt `:arch` suffixes and brew `org/tap/formula` names. → `tap` stays the supported way to use third-party formulae. Add `:arch` to the name rule. The error message names the field so the fix is obvious.
- **The `cli_args` allowlist drops options people rely on.** → Unknown options warn instead of failing, and the allowlist is easy to extend.
- **Pinned installer digests go stale when upstream republishes a script under the same version.** → The URLs pin immutable tags or commits; a mismatch fails closed with the manual-install URL.
- **`git ls-files` adds a process spawn to each `up`.** → It runs once per command and only when the paths exist.
- **Redaction may over-redact (for example `SESSION_TIMEOUT=30`).** → Over-redaction only loses AI context, which is the safe direction.
- **Stripping control characters from logs removes colors from `devy logs`.** → The cli spec keeps SGR color sequences for log output to a terminal and removes every other escape. Tests check that OSC 52, OSC 8 and cursor movement are removed.
- **`add-worktree-environments` conflicts** in the shell-integration completion list and in `.devy/` handling. → Land this change first. The worktree change already plans `.devy/.gitignore`, which complements the tracked-file check.

## Migration Plan

1. Land the root-cause fixes in order: D3 validation, D4/D5 filesystem safety, then D2 trust. Each is independently shippable and safe.
2. Ship the trust gate with the clear error, release notes, and a `devy allow` line added to `.github/workflows/integration.yml` and `doctor-e2e.sh`.
3. **Rollback:** the trust gate is one call per gated command, so reverting it restores the old behavior. Records in the trust store are inert if unused.
4. Dependency and CI changes ship as separate PRs within the change.

## Open Questions

- **Exact pinned versions and digests** for the Determinate Nix installer, Homebrew `install.sh`, deno, bun and gcloud installers, and the MinIO/MailHog image tags. These are filled in during implementation and don't affect the approach.
- **Linux trust-store path:** should it also honor a `DEVY_STATE_DIR` override for testing? It is harmless either way; tests can set `XDG_STATE_HOME`.
