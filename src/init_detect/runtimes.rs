//! Language runtimes and their pinned versions.

use std::path::Path;

use super::{Detector, Draft, package_json, read, snippet, version_of};
use crate::modules;

/// `.nvmrc`, then `.node-version`, then `package.json` `engines.node`.
pub struct Node;

impl Detector for Node {
    fn detect(&self, dir: &Path, draft: &mut Draft) {
        for file in [".nvmrc", ".node-version"] {
            if let Some(content) = read(dir, file) {
                let pinned = content.trim();
                match version_of(pinned) {
                    Some(v) => draft.add_dep("node", Some(v)),
                    None => {
                        draft.add_dep("node", None);
                        draft.todo(format!(
                            "`{file}` names `{}`; set a numeric version for node",
                            snippet(pinned)
                        ));
                    }
                }
            }
        }
        if let Some(pkg) = package_json(dir) {
            let engine = pkg
                .get("engines")
                .and_then(|e| e.get("node"))
                .and_then(|v| v.as_str())
                .and_then(|range| {
                    version_of(range.trim_start_matches(|c: char| !c.is_ascii_digit()))
                });
            draft.add_dep("node", engine);
        }
    }
}

/// asdf / mise `.tool-versions`: `<tool> <version> [fallback versions…]` per line.
pub struct ToolVersions;

impl Detector for ToolVersions {
    fn detect(&self, dir: &Path, draft: &mut Draft) {
        let Some(content) = read(dir, ".tool-versions") else {
            return;
        };
        for line in content.lines() {
            let line = line.split('#').next().unwrap_or_default();
            let mut parts = line.split_whitespace();
            let (Some(tool), version) = (parts.next(), parts.next()) else {
                continue;
            };
            let name = match tool {
                "dotnet-core" => "dotnet",
                other => other,
            };
            if modules::is_registered(name) {
                draft.add_dep(name, version.and_then(version_of));
            } else {
                draft.todo(format!(
                    "`.tool-versions` lists `{}`, which devy has no module for",
                    snippet(tool)
                ));
            }
        }
    }
}

/// `.ruby-version`, with or without a `ruby-` prefix.
pub struct Ruby;

impl Detector for Ruby {
    fn detect(&self, dir: &Path, draft: &mut Draft) {
        if let Some(content) = read(dir, ".ruby-version") {
            let v = content.trim();
            draft.add_dep("ruby", version_of(v.strip_prefix("ruby-").unwrap_or(v)));
        }
    }
}

/// `.python-version` (pyenv). Only the first listed version is used.
pub struct Python;

impl Detector for Python {
    fn detect(&self, dir: &Path, draft: &mut Draft) {
        if let Some(content) = read(dir, ".python-version") {
            let first = content.lines().map(str::trim).find(|l| !l.is_empty());
            draft.add_dep("python", first.and_then(version_of));
        }
    }
}

/// `rust-toolchain.toml` or legacy `rust-toolchain`. Channels like `stable` carry no version.
pub struct Rust;

impl Detector for Rust {
    fn detect(&self, dir: &Path, draft: &mut Draft) {
        for file in ["rust-toolchain.toml", "rust-toolchain"] {
            if let Some(content) = read(dir, file) {
                draft.add_dep("rust", rust_channel(&content).and_then(version_of));
                return;
            }
        }
    }
}

fn rust_channel(content: &str) -> Option<&str> {
    let channel = content.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "channel").then(|| value.trim().trim_matches(['"', '\'']))
    });
    // The legacy file may hold just the channel name.
    channel.or_else(|| {
        let first = content.lines().next()?.trim();
        (!first.contains(['=', '['])).then_some(first)
    })
}

/// The `go` directive in `go.mod`.
pub struct Go;

