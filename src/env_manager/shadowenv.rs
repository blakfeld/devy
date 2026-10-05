use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::EnvManager;

pub const ENV_FILE: &str = ".shadowenv.d/500_devy.lisp";
pub(crate) const ENV_FILENAME: &str = "500_devy.lisp";

/// The first line of every `500_devy.lisp` devy writes: a Lisp comment naming a random
/// nonce, new on each write (`; devy-env <32 lowercase hex digits>`).
const NONCE_PREFIX: &str = "; devy-env ";
/// The per-user directory (`<state>/devy/shadowenv/`, see `state_dir`) holding an
/// exact copy of each `500_devy.lisp` devy wrote, named `<nonce>.lisp`. The shell hook's
/// guard (`devy hook`) compares the project's file with the copy its first line names: repository content can neither write a copy nor learn the nonce of one
/// (it is generated on this machine), so a `500_devy.lisp` a pull replaced never matches.
pub(crate) const COPY_SUBDIR: &str = "shadowenv";

#[derive(Default)]
pub struct Shadowenv;

impl Shadowenv {
    /// Writes `.shadowenv.d/500_devy.lisp`, keeping its copy in the per-user state
    /// directory (see [`COPY_SUBDIR`]).
    pub(crate) fn write_env_file(
        &self,
        dir: &Path,
        vars: &HashMap<String, String>,
        path_prepends: &[String],
    ) -> Result<()> {
        let copies = copy_dir_for(dir)?;
        self.write_env_file_in(dir, vars, path_prepends, copies.as_deref())
    }

    /// [`Shadowenv::write_env_file`] with the copy directory supplied (`None`: no copy).
    pub(crate) fn write_env_file_in(
        &self,
        dir: &Path,
        vars: &HashMap<String, String>,
        path_prepends: &[String],
        copy_dir: Option<&Path>,
    ) -> Result<()> {
        let shadowenv_dir = dir.join(".shadowenv.d");
        crate::fs_safe::ensure_dir_in(dir, &shadowenv_dir)
            .context("Failed to create .shadowenv.d")?;
        // `shadowenv trust` trusts the whole directory, so it must hold only devy's file.
        refuse_foreign_entries(&shadowenv_dir)?;

        let esc = |s: &str| {
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\0', "")
        };
        let nonce = crate::fs_safe::random_hex128();
        let mut content = format!("{NONCE_PREFIX}{nonce}\n(provide \"devy\" \"1.0.0\")\n\n");

        // PATH prepends: emit in reverse so the first entry ends up leftmost in PATH.
        for entry in path_prepends.iter().rev() {
            content.push_str(&format!(
                "(env/prepend-to-pathlist \"PATH\" \"{}\")\n",
                esc(entry)
            ));
        }
        if !path_prepends.is_empty() {
            content.push('\n');
        }

        let mut sorted_vars: Vec<(&String, &String)> = vars.iter().collect();
        sorted_vars.sort_by_key(|(k, _)| k.as_str());
        for (key, value) in sorted_vars {
            // Both key and value are escaped: unescaped quotes or backslashes would
            // corrupt the Lisp expression; an unescaped key could inject directives.
            content.push_str(&format!("(env/set \"{}\" \"{}\")\n", esc(key), esc(value)));
        }

        let env_file = shadowenv_dir.join(ENV_FILENAME);
        // The copy goes first, so the shell hook never sees a file without one.
        if let Some(copies) = copy_dir {
            crate::fs_safe::write_atomic(&copy_path(copies, &nonce), content.as_bytes(), 0o600)
                .context(COPY_ERROR)?;
        }
        let previous = read_env_file(&env_file);
        crate::fs_safe::write_atomic(&env_file, content.as_bytes(), 0o644)
            .context("Failed to write shadowenv environment file")?;
        // The copy of the file this one replaced is no longer needed.
        if let (Some(copies), Some(previous)) = (copy_dir, previous.as_deref())
            && header_nonce(previous.as_bytes()) != Some(nonce.as_str())
        {
            remove_copy_of(previous.as_bytes(), copies);
        }

        Ok(())
    }

    fn run_trust(&self, dir: &Path, bin: &Path) -> Result<()> {
        let status = Command::new(bin)
            .args(["trust"])
            .current_dir(dir)
            .status()
            .context("Failed to run shadowenv trust")?;
        if !status.success() {
            bail!("shadowenv trust failed");
        }
        Ok(())
    }
}

const COPY_ERROR: &str =
    "Failed to record the shadowenv environment file in devy's state directory";

