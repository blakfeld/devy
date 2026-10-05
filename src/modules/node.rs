use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, extra_list, node_pkg, run_cmd};

pub struct NodeModule;

/// Detects the Node package manager from lockfiles.
/// Returns the command name and the lockfile to use as a stamp target.
pub(crate) fn detect_node_pm(project_root: &Path) -> (&'static str, Option<PathBuf>) {
    let pnpm = project_root.join("pnpm-lock.yaml");
    if pnpm.exists() {
        return ("pnpm", Some(pnpm));
    }
    let yarn = project_root.join("yarn.lock");
    if yarn.exists() {
        return ("yarn", Some(yarn));
    }
    let npm_lock = project_root.join("package-lock.json");
    if npm_lock.exists() {
        return ("npm", Some(npm_lock));
    }
    ("npm", None)
}

/// Builds `npm install -g -- <packages...>`. Packages follow `--` so a configured value can
/// never be parsed as an npm option, and each must be a registry spec as defined by
/// [`crate::validate::list_entry`] — the same rule `devy check` applies when the config is
/// loaded, so a value accepted there is never refused here (and vice versa).
/// Shared with the typescript module.
pub(super) fn npm_global_install_args(packages: &[String]) -> Result<Vec<&str>> {
    if let Some(bad) = packages.iter().find(|p| !crate::validate::list_entry(p)) {
        anyhow::bail!(
            "invalid global npm package '{bad}': only registry package names are allowed"
        );
    }
    let mut args = vec!["install", "-g", "--"];
    args.extend(packages.iter().map(String::as_str));
    Ok(args)
}

/// `22` or `22.11.0` → `nodejs_22`, for Node majors nixpkgs carries.
/// Shared with the typescript module, which installs the same Node.
pub(super) fn nix_versioned_node(version: &str) -> Option<String> {
    super::helpers::allowlisted_attr(version, 1, &["22", "24"], |v| format!("nodejs_{v}"))
}

