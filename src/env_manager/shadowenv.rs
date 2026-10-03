use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use which::which;

use super::EnvManager;

pub const ENV_FILE: &str = ".shadowenv.d/500_devy.lisp";
const ENV_FILENAME: &str = "500_devy.lisp";

#[derive(Default)]
pub struct Shadowenv;

impl Shadowenv {
    pub(crate) fn write_env_file(
        &self,
        dir: &Path,
        vars: &HashMap<String, String>,
        path_prepends: &[String],
    ) -> Result<()> {
        let shadowenv_dir = dir.join(".shadowenv.d");
        fs::create_dir_all(&shadowenv_dir).context("Failed to create .shadowenv.d")?;

        let esc = |s: &str| {
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\0', "")
        };
        let mut content = String::from("(provide \"devy\" \"1.0.0\")\n\n");

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

        fs::write(shadowenv_dir.join(ENV_FILENAME), content)
            .context("Failed to write shadowenv environment file")?;

        Ok(())
    }

    fn trust(&self, dir: &Path, bin: &Path) -> Result<()> {
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

/// Parses `(env/set "KEY" "VALUE")` lines from the shadowenv lisp file.
/// Returns `None` if the file does not exist yet.
pub fn read_vars(path: &Path) -> Option<HashMap<String, String>> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut vars = HashMap::new();
    for line in content.lines() {
        if let Some((key, value)) = parse_env_set_line(line.trim()) {
            vars.insert(key, value);
        }
    }
    Some(vars)
}

/// Parses `(env/prepend-to-pathlist "PATH" "ENTRY")` lines from the shadowenv lisp file.
/// Returns `None` if the file does not exist yet.
pub fn read_path_prepends(path: &Path) -> Option<Vec<String>> {
    let content = std::fs::read_to_string(path).ok()?;
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

impl EnvManager for Shadowenv {
    fn name(&self) -> &str {
        "shadowenv"
    }

    fn is_available(&self) -> bool {
        which("shadowenv").is_ok()
    }

    fn setup(
        &self,
        dir: &Path,
        vars: &HashMap<String, String>,
        path_prepends: &[String],
    ) -> Result<()> {
        self.write_env_file(dir, vars, path_prepends)?;
        // After a fresh Nix install the profile bin dir isn't on PATH yet, so
        // `which` may fail. Fall back to searching the dirs we're about to
        // prepend — one of them will contain the just-installed shadowenv binary.
        let bin = which("shadowenv")
            .ok()
            .or_else(|| {
                path_prepends
                    .iter()
                    .map(|p| PathBuf::from(p).join("shadowenv"))
                    .find(|p| p.exists())
            })
            .unwrap_or_else(|| PathBuf::from("shadowenv"));
        self.trust(dir, &bin)?;
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

    fn tmp_dir() -> crate::test_support::TempDir {
        crate::test_support::tmp_dir()
    }

    // ── read_vars ─────────────────────────────────────────────────────────────

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