/// The per-user copy directory for the project at `dir`, created (0700) when missing.
#[cfg(not(test))]
fn copy_dir_for(dir: &Path) -> Result<Option<PathBuf>> {
    crate::state_dir::StateDir::locate()
        .and_then(|state| state.ensure_env_copy_dir(dir))
        .map(Some)
        .context(COPY_ERROR)
}

/// Unit tests never touch the real state directory: they pass a copy directory to
/// [`Shadowenv::write_env_file_in`] themselves. `tests/cli.rs` covers the real one.
#[cfg(test)]
fn copy_dir_for(_dir: &Path) -> Result<Option<PathBuf>> {
    Ok(None)
}

/// The nonce on the first line of an env file devy wrote, if it has one.
fn header_nonce(content: &[u8]) -> Option<&str> {
    let first = content.split(|b| *b == b'\n').next()?;
    let nonce = std::str::from_utf8(first.strip_prefix(NONCE_PREFIX.as_bytes())?).ok()?;
    (nonce.len() == 32
        && nonce
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(nonce)
}

fn copy_path(copy_dir: &Path, nonce: &str) -> PathBuf {
    copy_dir.join(format!("{nonce}.lisp"))
}

/// Removes the copy in `copy_dir` that the env file `content` names, but only when it is
/// byte for byte that file: a `500_devy.lisp` copied from (or naming the nonce of)
/// another project must not delete that project's copy, which would make its shell hook
/// remove shadowenv's trust there. Returns whether a copy was removed.
fn remove_copy_of(content: &[u8], copy_dir: &Path) -> bool {
    let Some(nonce) = header_nonce(content) else {
        return false;
    };
    let copy = copy_path(copy_dir, nonce);
    crate::fs_safe::read_regular_capped(&copy, MAX_ENV_FILE_BYTES).is_ok_and(|c| c == content)
        && std::fs::remove_file(copy).is_ok()
}

/// Whether the project at `project_root` has a `.shadowenv.d/500_devy.lisp` that is not
/// the one devy last wrote: not a regular file, or not byte for byte the copy in
/// `copy_dir` that its first line names. An absent file is not foreign.
///
/// Test-only: devy itself never calls this. It is the Rust reference for the check the
/// shell hook's guard (`devy hook`, `_devy_shadowenv_check`) runs in shell, so the unit
/// tests pin down the rule the guard must match.
#[cfg(test)]
fn env_file_foreign(project_root: &Path, copy_dir: &Path) -> bool {
    let path = project_root.join(ENV_FILE);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
        Ok(meta) if !meta.file_type().is_file() => return true,
        Ok(_) => {}
    }
    let Ok(content) = crate::fs_safe::read_regular_capped(&path, MAX_ENV_FILE_BYTES) else {
        return true;
    };
    let Some(nonce) = header_nonce(&content) else {
        return true;
    };
    crate::fs_safe::read_regular_capped(&copy_path(copy_dir, nonce), MAX_ENV_FILE_BYTES)
        .map_or(true, |copy| copy != content)
}

fn unescape(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => result.push('"'),
                Some('\\') => result.push('\\'),
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Walks `s` byte-by-byte respecting `\"` escapes, returning the content before
/// the first unescaped `"` and the remainder after it.
fn scan_quoted(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2; // skip escaped character
            continue;
        }
        if b[i] == b'"' {
            return Some((&s[..i], &s[i + 1..]));
        }
        i += 1;
    }
    None
}

/// Parses a single `(env/set "KEY" "VALUE")` line, respecting escaped quotes in
/// both key and value. Returns `None` if the line doesn't match the format.
fn parse_env_set_line(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("(env/set \"")?;
    let (raw_key, rest) = scan_quoted(rest)?;
    let rest = rest.strip_prefix(" \"")?;
    let (raw_value, rest) = scan_quoted(rest)?;
    rest.strip_prefix(")")?;
    Some((unescape(raw_key), unescape(raw_value)))
}

/// The env file devy wrote is a few KiB; anything larger is not one of ours.
const MAX_ENV_FILE_BYTES: u64 = 1024 * 1024;

/// The content of the env file at `path`. `status`, `check` and `doctor` read it without
/// trust, so it must be a regular file (not a symlink a repository committed, which could
/// point at `/dev/zero` or a FIFO) of at most [`MAX_ENV_FILE_BYTES`]; anything else
/// counts as absent.
fn read_env_file(path: &Path) -> Option<String> {
    let bytes = crate::fs_safe::read_regular_capped(path, MAX_ENV_FILE_BYTES).ok()?;
    String::from_utf8(bytes).ok()
}

