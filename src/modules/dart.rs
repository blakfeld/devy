use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep};

// Flutter projects should use `flutter` instead — it bundles its own Dart SDK.
pub struct DartModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "Dart.Dart",
        "nix" => "dart",
        _ => "dart",
    }
}

impl Module for DartModule {
    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("dart".to_string())
    }

    fn backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency> {
        Some(pm_dep(dep, package_name(pm)))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        super::backend_installed(self, pm, dep)
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        super::install_backend(self, pm, dep)
    }

    fn setup_steps(&self, _dep: &Dependency, project_root: &Path) -> Vec<String> {
        super::helpers::step_if_exists(project_root, "pubspec.yaml", "dart pub get (pubspec.yaml)")
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let pubspec = project_root.join("pubspec.yaml");
        if !pubspec.exists() {
            return Ok(());
        }
        let lock = project_root.join("pubspec.lock");
        let manifest = if lock.exists() { lock } else { pubspec };
        let stamp_path = project_root.join(".devy_dart_stamp");
        if stamp_matches(&stamp_path, &manifest) {
            output::skip("Dart dependencies up to date");
            return Ok(());
        }
        output::step("Running dart pub get");
        let status = Command::new("dart")
            .args(["pub", "get"])
            .current_dir(project_root)
            .status()
            .context("Failed to run `dart pub get`")?;
        if !status.success() {
            anyhow::bail!("`dart pub get` failed — check the output above for details");
        }
        write_stamp(&stamp_path, &manifest)?;
        output::success("Dart dependencies installed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn dart_module_is_not_a_service() {
        assert!(!DartModule.is_service());
    }

    #[test]
    fn package_name_winget() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Dart.Dart");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "dart");
    }

    #[test]
    fn is_installed_true() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            DartModule
                .is_installed(&pm, &Dependency::simple("dart"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = MockPackageManager::default();
        assert!(
            !DartModule
                .is_installed(&pm, &Dependency::simple("dart"))
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
            DartModule
                .install(&pm, &Dependency::simple("dart"))
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
    fn dart_post_setup_no_pubspec_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default();
        assert!(
            DartModule
                .post_setup(&Dependency::simple("dart"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn dart_post_setup_skips_pub_get_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let pubspec = dir.join("pubspec.yaml");
        std::fs::write(&pubspec, "name: test").unwrap();
        std::fs::write(
            dir.join(".devy_dart_stamp"),
            file_mtime_secs(&pubspec).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            DartModule
                .post_setup(&Dependency::simple("dart"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn dart_post_setup_prefers_pubspec_lock_as_stamp_target() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("pubspec.yaml"), "name: test").unwrap();
        let lock = dir.join("pubspec.lock");
        std::fs::write(&lock, "").unwrap();
        std::fs::write(
            dir.join(".devy_dart_stamp"),
            file_mtime_secs(&lock).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            DartModule
                .post_setup(&Dependency::simple("dart"), &pm, &dir)
                .is_ok()
        );
    }
}
