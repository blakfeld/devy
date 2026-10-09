use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::node::{detect_node_pm, npm_global_install_args};
use super::{Module, extra_list, node_pkg, run_cmd};

pub struct TypeScriptModule;

impl Module for TypeScriptModule {
    fn nix_versioned_attr(&self, version: &str) -> Option<String> {
        super::node::nix_versioned_node(version)
    }

    fn nix_attr(&self, dep: &Dependency) -> Option<String> {
        Some(super::nix_install_attr(self, dep, "nodejs"))
    }

    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["global_packages"])
    }

    fn backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency> {
        Some(super::pkg_dep(self, pm, dep, node_pkg(pm)))
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
        super::install_backend(self, pm, dep)?;

        let mut globals = vec!["typescript".to_string()];
        globals.extend(extra_list(dep, "global_packages")?);

        run_cmd("npm", &npm_global_install_args(&globals)?)?;

        Ok(())
    }

    fn setup_steps(&self, dep: &Dependency, project_root: &Path) -> Vec<String> {
        let mut globals = vec!["typescript".to_string()];
        globals.extend(extra_list(dep, "global_packages").unwrap_or_default());
        let mut steps = vec![format!(
            "npm install -g {} {}",
            globals.join(" "),
            super::GLOBAL_PACKAGES_STEP
        )];
        if project_root.join("package.json").exists() {
            let (pm_cmd, _) = super::node::detect_node_pm(project_root);
            steps.push(format!("{pm_cmd} install (package.json lifecycle scripts)"));
        }
        steps
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        if !project_root.join("package.json").exists() {
            return Ok(());
        }
        let (pm_cmd, lockfile) = detect_node_pm(project_root);
        let package_json = project_root.join("package.json");
        let manifest: &Path = lockfile.as_deref().unwrap_or(&package_json);
        let stamp_path = project_root.join(".devy_ts_local_stamp");
        if stamp_matches(&stamp_path, manifest) {
            output::skip("Node dependencies up to date");
            return Ok(());
        }
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn typescript_module_is_not_a_service() {
        assert!(!TypeScriptModule.is_service());
    }

    #[test]
    fn typescript_is_installed_checks_node_formula() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let dep = Dependency::simple("typescript");
        assert!(TypeScriptModule.is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn typescript_source_is_none() {
        assert_eq!(TypeScriptModule.source(), None);
    }

    #[test]
    fn typescript_is_installed_false() {
        let pm = MockPackageManager::default();
        assert!(
            !TypeScriptModule
                .is_installed(&pm, &Dependency::simple("typescript"))
                .unwrap()
        );
    }

    #[test]
    fn typescript_install_propagates_pm_error() {
        // When pm.install_package fails, install must return Err (not Ok(())).
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("typescript");
        assert!(TypeScriptModule.install(&pm, &dep).is_err());
    }

    fn file_mtime_secs(path: &std::path::Path) -> u64 {
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn typescript_post_setup_no_package_json_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default();
        assert!(
            TypeScriptModule
                .post_setup(&Dependency::simple("typescript"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn typescript_post_setup_skips_install_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let pkg_json = dir.join("package.json");
        std::fs::write(&pkg_json, r#"{"name":"test"}"#).unwrap();
        std::fs::write(
            dir.join(".devy_ts_local_stamp"),
            file_mtime_secs(&pkg_json).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            TypeScriptModule
                .post_setup(&Dependency::simple("typescript"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn typescript_install_calls_install_package() {
        // Verify install_package is actually called (kills replace install -> Ok(())).
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("typescript");
        // install calls pm.install_package then run_cmd("npm", ...).
        // pm.install_package succeeds; npm may fail if not on PATH — both are Err-worthy.
        // The important thing: with mutation Ok(()), install_package would NOT be called.
        let _ = TypeScriptModule.install(&pm, &dep);
        // install_package must have been called (even if npm then fails)
        assert!(
            !pm.installed_packages.borrow().is_empty(),
            "install must call pm.install_package"
        );
    }

    #[test]
    fn typescript_accepts_global_packages_key() {
        let known = TypeScriptModule.known_extra_keys().unwrap();
        assert!(known.contains(&"global_packages"));
    }

    #[test]
    fn typescript_install_rejects_hostile_global_package_before_npm() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "global_packages".into(),
            crate::config::ExtraValue::Sequence(vec![crate::config::ExtraValue::String(
                "--registry=https://evil".into(),
            )]),
        );
        let dep = Dependency::with_extra("typescript", extra);
        let err = TypeScriptModule
            .install(&MockPackageManager::default(), &dep)
            .unwrap_err();
        assert!(err.to_string().contains("invalid list entry"), "{err}");
    }
}