/// Parses `(env/set "KEY" "VALUE")` lines from the shadowenv lisp file.
/// Returns `None` if the file does not exist yet (or is not a file devy would read).
pub fn read_vars(path: &Path) -> Option<HashMap<String, String>> {
    let content = read_env_file(path)?;
    let mut vars = HashMap::new();
    for line in content.lines() {
        if let Some((key, value)) = parse_env_set_line(line.trim()) {
            vars.insert(key, value);
        }
    }
    Some(vars)
}

/// Parses `(env/prepend-to-pathlist "PATH" "ENTRY")` lines from the shadowenv lisp file.
/// Returns `None` if the file does not exist yet (or is not a file devy would read).
pub fn read_path_prepends(path: &Path) -> Option<Vec<String>> {
    let content = read_env_file(path)?;
    let mut entries = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("(env/prepend-to-pathlist \"PATH\" \"") else {
            continue;
        };
        let Some((raw_entry, remainder)) = scan_quoted(rest) else {
            continue;
        };
        if !remainder.starts_with(')') {
            continue;
        }
        entries.push(unescape(raw_entry));
    }
    // Reverse: they were written in reverse-prepend order; restore original order.
    entries.reverse();
    Some(entries)
}

/// Fails when `.shadowenv.d` holds an entry shadowenv could evaluate other than devy's
/// `500_devy.lisp` (for example lisp a repository committed), naming them. Shadowenv
/// evaluates only `*.lisp` files, so other regular files (the `.gitignore` and
/// `.trust-<fingerprint>` that `shadowenv trust` writes itself, and the
/// `.error-<n>-<shell pid>` its hook writes while the directory is untrusted) are fine;
/// directories, symlinks and any other `*.lisp` are not. The shell hook's guard draws
/// the same line.
fn refuse_foreign_entries(shadowenv_dir: &Path) -> Result<()> {
    let read_err = || format!("Failed to read {}", shadowenv_dir.display());
    // Never list (or hand shadowenv) a directory a symlink points to: it could be
    // another project's, trusted or not. `ensure_dir_in` refuses one first; this keeps
    // the check from depending on that.
    crate::fs_safe::refuse_symlink(shadowenv_dir)?;
    let mut foreign = Vec::new();
    for entry in std::fs::read_dir(shadowenv_dir).with_context(read_err)? {
        let entry = entry.with_context(read_err)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_file = entry.file_type().with_context(read_err)?.is_file();
        let is_lisp = name.to_ascii_lowercase().ends_with(".lisp");
        if name != ENV_FILENAME && (!is_file || is_lisp) {
            foreign.push(name);
        }
    }
    if foreign.is_empty() {
        return Ok(());
    }
    foreign.sort();
    // shadowenv trusts the directory, not its content: a signature from an earlier
    // `devy up` would load the foreign lisp on the next prompt, so it goes first.
    if let Some(project) = shadowenv_dir.parent() {
        untrust(project);
    }
    bail!(
        ".shadowenv.d contains files devy did not write ({}); devy removed shadowenv's trust for this project; review and remove them, then run devy up",
        foreign.join(", ")
    )
}

/// [`remove_trust`] for the project at `root`, warning on failure.
pub(crate) fn untrust(root: &Path) {
    if let Err(e) = remove_trust(root) {
        crate::output::warn(&format!("could not remove shadowenv's trust: {e:#}"));
    }
}

/// Removes shadowenv's trust files (`.shadowenv.d/.trust-<fingerprint>`, the signature
/// `shadowenv trust` writes) for the project at `dir`, so the shell stops applying the
/// project's environment. shadowenv has no `untrust` command; deleting the signature is
/// how trust is revoked. Nothing happens when `.shadowenv.d` is missing or is not a real
/// directory (a symlink is never followed). Returns how many files were removed.
pub fn remove_trust(dir: &Path) -> Result<usize> {
    let shadowenv_dir = dir.join(".shadowenv.d");
    match std::fs::symlink_metadata(&shadowenv_dir) {
        Ok(meta) if meta.file_type().is_dir() => {}
        _ => return Ok(0),
    }
    let read_err = || format!("Failed to read {}", shadowenv_dir.display());
    let mut removed = 0;
    // Every signature is removed even when another entry fails, so a `.trust-0/`
    // directory a repository planted cannot keep the real one in place.
    let mut first_error = None;
    for entry in std::fs::read_dir(&shadowenv_dir).with_context(read_err)? {
        let entry = entry.with_context(read_err)?;
        if !entry.file_name().to_string_lossy().starts_with(".trust-") {
            continue;
        }
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir {
            continue; // not a signature shadowenv writes
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) => {
                first_error.get_or_insert_with(|| {
                    anyhow::Error::new(e).context(format!("Failed to remove {}", path.display()))
                });
            }
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(removed),
    }
}

