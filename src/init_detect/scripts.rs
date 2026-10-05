//! `commands:` from `package.json` scripts.

use std::path::Path;

use super::{Detector, Draft, is_plain_file, package_json, snippet};

pub struct PackageScripts;

impl Detector for PackageScripts {
    fn detect(&self, dir: &Path, draft: &mut Draft) {
        let Some(pkg) = package_json(dir) else {
            return;
        };
        let Some(scripts) = pkg.get("scripts").and_then(|s| s.as_object()) else {
            return;
        };
        let pm = node_package_manager(dir);
        let builtins = crate::cli::builtin_subcommands();
        for name in scripts.keys() {
            if !crate::validate::command_name(name) {
                draft.todo(format!(
                    "package.json script `{}` is not a valid devy command name; add it under another name",
                    snippet(name)
                ));
            } else if builtins.contains(name) {
                draft.todo(format!(
                    "package.json script `{name}` clashes with `devy {name}`; add it under another name"
                ));
            } else {
                draft.add_command(name, format!("{pm} run {name}"));
            }
        }
    }
}

/// The Node package manager a project uses, from its lockfile.
fn node_package_manager(dir: &Path) -> &'static str {
    const LOCKFILES: &[(&str, &str)] = &[
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lock", "bun"),
        ("bun.lockb", "bun"),
    ];
    LOCKFILES
        .iter()
        .find(|(file, _)| is_plain_file(dir, file))
        .map_or("npm", |(_, pm)| pm)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::fixture;
    use super::*;

    const PKG: &str = r#"{"scripts":{"dev":"vite","test":"vitest","start":"node ."}}"#;

    fn commands(files: &[(&str, &str)]) -> Draft {
        let dir = fixture(files);
        let mut draft = Draft::default();
        PackageScripts.detect(&dir, &mut draft);
        draft
    }

    #[test]
    fn invalid_script_names_become_todos() {
        let pkg = r#"{"scripts":{"//":"a comment","_postinstall":"x","dev":"vite"}}"#;
        let draft = commands(&[("package.json", pkg)]);
        let names: Vec<&str> = draft.commands.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["dev"]);
        assert!(
            draft.todos.iter().any(|t| t.contains("`//`")),
            "{:?}",
            draft.todos
        );
        assert!(draft.todos.iter().any(|t| t.contains("`_postinstall`")));
    }

    #[test]
    fn npm_by_default() {
        let draft = commands(&[("package.json", PKG), ("package-lock.json", "{}")]);
        assert!(
            draft
                .commands
                .contains(&("dev".into(), "npm run dev".into()))
        );
        assert!(
            draft
                .commands
                .contains(&("test".into(), "npm run test".into()))
        );
    }

    #[test]
    fn package_manager_from_lockfile() {
        for (lock, pm) in [
            ("pnpm-lock.yaml", "pnpm"),
            ("yarn.lock", "yarn"),
            ("bun.lockb", "bun"),
            ("bun.lock", "bun"),
        ] {
            let draft = commands(&[("package.json", PKG), (lock, "")]);
            assert!(
                draft
                    .commands
                    .contains(&("dev".into(), format!("{pm} run dev"))),
                "{lock}: {:?}",
                draft.commands
            );
        }
    }

    #[test]
    fn builtin_names_become_todos() {
        let draft = commands(&[("package.json", PKG)]);
        assert!(!draft.commands.iter().any(|(n, _)| n == "start"));
        assert!(draft.todos[0].contains("`start`"), "{:?}", draft.todos);
    }

    #[test]
    fn no_scripts_no_commands() {
        assert!(commands(&[("package.json", "{}")]).commands.is_empty());
    }
}
