use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep};

// apt requires the Crystal apt repository; see https://crystal-lang.org/install/on_debian_and_ubuntu/
pub struct CrystalModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "Manas.Crystal",
        "nix" => "crystal",
        _ => "crystal",
    }
}

impl Module for CrystalModule {
    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("crystal".to_string())
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
        super::helpers::step_if_exists(
            project_root,
            "shard.yml",
            "shards install (shard.yml postinstall scripts)",
        )
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let shard_yml = project_root.join("shard.yml");
        if !shard_yml.exists() {
            return Ok(());
        }
        let lock = project_root.join("shard.lock");
        let manifest = if lock.exists() { lock } else { shard_yml };
        let stamp_path = project_root.join(".devy_crystal_stamp");
        if stamp_matches(&stamp_path, &manifest) {
            output::skip("Crystal dependencies up to date");
            return Ok(());
        }
        output::step("Running shards install");
        let status = Command::new("shards")
            .arg("install")
            .current_dir(project_root)
            .status()
            .context("Failed to run `shards install`")?;
        if !status.success() {
            anyhow::bail!("`shards install` failed — check the output above for details");
        }
        write_stamp(&stamp_path, &manifest)?;
        output::success("Crystal dependencies installed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn crystal_module_is_not_a_service() {
        assert!(!CrystalModule.is_service());
    }

    #[test]
    fn package_name_winget() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Manas.Crystal");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "crystal");
    }

    #[test]
    fn is_installed_true() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            CrystalModule
                .is_installed(&pm, &Dependency::simple("crystal"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = MockPackageManager::default();
        assert!(
            !CrystalModule
                .is_installed(&pm, &Dependency::simple("crystal"))
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
            CrystalModule
                .install(&pm, &Dependency::simple("crystal"))
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
    fn crystal_post_setup_no_shard_yml_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default();
        assert!(
            CrystalModule
                .post_setup(&Dependency::simple("crystal"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn crystal_post_setup_skips_shards_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let shard_yml = dir.join("shard.yml");
        std::fs::write(&shard_yml, "name: test").unwrap();
        std::fs::write(
            dir.join(".devy_crystal_stamp"),
            file_mtime_secs(&shard_yml).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            CrystalModule
                .post_setup(&Dependency::simple("crystal"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn crystal_post_setup_prefers_shard_lock_as_stamp_target() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("shard.yml"), "name: test").unwrap();
        let lock = dir.join("shard.lock");
        std::fs::write(&lock, "").unwrap();
        std::fs::write(
            dir.join(".devy_crystal_stamp"),
            file_mtime_secs(&lock).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            CrystalModule
                .post_setup(&Dependency::simple("crystal"), &pm, &dir)
                .is_ok()
        );
    }
}
