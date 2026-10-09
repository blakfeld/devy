use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep};

pub struct ElixirModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "Erlang-Solutions.Elixir",
        "nix" => "elixir",
        _ => "elixir",
    }
}

impl Module for ElixirModule {
    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("elixir".to_string())
    }

    fn backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency> {
        Some(pm_dep(dep, package_name(pm)))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        super::backend_installed(self, pm, dep)
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        // Elixir requires Erlang — install it first if not already present.
        let erlang_dep = Dependency::simple("erlang");
        let erlang = super::get("erlang");
        if !erlang.is_installed(pm, &erlang_dep)? {
            output::step("Installing erlang (required by elixir)");
            erlang.install(pm, &erlang_dep)?;
            output::success("erlang installed");
        }
        super::install_backend(self, pm, dep)
    }

    fn setup_steps(&self, _dep: &Dependency, project_root: &Path) -> Vec<String> {
        super::helpers::step_if_exists(project_root, "mix.exs", "mix deps.get (mix.exs)")
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let mix_exs = project_root.join("mix.exs");
        if !mix_exs.exists() {
            return Ok(());
        }
        let lock = project_root.join("mix.lock");
        let manifest = if lock.exists() { lock } else { mix_exs };
        let stamp_path = project_root.join(".devy_elixir_stamp");
        if stamp_matches(&stamp_path, &manifest) {
            output::skip("Elixir dependencies up to date");
            return Ok(());
        }
        output::step("Running mix deps.get");
        let status = Command::new("mix")
            .arg("deps.get")
            .current_dir(project_root)
            .status()
            .context("Failed to run `mix deps.get`")?;
        if !status.success() {
            anyhow::bail!("`mix deps.get` failed — check the output above for details");
        }
        write_stamp(&stamp_path, &manifest)?;
        output::success("Elixir dependencies installed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn elixir_module_is_not_a_service() {
        assert!(!ElixirModule.is_service());
    }

    #[test]
    fn package_name_winget() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Erlang-Solutions.Elixir");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "elixir");
    }

    #[test]
    fn is_installed_true() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            ElixirModule
                .is_installed(&pm, &Dependency::simple("elixir"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = MockPackageManager::default();
        assert!(
            !ElixirModule
                .is_installed(&pm, &Dependency::simple("elixir"))
                .unwrap()
        );
    }

    #[test]
    fn install_installs_erlang_first_when_not_present() {
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("elixir");
        assert!(ElixirModule.install(&pm, &dep).is_ok());
    }

    #[test]
    fn install_skips_erlang_when_already_installed() {
        // With installed=true, erlang is already present — install should not try to install it.
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let dep = Dependency::simple("elixir");
        assert!(ElixirModule.install(&pm, &dep).is_ok());
    }

    #[test]
    fn install_propagates_pm_error() {
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("elixir");
        assert!(ElixirModule.install(&pm, &dep).is_err());
    }

    #[test]
    fn install_propagates_erlang_pm_error_when_not_installed() {
        let pm = MockPackageManager {
            installed: false,
            install_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("elixir");
        assert!(ElixirModule.install(&pm, &dep).is_err());
    }

    #[test]
    fn install_does_not_reinstall_erlang_when_already_installed() {
        // installed=true means is_package_installed always returns true (erlang already present).
        // With the correct `!erlang.is_installed()` guard, erlang install is skipped.
        // Only elixir's install_package call happens → install_count = 1.
        // With mutant (delete !): erlang IS installed → condition true → install erlang too →
        // install_count = 2.
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        ElixirModule
            .install(&pm, &Dependency::simple("elixir"))
            .unwrap();
        assert_eq!(
            pm.installed_packages.borrow().len(),
            1,
            "Only elixir should be installed when erlang is already present; got: {:?}",
            pm.installed_packages.borrow()
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
    fn elixir_post_setup_no_mix_exs_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default();
        assert!(
            ElixirModule
                .post_setup(&Dependency::simple("elixir"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn elixir_post_setup_skips_mix_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let mix_exs = dir.join("mix.exs");
        std::fs::write(&mix_exs, "").unwrap();
        std::fs::write(
            dir.join(".devy_elixir_stamp"),
            file_mtime_secs(&mix_exs).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            ElixirModule
                .post_setup(&Dependency::simple("elixir"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn elixir_post_setup_prefers_mix_lock_as_stamp_target() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("mix.exs"), "").unwrap();
        let lock = dir.join("mix.lock");
        std::fs::write(&lock, "").unwrap();
        std::fs::write(
            dir.join(".devy_elixir_stamp"),
            file_mtime_secs(&lock).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            ElixirModule
                .post_setup(&Dependency::simple("elixir"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn install_installs_erlang_then_elixir_when_neither_present() {
        let pm = MockPackageManager {
            installed: false,
            ..Default::default()
        };
        ElixirModule
            .install(&pm, &Dependency::simple("elixir"))
            .unwrap();
        let pkgs = pm.installed_packages.borrow();
        assert_eq!(
            pkgs.len(),
            2,
            "Expected erlang + elixir to be installed, got: {pkgs:?}"
        );
    }
}
