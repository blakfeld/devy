//! `--json` output shared by `status`, `services` and `check`.

use anyhow::Result;
use serde::Serialize;

/// The `version` of every JSON document. Adding fields keeps it; removing or renaming a
/// field, or changing its type or meaning, increments it.
pub const VERSION: u32 = 1;

#[derive(Serialize)]
struct Document<'a, T: Serialize> {
    version: u32,
    #[serde(flatten)]
    body: &'a T,
}

/// `body` as a JSON document with the top-level `version` field.
pub fn render<T: Serialize>(body: &T) -> Result<String> {
    let mut out = serde_json::to_string_pretty(&Document {
        version: VERSION,
        body,
    })?;
    out.push('\n');
    Ok(out)
}

/// Prints `body` to stdout as the command's only output.
#[cfg_attr(test, mutants::skip)] // thin stdout wrapper over `render`
pub fn print<T: Serialize>(body: &T) -> Result<()> {
    write_stdout(&render(body)?)
}

/// Writes `s` to stdout. A reader that closed the pipe early (`devy status --json | head`)
/// is not an error, where `print!` would panic.
#[cfg_attr(test, mutants::skip)] // writes to the process's stdout
pub fn write_stdout(s: &str) -> Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match out.write_all(s.as_bytes()).and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

/// Turns off ANSI colors for the rest of the process, so stderr warnings in JSON mode
/// are plain text too.
#[cfg_attr(test, mutants::skip)] // global switch for the `colored` crate
pub fn disable_color() {
    colored::control::set_override(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Body {
        items: Vec<u32>,
    }

    #[test]
    fn render_puts_version_first_and_ends_with_newline() {
        let out = render(&Body { items: vec![1] }).unwrap();
        assert!(out.ends_with("}\n"), "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["items"], serde_json::json!([1]));
        assert!(
            out.find("\"version\"") < out.find("\"items\""),
            "version leads the document: {out}"
        );
    }
}