/// The `shadowenv` binary devy runs: on PATH outside the project, or in the project nix
/// profile once it passes the `/nix/store` check.
fn shadowenv_bin() -> Option<PathBuf> {
    crate::fs_safe::which_with_project_profile("shadowenv")
}

impl EnvManager for Shadowenv {
    fn name(&self) -> &str {
        "shadowenv"
    }

    fn is_available(&self) -> bool {
        shadowenv_bin().is_some()
    }

    fn setup(
        &self,
        dir: &Path,
        vars: &HashMap<String, String>,
        path_prepends: &[String],
    ) -> Result<()> {
        // Refuses a `.shadowenv.d` holding anything but devy's own files, so what
        // `shadowenv trust` signs below is only what devy wrote.
        self.write_env_file(dir, vars, path_prepends)?;
        // Found outside the project, or in the verified project nix profile (where a
        // fresh nix-backend install puts it before its bin dir is on PATH), never in any
        // other project-local directory such as the PATH prepends written above.
        let bin = shadowenv_bin()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "shadowenv not found on PATH outside the project or in a verified .devy/nix-profile/bin"
                )
            })
            .context("Failed to run shadowenv trust")?;
        self.run_trust(dir, &bin)?;
        Ok(())
    }

    fn read_vars(&self, project_root: &Path) -> Option<HashMap<String, String>> {
        read_vars(&project_root.join(ENV_FILE))
    }

    fn read_path_prepends(&self, project_root: &Path) -> Option<Vec<String>> {
        read_path_prepends(&project_root.join(ENV_FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use which::which;

    fn tmp_dir() -> crate::test_support::TempDir {
        crate::test_support::tmp_dir()
    }

    // ── remove_trust ──────────────────────────────────────────────────────────

    #[test]
    fn remove_trust_deletes_only_trust_files() {
        let dir = tmp_dir();
        let sd = dir.join(".shadowenv.d");
        std::fs::create_dir(&sd).unwrap();
        std::fs::write(sd.join(".trust-a46f63ff"), "sig").unwrap();
        std::fs::write(sd.join(".trust-0badc0de"), "sig").unwrap();
        std::fs::write(sd.join(ENV_FILENAME), "(provide \"devy\")\n").unwrap();
        std::fs::write(sd.join(".gitignore"), "*\n").unwrap();
        assert_eq!(remove_trust(&dir).unwrap(), 2);
        assert!(!sd.join(".trust-a46f63ff").exists());
        assert!(sd.join(ENV_FILENAME).exists());
        assert!(sd.join(".gitignore").exists());
        // Missing directory: nothing to do.
        assert_eq!(remove_trust(&tmp_dir()).unwrap(), 0);
    }

    #[test]
    fn remove_trust_skips_a_planted_directory_and_still_removes_signatures() {
        let dir = tmp_dir();
        let sd = dir.join(".shadowenv.d");
        std::fs::create_dir_all(sd.join(".trust-0")).unwrap();
        std::fs::write(sd.join(".trust-a46f63ff"), "sig").unwrap();
        assert_eq!(remove_trust(&dir).unwrap(), 1);
        assert!(!sd.join(".trust-a46f63ff").exists());
        assert!(sd.join(".trust-0").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn remove_trust_never_follows_a_symlinked_directory() {
        let dir = tmp_dir();
        let outside = tmp_dir();
        std::fs::write(outside.join(".trust-a46f63ff"), "sig").unwrap();
        std::os::unix::fs::symlink(&*outside, dir.join(".shadowenv.d")).unwrap();
        assert_eq!(remove_trust(&dir).unwrap(), 0);
        assert!(outside.join(".trust-a46f63ff").exists());
    }

    // ── read_vars ─────────────────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn env_file_symlinks_fifos_and_huge_files_are_not_read() {
        let dir = tmp_dir();
        let outside = tmp_dir();
        let target = outside.join("x.lisp");
        std::fs::write(&target, "(env/set \"FOO\" \"bar\")\n").unwrap();
        let link = dir.join("link.lisp");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_vars(&link).is_none());
        assert!(read_path_prepends(&link).is_none());
        let zero = dir.join("zero.lisp");
        std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
        assert!(read_vars(&zero).is_none());
        let fifo = dir.join("fifo.lisp");
        if std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success())
        {
            // Returns promptly instead of blocking.
            assert!(read_vars(&fifo).is_none());
        }
        let huge = dir.join("huge.lisp");
        std::fs::write(&huge, "x".repeat(MAX_ENV_FILE_BYTES as usize + 1)).unwrap();
        assert!(read_vars(&huge).is_none());
        assert_eq!(read_vars(&target).unwrap()["FOO"], "bar");
    }

    #[test]
    fn read_vars_missing_file_returns_none() {
        let path = std::path::Path::new("/nonexistent/500_devy_test.lisp");
        assert!(read_vars(path).is_none());
    }

    #[test]
    fn read_vars_parses_env_set_lines() {
        let dir = tmp_dir();
        let file = dir.join("500_devy.lisp");
        std::fs::write(
            &file,
            "(provide \"devy\" \"1.0.0\")\n\n(env/set \"FOO\" \"bar\")\n(env/set \"BAZ\" \"qux\")\n",
        ).unwrap();

        let vars = read_vars(&file).unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars["FOO"], "bar");
        assert_eq!(vars["BAZ"], "qux");
    }

    #[test]
    fn read_vars_ignores_non_env_set_lines() {
        let dir = tmp_dir();
        let file = dir.join("500_devy.lisp");
        std::fs::write(
            &file,
            "(provide \"devy\" \"1.0.0\")\n; a comment\n(env/set \"KEY\" \"value\")\n",
        )
        .unwrap();

        let vars = read_vars(&file).unwrap();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["KEY"], "value");
    }

    #[test]
    fn unescape_double_backslash_produces_single() {
        // "a\\\\b" in source (4 chars: a, \, \, b) → unescape → "a\\b" (3 chars: a, \, b)
        assert_eq!(unescape(r"a\\b"), r"a\b");
    }

    #[test]
    fn read_vars_unescapes_backslash_and_quote() {
        let dir = tmp_dir();
        let file = dir.join("500_devy.lisp");
        // Stored as: (env/set "KEY" "a\\b\"c")
        std::fs::write(&file, "(env/set \"KEY\" \"a\\\\b\\\"c\")\n").unwrap();

        let vars = read_vars(&file).unwrap();
        assert_eq!(vars["KEY"], "a\\b\"c");
    }

    #[test]
    fn read_vars_empty_file_returns_empty_map() {
        let dir = tmp_dir();
        let file = dir.join("500_devy.lisp");
        std::fs::write(&file, "").unwrap();

        let vars = read_vars(&file).unwrap();
        assert!(vars.is_empty());
    }

    // ── write_env_file ────────────────────────────────────────────────────────

    #[test]
    fn write_env_file_creates_directory_and_file() {
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let mut vars = HashMap::new();
        vars.insert("MY_VAR".into(), "hello".into());

        shadowenv.write_env_file(&dir, &vars, &[]).unwrap();

        let file = dir.join(".shadowenv.d").join("500_devy.lisp");
        assert!(file.exists());
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("(provide \"devy\""));
        assert!(content.contains("MY_VAR"));
        assert!(content.contains("hello"));
    }

    #[test]
    fn write_env_file_escapes_special_chars_in_value() {
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let mut vars = HashMap::new();
        vars.insert("K".into(), "back\\slash and \"quote\"".into());

        shadowenv.write_env_file(&dir, &vars, &[]).unwrap();

        let file = dir.join(".shadowenv.d").join("500_devy.lisp");
        let parsed = read_vars(&file).unwrap();
        assert_eq!(parsed["K"], "back\\slash and \"quote\"");
    }

    #[test]
    fn write_env_file_escapes_newline_in_value() {
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let mut vars = HashMap::new();
        vars.insert("K".into(), "line1\nline2".into());
        shadowenv.write_env_file(&dir, &vars, &[]).unwrap();
        let file = dir.join(".shadowenv.d").join("500_devy.lisp");
        let raw = std::fs::read_to_string(&file).unwrap();
        assert!(
            !raw.contains("line1\nline2"),
            "raw newline must not appear in file"
        );
        let parsed = read_vars(&file).unwrap();
        assert_eq!(parsed["K"], "line1\nline2");
    }

    #[test]
    fn read_vars_round_trips_value_with_embedded_quote_space_quote() {
        // Value contains `" "` (quote-space-quote) which previously caused a wrong split.
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let mut vars = HashMap::new();
        vars.insert("K".into(), "a\" \"b".into());
        shadowenv.write_env_file(&dir, &vars, &[]).unwrap();
        let file = dir.join(".shadowenv.d").join("500_devy.lisp");
        let parsed = read_vars(&file).unwrap();
        assert_eq!(
            parsed["K"], "a\" \"b",
            "value containing '\" \"' must round-trip correctly"
        );
    }

    #[test]
    fn write_env_file_escapes_carriage_return_in_value() {
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let mut vars = HashMap::new();
        vars.insert("K".into(), "a\rb".into());
        shadowenv.write_env_file(&dir, &vars, &[]).unwrap();
        let file = dir.join(".shadowenv.d").join("500_devy.lisp");
        let parsed = read_vars(&file).unwrap();
        assert_eq!(parsed["K"], "a\rb");
    }

    #[test]
    fn write_env_file_escapes_special_chars_in_key() {
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let mut vars = HashMap::new();
        // A key with a quote would be a Lisp injection if not escaped.
        vars.insert("KEY_WITH_\"QUOTE\"".into(), "value".into());

        shadowenv.write_env_file(&dir, &vars, &[]).unwrap();

        let file = dir.join(".shadowenv.d").join("500_devy.lisp");
        let parsed = read_vars(&file).unwrap();
        // Should round-trip correctly rather than breaking the Lisp structure.
        assert_eq!(parsed["KEY_WITH_\"QUOTE\""], "value");
    }

    // ── Shadowenv::name ───────────────────────────────────────────────────────

    #[test]
    fn shadowenv_name_is_shadowenv() {
        assert_eq!(Shadowenv.name(), "shadowenv");
    }

    // ── Shadowenv::is_available ───────────────────────────────────────────────

    #[test]
    fn shadowenv_is_available_consistent_with_which() {
        let expected = which("shadowenv").is_ok();
        assert_eq!(Shadowenv.is_available(), expected);
    }

    #[test]
    fn shadowenv_is_available_true_when_installed() {
        if which("shadowenv").is_err() {
            return;
        }
        assert!(
            Shadowenv.is_available(),
            "must be true when shadowenv is on PATH"
        );
    }

    #[test]
    fn shadowenv_is_available_false_when_not_installed() {
        if which("shadowenv").is_ok() {
            return;
        }
        assert!(
            !Shadowenv.is_available(),
            "must be false when shadowenv is absent"
        );
    }

    // ── Shadowenv::trust / setup ───────────────────────────────────────────────

    #[test]
    fn shadowenv_setup_writes_env_file() {
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let mut vars = HashMap::new();
        vars.insert("SETUP_KEY".into(), "setup_val".into());
        shadowenv.write_env_file(&dir, &vars, &[]).unwrap();
        let file = dir.join(".shadowenv.d").join("500_devy.lisp");
        assert!(file.exists(), "setup must create the lisp file");
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("SETUP_KEY"));
    }

    #[test]
    fn shadowenv_setup_fails_when_shadowenv_not_installed() {
        // When shadowenv binary is absent, trust() must fail, and setup() propagates that.
        // This kills `replace setup -> Ok(())` and `replace trust -> Ok(())`.
        if which("shadowenv").is_ok() {
            return;
        }
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let mut vars = HashMap::new();
        vars.insert("KEY".into(), "val".into());
        let result = shadowenv.setup(&dir, &vars, &[]);
        assert!(
            result.is_err(),
            "setup must fail when shadowenv binary is absent"
        );
    }

    #[test]
    fn shadowenv_setup_succeeds_when_shadowenv_installed() {
        // When shadowenv IS installed, setup() must succeed (trust runs without bail).
        // This kills `delete ! in trust` — mutation bails on success, making this Err.
        if which("shadowenv").is_err() {
            return;
        }
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let mut vars = HashMap::new();
        vars.insert("KEY".into(), "val".into());
        let result = shadowenv.setup(&dir, &vars, &[]);
        assert!(
            result.is_ok(),
            "setup must succeed when shadowenv is installed"
        );
    }

    // ── .shadowenv.d contents ─────────────────────────────────────────────────

    #[test]
    fn foreign_lisp_and_directories_are_refused_before_writing() {
        for planted in ["000_evil.lisp", "600_X.LISP"] {
            let dir = tmp_dir();
            std::fs::create_dir_all(dir.join(".shadowenv.d")).unwrap();
            std::fs::write(
                dir.join(".shadowenv.d").join(planted),
                "(env/set \"X\" \"y\")",
            )
            .unwrap();
            let err = Shadowenv
                .write_env_file(&dir, &HashMap::new(), &[])
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    ".shadowenv.d contains files devy did not write ({planted}); devy removed shadowenv's trust for this project; review and remove them, then run devy up"
                )
            );
            assert!(!dir.join(ENV_FILE).exists());
        }
        let dir = tmp_dir();
        std::fs::create_dir_all(dir.join(".shadowenv.d/nested")).unwrap();
        assert!(
            Shadowenv
                .write_env_file(&dir, &HashMap::new(), &[])
                .is_err()
        );
    }

    #[test]
    fn foreign_lisp_removes_an_earlier_shadowenv_trust() {
        let dir = tmp_dir();
        let d = dir.join(".shadowenv.d");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".trust-a46f63ff"), "sig").unwrap();
        std::fs::write(d.join("000_evil.lisp"), "(env/set \"X\" \"y\")").unwrap();
        assert!(
            Shadowenv
                .write_env_file(&dir, &HashMap::new(), &[])
                .is_err()
        );
        assert!(!d.join(".trust-a46f63ff").exists());
        assert!(d.join("000_evil.lisp").exists());
    }

    /// A committed `.shadowenv.d` symlink (to another project's trusted directory, say)
    /// is refused before anything is listed, written or trusted, and the directory it
    /// points to keeps its files and its signature.
    #[cfg(unix)]
    #[test]
    fn symlinked_shadowenv_dir_is_refused_and_its_target_untouched() {
        let dir = tmp_dir();
        let other = tmp_dir();
        let target = other.join(".shadowenv.d");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join(".trust-a46f63ff"), "sig").unwrap();
        std::fs::write(target.join("500_devy.lisp"), "theirs").unwrap();
        std::os::unix::fs::symlink(&target, dir.join(".shadowenv.d")).unwrap();
        let err = Shadowenv.setup(&dir, &HashMap::new(), &[]).unwrap_err();
        assert!(format!("{err:#}").contains("is a symbolic link"), "{err:#}");
        assert!(refuse_foreign_entries(&dir.join(".shadowenv.d")).is_err());
        assert_eq!(remove_trust(&dir).unwrap(), 0);
        assert_eq!(
            std::fs::read(target.join(".trust-a46f63ff")).unwrap(),
            b"sig"
        );
        assert_eq!(
            std::fs::read(target.join("500_devy.lisp")).unwrap(),
            b"theirs"
        );
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 2);
    }

    /// A `.error-*` that is a symlink is not shadowenv's (its hook would write through
    /// it): refused like any other foreign entry, and trust goes.
    #[cfg(unix)]
    #[test]
    fn symlinked_error_file_is_foreign() {
        let dir = tmp_dir();
        let outside = tmp_dir();
        let d = dir.join(".shadowenv.d");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".trust-a46f63ff"), "sig").unwrap();
        std::fs::write(outside.join("victim"), "keep").unwrap();
        std::os::unix::fs::symlink(outside.join("victim"), d.join(".error-0-4242")).unwrap();
        let err = Shadowenv
            .write_env_file(&dir, &HashMap::new(), &[])
            .unwrap_err();
        assert!(format!("{err:#}").contains(".error-0-4242"), "{err:#}");
        assert!(!d.join(".trust-a46f63ff").exists());
        assert_eq!(
            std::fs::read_to_string(outside.join("victim")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn files_shadowenv_trust_writes_are_accepted() {
        let dir = tmp_dir();
        let d = dir.join(".shadowenv.d");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".gitignore"), "*").unwrap();
        std::fs::write(d.join(".trust-a46f63ff"), "sig").unwrap();
        std::fs::write(d.join(".error-0-123"), "untrusted").unwrap();
        std::fs::write(d.join(ENV_FILENAME), "old").unwrap();
        Shadowenv
            .write_env_file(&dir, &HashMap::new(), &[])
            .unwrap();
    }

    /// Rewriting a `500_devy.lisp` that names another project's copy (copied over, or
    /// forged) leaves that copy alone.
    #[test]
    fn only_the_projects_own_copy_is_removed() {
        let copies = tmp_dir();
        let (a, b) = (tmp_dir(), tmp_dir());
        let vars = HashMap::from([("K".to_string(), "v".to_string())]);
        Shadowenv
            .write_env_file_in(&a, &vars, &[], Some(&copies))
            .unwrap();
        let a_file = std::fs::read(a.join(ENV_FILE)).unwrap();
        let a_copy = copy_path(&copies, header_nonce(&a_file).unwrap());
        // b's file names a's nonce, with other content.
        std::fs::create_dir_all(b.join(".shadowenv.d")).unwrap();
        let mut forged = a_file.clone();
        forged.extend_from_slice(b"(env/set \"X\" \"y\")\n");
        std::fs::write(b.join(ENV_FILE), &forged).unwrap();
        Shadowenv
            .write_env_file_in(&b, &vars, &[], Some(&copies))
            .unwrap();
        assert!(a_copy.exists(), "another project's copy must stay");
        assert!(!env_file_foreign(&a, &copies));
    }

    #[test]
    fn env_file_carries_a_fresh_nonce_and_its_copy_replaces_the_last_one() {
        let dir = tmp_dir();
        let copies = tmp_dir();
        let mut vars = HashMap::new();
        vars.insert("K".to_string(), "v".to_string());
        Shadowenv
            .write_env_file_in(&dir, &vars, &[], Some(&copies))
            .unwrap();
        let first = std::fs::read(dir.join(ENV_FILE)).unwrap();
        let nonce1 = header_nonce(&first).unwrap().to_string();
        assert_eq!(std::fs::read(copy_path(&copies, &nonce1)).unwrap(), first);
        assert!(!env_file_foreign(&dir, &copies));
        // The nonce line is a comment: the variables still parse.
        assert_eq!(read_vars(&dir.join(ENV_FILE)).unwrap()["K"], "v");

        Shadowenv
            .write_env_file_in(&dir, &vars, &[], Some(&copies))
            .unwrap();
        let second = std::fs::read(dir.join(ENV_FILE)).unwrap();
        let nonce2 = header_nonce(&second).unwrap().to_string();
        assert_ne!(nonce1, nonce2);
        assert!(!copy_path(&copies, &nonce1).exists(), "old copy removed");
        assert!(!env_file_foreign(&dir, &copies));
        // Without a copy (another machine's file, or a replaced one) it is foreign.
        assert!(env_file_foreign(&dir, &tmp_dir()));
        // Absent: nothing to load, so not foreign.
        assert!(!env_file_foreign(&tmp_dir(), &copies));
    }

    #[test]
    fn header_nonce_requires_32_lowercase_hex_digits() {
        let ok = format!("{NONCE_PREFIX}{}\n(x)", "a".repeat(32));
        assert_eq!(header_nonce(ok.as_bytes()), Some("a".repeat(32).as_str()));
        for bad in [
            format!("{NONCE_PREFIX}{}\n", "A".repeat(32)),
            format!("{NONCE_PREFIX}{}\n", "a".repeat(31)),
            format!("{NONCE_PREFIX}../../{}\n", "a".repeat(26)),
            format!(" {NONCE_PREFIX}{}\n", "a".repeat(32)),
        ] {
            assert_eq!(header_nonce(bad.as_bytes()), None, "{bad}");
        }
    }

    // ── read_path_prepends ────────────────────────────────────────────────────

    #[test]
    fn read_path_prepends_handles_escaped_quote_in_path() {
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let path_with_quote = "/home/user/\"project\"/bin".to_string();
        shadowenv
            .write_env_file(
                &dir,
                &HashMap::new(),
                std::slice::from_ref(&path_with_quote),
            )
            .unwrap();
        let file = dir.join(".shadowenv.d").join("500_devy.lisp");
        let entries = read_path_prepends(&file).unwrap();
        assert_eq!(
            entries,
            vec![path_with_quote],
            "path containing '\"' must round-trip through write/read correctly"
        );
    }

    #[test]
    fn read_path_prepends_round_trips_multiple_entries() {
        let dir = tmp_dir();
        let shadowenv = Shadowenv;
        let paths = vec![
            "/usr/local/bin".to_string(),
            "/home/user/.local/bin".to_string(),
        ];
        shadowenv
            .write_env_file(&dir, &HashMap::new(), &paths)
            .unwrap();
        let file = dir.join(".shadowenv.d").join("500_devy.lisp");
        let entries = read_path_prepends(&file).unwrap();
        assert_eq!(
            entries, paths,
            "multiple path entries must round-trip in order"
        );
    }
}