impl Module for NodeModule {
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["global_packages"])
    }

    fn nix_versioned_attr(&self, version: &str) -> Option<String> {
        nix_versioned_node(version)
    }

    fn nix_attr(&self, dep: &Dependency) -> Option<String> {
        Some(super::nix_install_attr(self, dep, "nodejs"))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        super::pkg_installed(self, pm, dep, node_pkg(pm))
    }

    fn resolved_version(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
    ) -> Result<Option<String>> {
        super::pkg_resolved_version(self, pm, dep, node_pkg(pm))
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.install_package(&super::pkg_dep(self, pm, dep, node_pkg(pm)))
    }

    fn setup_steps(&self, dep: &Dependency, project_root: &Path) -> Vec<String> {
        let mut steps = Vec::new();
        if project_root.join("package.json").exists() {
            let (pm_cmd, _) = detect_node_pm(project_root);
            steps.push(format!("{pm_cmd} install (package.json lifecycle scripts)"));
        }
        if let Ok(globals) = extra_list(dep, "global_packages")
            && !globals.is_empty()
        {
            steps.push(format!(
                "npm install -g {} {}",
                globals.join(" "),
                super::GLOBAL_PACKAGES_STEP
            ));
        }
        steps
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        if project_root.join("package.json").exists() {
            let (pm_cmd, lockfile) = detect_node_pm(project_root);
            let package_json = project_root.join("package.json");
            let manifest: &Path = lockfile.as_deref().unwrap_or(&package_json);
            let stamp_path = project_root.join(".devy_node_local_stamp");
            if stamp_matches(&stamp_path, manifest) {
                output::skip("Node dependencies up to date");
            } else {
                output::step(&format!("Running {pm_cmd} install"));
                let status = Command::new(pm_cmd)
                    .arg("install")
                    .current_dir(project_root)
                    .status()
                    .with_context(|| format!("Failed to run `{pm_cmd} install`"))?;
                if !status.success() {
                    anyhow::bail!("`{pm_cmd} install` failed — check the output above for details");
                }
                write_stamp(&stamp_path, manifest)?;
                output::success(&format!("{pm_cmd} install complete"));
            }
        }

        let packages = extra_list(dep, "global_packages")?;
        if packages.is_empty() {
            return Ok(());
        }
        let stamp = project_root.join(".devy_node_global_stamp");
        let current = packages.join("\n");
        if std::fs::read_to_string(&stamp).ok().as_deref() == Some(current.as_str()) {
            return Ok(());
        }
        run_cmd("npm", &npm_global_install_args(&packages)?)?;
        super::helpers::write_stamp_text(&stamp, &current)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;
    use std::collections::HashMap;

    #[test]
    fn npm_global_install_args_use_separator() {
        let pkgs = vec![
            "typescript".to_string(),
            "eslint@9".to_string(),
            "@angular/cli@^17.0.0".to_string(),
        ];
        assert!(
            npm_global_install_args(&[
                "pkg@~1.2".to_string(),
                "pkg@latest".to_string(),
                "pkg@>=1.0".to_string()
            ])
            .is_ok()
        );
        assert_eq!(
            npm_global_install_args(&pkgs).unwrap(),
            [
                "install",
                "-g",
                "--",
                "typescript",
                "eslint@9",
                "@angular/cli@^17.0.0"
            ]
        );
    }

    #[test]
    fn npm_global_install_args_accept_what_config_validation_accepts() {
        // npm aliases pass `devy check` (validate::list_entry), so `up` must accept them too.
        for good in ["ts@npm:typescript@5", "x@npm:@scope/pkg", "@a/b@npm:c@^1.2"] {
            assert!(crate::validate::list_entry(good), "{good}");
            assert!(
                npm_global_install_args(&[good.to_string()]).is_ok(),
                "{good}"
            );
        }
    }

    #[test]
    fn npm_global_install_args_reject_non_registry_specs() {
        for bad in [
            "-g",
            "--prefix=/tmp",
            "user/repo",
            "file:../x",
            "./dir",
            "/abs",
            "~/x",
            "git+https://example.com/x.git",
            "@scope",
            "@/name",
            "@scope/name/extra",
            "a b",
            ".hidden",
            "evil.tgz",
            "evil.TAR.GZ",
            "pkg@.",
            "pkg@..",
            "pkg@./x",
            "pkg@~/x",
            "pkg@x.tgz",
            "pkg@",
            "@scope/name@.",
            "pkg@1|2",
            "pkg@npm:file:../x",
            "pkg@npm:user/repo",
        ] {
            assert!(
                npm_global_install_args(&[bad.to_string()]).is_err(),
                "{bad} must be rejected"
            );
            assert!(
                !crate::validate::list_entry(bad),
                "{bad} must fail config validation"
            );
        }
    }

    fn dep_with_global_packages(pkgs: &[&str]) -> Dependency {
        let mut extra = HashMap::new();
        extra.insert(
            "global_packages".into(),
            crate::config::ExtraValue::Sequence(
                pkgs.iter()
                    .map(|s| crate::config::ExtraValue::String((*s).into()))
                    .collect(),
            ),
        );
        Dependency::with_extra("node", extra)
    }

    #[test]
    fn node_module_is_not_a_service() {
        assert!(!NodeModule.is_service());
    }

    #[test]
    fn node_is_installed_delegates_to_pm() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let dep = Dependency::simple("node");
        assert!(NodeModule.is_installed(&pm, &dep).unwrap());

        let pm2 = MockPackageManager::default();
        assert!(!NodeModule.is_installed(&pm2, &dep).unwrap());
    }

    #[test]
    fn node_install_without_global_packages_does_not_run_npm() {
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("node");
        assert!(NodeModule.install(&pm, &dep).is_ok());
    }

    #[test]
    fn node_install_propagates_pm_error() {
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        assert!(
            NodeModule
                .install(&pm, &Dependency::simple("node"))
                .is_err()
        );
    }

    #[test]
    fn node_post_setup_no_global_packages_is_noop() {
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("node");
        assert!(
            NodeModule
                .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
                .is_ok(),
            "post_setup with no global_packages must return Ok"
        );
    }

    #[test]
    fn node_post_setup_skips_npm_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let dep = dep_with_global_packages(&["typescript"]);
        let stamp = dir.join(".devy_node_global_stamp");
        // Write a stamp that already matches the package list.
        std::fs::write(&stamp, "typescript").unwrap();
        let pm = MockPackageManager::default();
        // If the stamp check is bypassed, `npm` would be invoked and likely fail,
        // returning Err. A successful return proves the stamp short-circuited.
        let result = NodeModule.post_setup(&dep, &pm, &dir);
        assert!(
            result.is_ok(),
            "post_setup must skip npm when stamp matches"
        );
        assert_eq!(
            std::fs::read_to_string(&stamp).unwrap(),
            "typescript",
            "stamp must be unchanged when skipped"
        );
    }

    #[test]
    fn node_post_setup_stamp_matches_multiple_packages() {
        let dir = crate::test_support::tmp_dir();
        let dep = dep_with_global_packages(&["typescript", "eslint"]);
        let stamp = dir.join(".devy_node_global_stamp");
        std::fs::write(&stamp, "typescript\neslint").unwrap();
        let pm = MockPackageManager::default();
        let result = NodeModule.post_setup(&dep, &pm, &dir);
        assert!(
            result.is_ok(),
            "post_setup must skip npm when stamp matches multiple packages"
        );
    }

    // ── detect_node_pm ────────────────────────────────────────────────────────

    #[test]
    fn detect_node_pm_defaults_to_npm_when_no_lockfile() {
        let dir = crate::test_support::tmp_dir();
        let (cmd, lockfile) = detect_node_pm(&dir);
        assert_eq!(cmd, "npm");
        assert!(lockfile.is_none());
    }

    #[test]
    fn detect_node_pm_detects_pnpm_from_lockfile() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        let (cmd, lockfile) = detect_node_pm(&dir);
        assert_eq!(cmd, "pnpm");
        assert!(lockfile.unwrap().ends_with("pnpm-lock.yaml"));
    }

    #[test]
    fn detect_node_pm_detects_yarn_from_lockfile() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("yarn.lock"), "").unwrap();
        let (cmd, lockfile) = detect_node_pm(&dir);
        assert_eq!(cmd, "yarn");
        assert!(lockfile.unwrap().ends_with("yarn.lock"));
    }

    #[test]
    fn detect_node_pm_detects_npm_from_lockfile() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("package-lock.json"), "").unwrap();
        let (cmd, lockfile) = detect_node_pm(&dir);
        assert_eq!(cmd, "npm");
        assert!(lockfile.unwrap().ends_with("package-lock.json"));
    }

    #[test]
    fn detect_node_pm_pnpm_takes_priority_over_yarn() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(dir.join("yarn.lock"), "").unwrap();
        let (cmd, _) = detect_node_pm(&dir);
        assert_eq!(
            cmd, "pnpm",
            "pnpm-lock.yaml must take priority over yarn.lock"
        );
    }

    #[test]
    fn node_post_setup_skips_local_install_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let pkg_json = dir.join("package.json");
        std::fs::write(&pkg_json, r#"{"name":"test"}"#).unwrap();
        let mtime = std::fs::metadata(&pkg_json)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        std::fs::write(dir.join(".devy_node_local_stamp"), mtime.to_string()).unwrap();
        let dep = Dependency::simple("node");
        let pm = MockPackageManager::default();
        // npm would be invoked and fail if stamp check is bypassed.
        let result = NodeModule.post_setup(&dep, &pm, &dir);
        assert!(
            result.is_ok(),
            "post_setup must skip install when stamp matches"
        );
    }

    #[test]
    fn node_post_setup_rejects_hostile_global_package_before_npm() {
        let dir = crate::test_support::tmp_dir();
        let dep = dep_with_global_packages(&["./x"]);
        // A matching stamp would skip npm; validation must still fail first.
        std::fs::write(dir.join(".devy_node_global_stamp"), "./x").unwrap();
        let err = NodeModule
            .post_setup(&dep, &MockPackageManager::default(), &dir)
            .unwrap_err();
        assert!(err.to_string().contains("invalid list entry"), "{err}");
    }
}
