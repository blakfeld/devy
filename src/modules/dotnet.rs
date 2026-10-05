use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep};

pub struct DotnetModule;

/// Extracts the major version number from a version string like "8", "8.0", or "8.0.1".
fn major_version(dep: &Dependency) -> u32 {
    dep.version
        .as_deref()
        .and_then(|v| v.split('.').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(8)
}

/// The alphabetically first `.sln` (preferred) or `.csproj` in the project root, so
/// the choice does not depend on directory-listing order.
fn find_dotnet_manifest(project_root: &Path) -> Option<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(project_root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    let with_ext = |ext: &str| {
        paths
            .iter()
            .find(|p| p.extension().and_then(|e| e.to_str()) == Some(ext))
            .cloned()
    };
    with_ext("sln").or_else(|| with_ext("csproj"))
}

/// The SDK nixpkgs installs when no supported version is given.
const NIX_DEFAULT_SDK: &str = "dotnet-sdk_8";

fn package_name(pm: &dyn PackageManager, dep: &Dependency) -> String {
    let major = major_version(dep);
    match pm.name() {
        "apt" => format!("dotnet-sdk-{major}.0"),
        "winget" => format!("Microsoft.DotNet.SDK.{major}"),
        "nix" => super::nix_install_attr(&DotnetModule, dep, NIX_DEFAULT_SDK),
        _ => "dotnet".to_string(),
    }
}

impl Module for DotnetModule {
    fn nix_versioned_attr(&self, version: &str) -> Option<String> {
        // `8` or `8.0.100` → `dotnet-sdk_8`.
        super::helpers::allowlisted_attr(version, 1, &["6", "7", "8", "9", "10"], |v| {
            format!("dotnet-sdk_{v}")
        })
    }

    fn nix_attr(&self, dep: &Dependency) -> Option<String> {
        Some(super::nix_install_attr(self, dep, NIX_DEFAULT_SDK))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        if pm.name() == "nix" {
            return super::pkg_installed(self, pm, dep, NIX_DEFAULT_SDK);
        }
        let name = package_name(pm, dep);
        pm.is_package_installed(&pm_dep(dep, &name))
    }

    fn resolved_version(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
    ) -> Result<Option<String>> {
        super::pkg_resolved_version(self, pm, dep, NIX_DEFAULT_SDK)
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        let name = package_name(pm, dep);
        pm.install_package(&pm_dep(dep, &name))
    }

    fn setup_steps(&self, _dep: &Dependency, project_root: &Path) -> Vec<String> {
        find_dotnet_manifest(project_root)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .map(|name| vec![format!("dotnet restore ({name} MSBuild targets)")])
            .unwrap_or_default()
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let Some(manifest) = find_dotnet_manifest(project_root) else {
            return Ok(());
        };
        let stamp_path = project_root.join(".devy_dotnet_stamp");
        if stamp_matches(&stamp_path, &manifest) {
            output::skip(".NET dependencies up to date");
            return Ok(());
        }
        output::step("Running dotnet restore");
        let status = Command::new("dotnet")
            .arg("restore")
            .current_dir(project_root)
            .status()
            .context("Failed to run `dotnet restore`")?;
        if !status.success() {
            anyhow::bail!("`dotnet restore` failed — check the output above for details");
        }
        write_stamp(&stamp_path, &manifest)?;
        output::success(".NET dependencies restored");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep_with_version(v: &str) -> Dependency {
        let mut dep = Dependency::simple("dotnet");
        dep.version = Some(v.into());
        dep
    }

    #[test]
    fn dotnet_module_is_not_a_service() {
        assert!(!DotnetModule.is_service());
    }

    #[test]
    fn major_version_defaults_to_8() {
        let dep = Dependency::simple("dotnet");
        assert_eq!(major_version(&dep), 8);
    }

    #[test]
    fn major_version_from_bare_number() {
        assert_eq!(major_version(&dep_with_version("7")), 7);
    }

    #[test]
    fn major_version_from_full_semver() {
        assert_eq!(major_version(&dep_with_version("6.0.1")), 6);
    }

    #[test]
    fn apt_package_name_includes_major_version() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm, &dep_with_version("8")), "dotnet-sdk-8.0");
        assert_eq!(package_name(&pm, &dep_with_version("6")), "dotnet-sdk-6.0");
    }

    #[test]
    fn winget_package_name_includes_major_version() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(
            package_name(&pm, &dep_with_version("8")),
            "Microsoft.DotNet.SDK.8"
        );
    }

    #[test]
    fn brew_package_name_is_dotnet() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm, &dep_with_version("9")), "dotnet");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            DotnetModule
                .is_installed(&pm, &Dependency::simple("dotnet"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !DotnetModule
                .is_installed(&pm, &Dependency::simple("dotnet"))
                .unwrap()
        );
    }

    #[test]
    fn install_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        assert!(
            DotnetModule
                .install(&pm, &Dependency::simple("dotnet"))
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
    fn find_dotnet_manifest_returns_none_for_empty_dir() {
        let dir = crate::test_support::tmp_dir();
        assert!(find_dotnet_manifest(&dir).is_none());
    }

    #[test]
    fn find_dotnet_manifest_finds_csproj() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("MyApp.csproj"), "").unwrap();
        assert!(
            find_dotnet_manifest(&dir)
                .unwrap()
                .ends_with("MyApp.csproj")
        );
    }

    #[test]
    fn find_dotnet_manifest_prefers_sln_over_csproj() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("MyApp.csproj"), "").unwrap();
        std::fs::write(dir.join("Solution.sln"), "").unwrap();
        assert!(
            find_dotnet_manifest(&dir)
                .unwrap()
                .ends_with("Solution.sln")
        );
    }

    #[test]
    fn dotnet_post_setup_no_manifest_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            DotnetModule
                .post_setup(&Dependency::simple("dotnet"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn dotnet_post_setup_skips_restore_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let csproj = dir.join("App.csproj");
        std::fs::write(&csproj, "").unwrap();
        std::fs::write(
            dir.join(".devy_dotnet_stamp"),
            file_mtime_secs(&csproj).to_string(),
        )
        .unwrap();
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            DotnetModule
                .post_setup(&Dependency::simple("dotnet"), &pm, &dir)
                .is_ok()
        );
    }
}