impl Detector for Go {
    fn detect(&self, dir: &Path, draft: &mut Draft) {
        if let Some(content) = read(dir, "go.mod") {
            let version = content
                .lines()
                .find_map(|l| l.trim().strip_prefix("go "))
                .and_then(version_of);
            draft.add_dep("go", version);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::fixture;
    use super::*;

    fn deps(
        detector: &dyn Detector,
        files: &[(&str, &str)],
    ) -> (Vec<(String, Option<String>)>, Draft) {
        let dir = fixture(files);
        let mut draft = Draft::default();
        detector.detect(&dir, &mut draft);
        let deps = draft
            .deps
            .iter()
            .map(|d| (d.name.clone(), d.version.clone()))
            .collect();
        (deps, draft)
    }

    fn one(name: &str, version: Option<&str>) -> Vec<(String, Option<String>)> {
        vec![(name.to_string(), version.map(String::from))]
    }

    #[test]
    fn nvmrc() {
        assert_eq!(
            deps(&Node, &[(".nvmrc", "22\n")]).0,
            one("node", Some("22"))
        );
    }

    #[test]
    fn nvmrc_alias_becomes_todo() {
        let (d, draft) = deps(&Node, &[(".nvmrc", "lts/iron\n")]);
        assert_eq!(d, one("node", None));
        assert!(draft.todos[0].contains("lts/iron"), "{:?}", draft.todos);
    }

    #[test]
    fn node_version_file() {
        assert_eq!(
            deps(&Node, &[(".node-version", "v20.11.1")]).0,
            one("node", Some("20.11.1"))
        );
    }

    #[test]
    fn package_json_engines() {
        let pkg = r#"{"engines":{"node":">=18.17 <23"}}"#;
        assert_eq!(
            deps(&Node, &[("package.json", pkg)]).0,
            one("node", Some("18.17"))
        );
    }

    #[test]
    fn nvmrc_wins_over_engines() {
        let pkg = r#"{"engines":{"node":"^18"}}"#;
        let files = [(".nvmrc", "22"), ("package.json", pkg)];
        assert_eq!(deps(&Node, &files).0, one("node", Some("22")));
    }

    #[test]
    fn package_json_without_engines_still_means_node() {
        assert_eq!(deps(&Node, &[("package.json", "{}")]).0, one("node", None));
    }

    #[test]
    fn tool_versions() {
        let content =
            "nodejs 22.1.0\ngolang 1.22.3 1.21.0\n# comment\nterraform 1.8.0\nmadeup 1.0\n";
        let (d, draft) = deps(&ToolVersions, &[(".tool-versions", content)]);
        assert_eq!(
            d,
            vec![
                ("node".to_string(), Some("22.1.0".to_string())),
                ("go".to_string(), Some("1.22.3".to_string())),
                ("terraform".to_string(), Some("1.8.0".to_string())),
            ]
        );
        assert!(draft.todos[0].contains("`madeup`"), "{:?}", draft.todos);
    }

    #[test]
    fn ruby_version() {
        assert_eq!(
            deps(&Ruby, &[(".ruby-version", "ruby-3.3.0\n")]).0,
            one("ruby", Some("3.3.0"))
        );
    }

    #[test]
    fn python_version() {
        assert_eq!(
            deps(&Python, &[(".python-version", "3.12.1\n3.11\n")]).0,
            one("python", Some("3.12.1"))
        );
    }

    #[test]
    fn rust_toolchain_toml() {
        let content = "[toolchain]\nchannel = \"1.78.0\"\ncomponents = [\"clippy\"]\n";
        assert_eq!(
            deps(&Rust, &[("rust-toolchain.toml", content)]).0,
            one("rust", Some("1.78.0"))
        );
    }

    #[test]
    fn legacy_rust_toolchain() {
        assert_eq!(
            deps(&Rust, &[("rust-toolchain", "1.77.2\n")]).0,
            one("rust", Some("1.77.2"))
        );
        assert_eq!(
            deps(&Rust, &[("rust-toolchain", "stable\n")]).0,
            one("rust", None)
        );
    }

    #[test]
    fn go_mod() {
        let content = "module example.com/x\n\ngo 1.22\n\nrequire foo v1\n";
        assert_eq!(deps(&Go, &[("go.mod", content)]).0, one("go", Some("1.22")));
    }
}
