use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep};

pub struct ErlangModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "Erlang-Solutions.Erlang",
        "nix" => "erlang",
        _ => "erlang",
    }
}

impl Module for ErlangModule {
    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("erlang".to_string())
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
            "rebar.config",
            "rebar3 get-deps (rebar.config)",
        )
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let rebar_config = project_root.join("rebar.config");
        if !rebar_config.exists() {
            return Ok(());
        }
        let lock = project_root.join("rebar.lock");
        let manifest = if lock.exists() { lock } else { rebar_config };
        let stamp_path = project_root.join(".devy_erlang_stamp");
        if stamp_matches(&stamp_path, &manifest) {
            output::skip("Erlang dependencies up to date");
            return Ok(());
        }
        output::step("Running rebar3 get-deps");
        let status = Command::new("rebar3")
            .arg("get-deps")
            .current_dir(project_root)
            .status()
            .context("Failed to run `rebar3 get-deps`")?;
        if !status.success() {
            anyhow::bail!("`rebar3 get-deps` failed — check the output above for details");
        }
        write_stamp(&stamp_path, &manifest)?;
        output::success("Erlang dependencies fetched");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn erlang_module_is_not_a_service() {
        assert!(!ErlangModule.is_service());
    }

    #[test]
    fn package_name_winget() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Erlang-Solutions.Erlang");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "erlang");
    }

    #[test]
    fn package_name_apt_default() {
        let pm = MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "erlang");
    }

    #[test]
    fn is_installed_true() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            ErlangModule
                .is_installed(&pm, &Dependency::simple("erlang"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = MockPackageManager::default();
        assert!(
            !ErlangModule
                .is_installed(&pm, &Dependency::simple("erlang"))
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
            ErlangModule
                .install(&pm, &Dependency::simple("erlang"))
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
    fn erlang_post_setup_no_rebar_config_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default();
        assert!(
            ErlangModule
                .post_setup(&Dependency::simple("erlang"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn erlang_post_setup_skips_rebar3_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let rebar_config = dir.join("rebar.config");
        std::fs::write(&rebar_config, "").unwrap();
        std::fs::write(
            dir.join(".devy_erlang_stamp"),
            file_mtime_secs(&rebar_config).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            ErlangModule
                .post_setup(&Dependency::simple("erlang"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn erlang_post_setup_prefers_rebar_lock_as_stamp_target() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("rebar.config"), "").unwrap();
        let lock = dir.join("rebar.lock");
        std::fs::write(&lock, "").unwrap();
        std::fs::write(
            dir.join(".devy_erlang_stamp"),
            file_mtime_secs(&lock).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            ErlangModule
                .post_setup(&Dependency::simple("erlang"), &pm, &dir)
                .is_ok()
        );
    }
}
