use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use which::which;

use crate::config::Dependency;
use crate::installers;
use crate::output;
use crate::package_manager::PackageManager;

use super::Module;
use super::helpers::{stamp_matches, write_stamp};

pub struct DenoModule;

/// Arguments for the deno installer: `-s v<version>` when a version is pinned (a version
/// already starting with `v` is passed as is). The installer ignores arguments starting
/// with `-` and takes the first other one as the version.
fn installer_args(version: Option<&str>) -> Vec<OsString> {
    match version {
        Some(v) if v.starts_with('v') => vec!["-s".into(), v.into()],
        Some(v) => vec!["-s".into(), format!("v{v}").into()],
        None => Vec::new(),
    }
}

fn deno_bin() -> Option<String> {
    // The same home the installer runs with (see `installers::user_home`).
    installers::user_home().map(|home| deno_bin_in(&home))
}

/// Where the deno installer puts the binary under `home`.
fn deno_bin_in(home: &std::ffi::OsStr) -> String {
    format!("{}/.deno/bin/deno", home.to_string_lossy())
}

/// Whether deno is on PATH or present at `bin` (the installer's location).
fn installed(bin: Option<&str>) -> bool {
    which("deno").is_ok() || bin.is_some_and(|b| Path::new(b).exists())
}

impl Module for DenoModule {
    fn source(&self) -> Option<&'static str> {
        Some("deno-installer")
    }

    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("deno".to_string())
    }

    fn is_installed(&self, _pm: &dyn PackageManager, _dep: &Dependency) -> Result<bool> {
        Ok(installed(deno_bin().as_deref()))
    }

    fn install(&self, _pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        let status = installers::run_script(
            &installers::DENO,
            installers::Interpreter::Sh,
            &installer_args(dep.version.as_deref()),
            // CI=1 skips the script's interactive shell setup, which would run an
            // unpinned JSR module and offer to edit shell rc files.
            &[("CI", "1")],
        )?;
        if !status.success() {
            bail!("Deno installation failed — check the output above for details");
        }
        Ok(())
    }

    fn setup_steps(&self, _dep: &Dependency, project_root: &Path) -> Vec<String> {
        ["deno.json", "deno.jsonc"]
            .into_iter()
            .find(|f| project_root.join(f).exists())
            .map(|f| vec![format!("deno install ({f})")])
            .unwrap_or_default()
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let deno_json = project_root.join("deno.json");
        let deno_jsonc = project_root.join("deno.jsonc");
        let manifest = if deno_json.exists() {
            Some(deno_json)
        } else if deno_jsonc.exists() {
            Some(deno_jsonc)
        } else {
            None
        };
        let Some(manifest) = manifest else {
            return Ok(());
        };
        let stamp_path = project_root.join(".devy_deno_stamp");
        if stamp_matches(&stamp_path, &manifest) {
            output::skip("Deno dependencies up to date");
            return Ok(());
        }
        let deno = deno_bin()
            .filter(|b| std::path::Path::new(b).exists())
            .unwrap_or_else(|| "deno".into());
        output::step("Running deno install");
        let status = Command::new(&deno)
            .arg("install")
            .current_dir(project_root)
            .status()
            .context("Failed to run `deno install`")?;
        if !status.success() {
            anyhow::bail!("`deno install` failed — check the output above for details");
        }
        write_stamp(&stamp_path, &manifest)?;
        output::success("Deno dependencies installed");
        Ok(())
    }

    fn resolved_version(
        &self,
        _pm: &dyn PackageManager,
        _dep: &Dependency,
    ) -> Result<Option<String>> {
        // The installer's binary, else one on PATH outside the project — never a bare
        // name resolved against the full PATH.
        let Some(path) = deno_bin()
            .filter(|b| std::path::Path::new(b).exists())
            .map(std::path::PathBuf::from)
            .or_else(|| crate::fs_safe::which_outside_project("deno"))
        else {
            return Ok(None);
        };
        let out = Command::new(path).arg("--version").output();
        // "deno 1.40.0 ..." — take second token of first line
        Ok(out.ok().and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()?
                .lines()
                .next()?
                .split_whitespace()
                .nth(1)
                .map(String::from)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn deno_module_is_not_a_service() {
        assert!(!DenoModule.is_service());
    }

    #[test]
    fn deno_source_is_deno_installer() {
        assert_eq!(DenoModule.source(), Some("deno-installer"));
    }

    #[test]
    fn deno_bin_returns_some_with_non_empty_home() {
        if installers::user_home().is_some() {
            let bin = deno_bin().unwrap();
            assert!(
                bin.contains(".deno/bin/deno"),
                "Expected ~/.deno/bin/deno, got {bin}"
            );
        }
    }

    #[test]
    fn deno_bin_path_contains_home() {
        if let Some(home) = installers::user_home() {
            let bin = deno_bin().unwrap();
            assert!(bin.starts_with(&*home.to_string_lossy()));
        }
    }

    #[test]
    fn deno_is_installed_does_not_panic() {
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("deno");
        let _ = DenoModule.is_installed(&pm, &dep);
    }

    #[test]
    fn deno_is_installed_true_when_bin_file_exists_and_not_on_path() {
        // Only meaningful when deno is NOT on PATH. Uses a temporary home rather than
        // the real one, so it never touches the user's ~/.deno and never races the
        // tests that expect no binary there.
        if which("deno").is_ok() {
            return;
        }
        let home = crate::test_support::tmp_dir();
        let bin_path = deno_bin_in(home.as_os_str());
        assert!(!installed(Some(&bin_path)));
        let path = Path::new(&bin_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh").unwrap();
        assert!(
            installed(Some(&bin_path)),
            "Expected installed=true when deno binary exists at {bin_path}"
        );
    }

    #[test]
    fn deno_resolved_version_returns_ok() {
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("deno");
        assert!(DenoModule.resolved_version(&pm, &dep).is_ok());
    }

    #[test]
    fn deno_resolved_version_is_non_empty_when_installed() {
        if which("deno").is_err() {
            return;
        }
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("deno");
        let ver = DenoModule.resolved_version(&pm, &dep).unwrap();
        assert!(ver.is_some(), "Expected Some version when deno is on PATH");
        assert!(
            !ver.unwrap().is_empty(),
            "Expected non-empty version string"
        );
    }

    #[test]
    fn deno_is_installed_checks_the_installer_location_in_the_user_home() {
        let home = crate::test_support::tmp_dir();
        let bin = deno_bin_in(home.as_os_str());
        assert!(bin.ends_with("/.deno/bin/deno"), "{bin}");
        // Not there yet: only a deno on PATH can make it count as installed.
        assert_eq!(installed(Some(&bin)), which("deno").is_ok());
        std::fs::create_dir_all(home.join(".deno/bin")).unwrap();
        std::fs::write(&bin, b"").unwrap();
        assert!(
            installed(Some(&bin)),
            "the installer's binary must count as installed"
        );
    }

    #[test]
    fn deno_is_installed_false_when_not_on_path_and_no_bin_file() {
        if which("deno").is_ok() {
            return;
        }
        if let Some(p) = deno_bin()
            && std::path::Path::new(&p).exists()
        {
            return;
        }
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("deno");
        assert!(
            !DenoModule.is_installed(&pm, &dep).unwrap(),
            "is_installed must return false when deno is absent from PATH and no bin file"
        );
    }

    #[test]
    fn deno_resolved_version_is_none_when_not_installed() {
        if which("deno").is_ok() {
            return;
        }
        if let Some(p) = deno_bin()
            && std::path::Path::new(&p).exists()
        {
            return;
        }
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("deno");
        let ver = DenoModule.resolved_version(&pm, &dep).unwrap();
        assert!(
            ver.is_none(),
            "Expected None version when deno is not installed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn deno_install_fails_when_script_exits_nonzero() {
        use crate::installers::test_hooks;
        test_hooks::clear();
        test_hooks::serve(&installers::DENO, b"exit 1\n");
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("deno");
        let result = DenoModule.install(&pm, &dep);
        test_hooks::clear();
        assert!(
            result.is_err(),
            "install must return Err when script exits non-zero"
        );
    }

    #[cfg(unix)]
    #[test]
    fn deno_install_runs_verified_installer_with_pinned_version() {
        use crate::installers::{Interpreter, test_hooks};
        test_hooks::clear();
        test_hooks::serve(&installers::DENO, b"exit 0\n");
        let pm = MockPackageManager::default();
        let dep = Dependency {
            version: Some("1.40.0".into()),
            ..Dependency::simple("deno")
        };
        let result = DenoModule.install(&pm, &dep);
        let runs = test_hooks::runs();
        let envs = test_hooks::envs();
        test_hooks::clear();
        result.unwrap();
        assert_eq!(
            runs,
            vec![(
                installers::DENO.name,
                Interpreter::Sh,
                vec!["-s".into(), "v1.40.0".into()]
            )]
        );
        assert_eq!(envs, vec![("CI".to_string(), "1".to_string())]);
    }

    #[test]
    fn deno_install_without_verified_download_runs_nothing() {
        use crate::installers::test_hooks;
        test_hooks::clear();
        let pm = MockPackageManager::default();
        assert!(
            DenoModule
                .install(&pm, &Dependency::simple("deno"))
                .is_err()
        );
        assert!(test_hooks::runs().is_empty());
    }

    #[test]
    fn deno_installer_args() {
        assert!(installer_args(None).is_empty());
        assert_eq!(installer_args(Some("1.40.0")), vec!["-s", "v1.40.0"]);
        assert_eq!(installer_args(Some("v2.0.0")), vec!["-s", "v2.0.0"]);
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
    fn deno_post_setup_no_config_file_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default();
        assert!(
            DenoModule
                .post_setup(&Dependency::simple("deno"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn deno_post_setup_skips_install_when_stamp_matches_deno_json() {
        let dir = crate::test_support::tmp_dir();
        let cfg = dir.join("deno.json");
        std::fs::write(&cfg, r#"{"imports":{}}"#).unwrap();
        std::fs::write(
            dir.join(".devy_deno_stamp"),
            file_mtime_secs(&cfg).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            DenoModule
                .post_setup(&Dependency::simple("deno"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn deno_post_setup_skips_install_when_stamp_matches_deno_jsonc() {
        let dir = crate::test_support::tmp_dir();
        let cfg = dir.join("deno.jsonc");
        std::fs::write(&cfg, "{}").unwrap();
        std::fs::write(
            dir.join(".devy_deno_stamp"),
            file_mtime_secs(&cfg).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            DenoModule
                .post_setup(&Dependency::simple("deno"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn deno_post_setup_prefers_deno_json_over_deno_jsonc() {
        let dir = crate::test_support::tmp_dir();
        let json = dir.join("deno.json");
        std::fs::write(&json, "{}").unwrap();
        std::fs::write(dir.join("deno.jsonc"), "{}").unwrap();
        std::fs::write(
            dir.join(".devy_deno_stamp"),
            file_mtime_secs(&json).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            DenoModule
                .post_setup(&Dependency::simple("deno"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn deno_resolved_version_contains_dot_when_installed() {
        if which("deno").is_err() {
            if let Some(p) = deno_bin() {
                if !std::path::Path::new(&p).exists() {
                    return;
                }
            } else {
                return;
            }
        }
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("deno");
        let ver = DenoModule.resolved_version(&pm, &dep).unwrap();
        if let Some(v) = ver {
            assert!(v.contains('.'), "Expected semver-like version, got: {v}");
            assert_ne!(v, "xyzzy", "Version must not be the placeholder 'xyzzy'");
        }
    }
}
