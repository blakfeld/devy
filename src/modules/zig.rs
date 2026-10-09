use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep};

// apt: `zig` may not be available in standard Ubuntu repos on older releases.
// Users may need to add a PPA or install from https://ziglang.org/download/
pub struct ZigModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "zig-lang.zig",
        "nix" => "zig",
        _ => "zig",
    }
}

impl Module for ZigModule {
    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("zig".to_string())
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
        super::helpers::step_if_exists(project_root, "build.zig.zon", "zig build (build.zig)")
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let build_zon = project_root.join("build.zig.zon");
        if !build_zon.exists() {
            return Ok(());
        }
        let stamp_path = project_root.join(".devy_zig_stamp");
        if stamp_matches(&stamp_path, &build_zon) {
            output::skip("Zig dependencies up to date");
            return Ok(());
        }
        output::step("Running zig build");
        let status = Command::new("zig")
            .arg("build")
            .current_dir(project_root)
            .status()
            .context("Failed to run `zig build`")?;
        if !status.success() {
            anyhow::bail!("`zig build` failed — check the output above for details");
        }
        write_stamp(&stamp_path, &build_zon)?;
        output::success("Zig project built");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn zig_module_is_not_a_service() {
        assert!(!ZigModule.is_service());
    }

    #[test]
    fn package_name_winget() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "zig-lang.zig");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "zig");
    }

    #[test]
    fn is_installed_true() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            ZigModule
                .is_installed(&pm, &Dependency::simple("zig"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = MockPackageManager::default();
        assert!(
            !ZigModule
                .is_installed(&pm, &Dependency::simple("zig"))
                .unwrap()
        );
    }

    #[test]
    fn install_propagates_pm_error() {
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        assert!(ZigModule.install(&pm, &Dependency::simple("zig")).is_err());
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
    fn zig_post_setup_no_build_zon_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default();
        assert!(
            ZigModule
                .post_setup(&Dependency::simple("zig"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn zig_post_setup_skips_build_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let zon = dir.join("build.zig.zon");
        std::fs::write(&zon, "").unwrap();
        std::fs::write(
            dir.join(".devy_zig_stamp"),
            file_mtime_secs(&zon).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            ZigModule
                .post_setup(&Dependency::simple("zig"), &pm, &dir)
                .is_ok()
        );
    }
}
