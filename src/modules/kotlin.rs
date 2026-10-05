use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep};

pub struct KotlinModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "JetBrains.Kotlin",
        "nix" => "kotlin",
        _ => "kotlin",
    }
}

impl Module for KotlinModule {
    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("kotlin".to_string())
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        pm.is_package_installed(&pm_dep(dep, package_name(pm)))
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.install_package(&pm_dep(dep, package_name(pm)))
    }

    fn setup_steps(&self, _dep: &Dependency, project_root: &Path) -> Vec<String> {
        super::helpers::gradle_steps(project_root)
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let gradle_kts = project_root.join("build.gradle.kts");
        let gradle = project_root.join("build.gradle");
        let manifest = if gradle_kts.exists() {
            Some(gradle_kts)
        } else if gradle.exists() {
            Some(gradle)
        } else {
            None
        };
        let Some(manifest) = manifest else {
            return Ok(());
        };
        let stamp_path = project_root.join(".devy_kotlin_stamp");
        if stamp_matches(&stamp_path, &manifest) {
            output::skip("Kotlin dependencies up to date");
            return Ok(());
        }
        let gradlew = if project_root.join("gradlew").exists() {
            "./gradlew"
        } else {
            "gradle"
        };
        output::step("Running gradle dependencies");
        let status = Command::new(gradlew)
            .args(["--no-daemon", "dependencies"])
            .current_dir(project_root)
            .status()
            .with_context(|| format!("Failed to run `{gradlew} dependencies`"))?;
        if !status.success() {
            anyhow::bail!("`{gradlew} dependencies` failed — check the output above for details");
        }
        write_stamp(&stamp_path, &manifest)?;
        output::success("Gradle dependencies resolved");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn kotlin_module_is_not_a_service() {
        assert!(!KotlinModule.is_service());
    }

    #[test]
    fn package_name_brew_default() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "kotlin");
    }

    #[test]
    fn package_name_apt_default() {
        let pm = MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "kotlin");
    }

    #[test]
    fn package_name_winget() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "JetBrains.Kotlin");
    }

    #[test]
    fn is_installed_true() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            KotlinModule
                .is_installed(&pm, &Dependency::simple("kotlin"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = MockPackageManager::default();
        assert!(
            !KotlinModule
                .is_installed(&pm, &Dependency::simple("kotlin"))
                .unwrap()
        );
    }

    #[test]
    fn install_propagates_pm_error() {
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        assert!(
            KotlinModule
                .install(&pm, &Dependency::simple("kotlin"))
                .is_err()
        );
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
    fn kotlin_post_setup_no_build_file_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default();
        assert!(
            KotlinModule
                .post_setup(&Dependency::simple("kotlin"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn kotlin_post_setup_skips_gradle_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let build = dir.join("build.gradle.kts");
        std::fs::write(&build, "").unwrap();
        std::fs::write(
            dir.join(".devy_kotlin_stamp"),
            file_mtime_secs(&build).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            KotlinModule
                .post_setup(&Dependency::simple("kotlin"), &pm, &dir)
                .is_ok()
        );
    }
}
