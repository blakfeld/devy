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

pub struct BunModule;

/// Arguments for the bun installer: its positional release tag `bun-v<version>` when a
/// version is pinned (a leading `v` or `bun-v` is not repeated). `latest` passes nothing
/// (the installer's default) and `canary` passes the `canary` tag.
fn installer_args(version: Option<&str>) -> Vec<OsString> {
    match version {
        None | Some("latest") => Vec::new(),
        Some("canary") => vec!["canary".into()],
        Some(v) => {
            let bare = v
                .strip_prefix("bun-v")
                .or_else(|| v.strip_prefix('v'))
                .unwrap_or(v);
            vec![format!("bun-v{bare}").into()]
        }
    }
}

fn bun_bin() -> Option<String> {
    // The same home the installer runs with (see `installers::user_home`).
    installers::user_home().map(|home| bun_bin_in(&home))
}

/// Where the bun installer puts the binary under `home`.
fn bun_bin_in(home: &std::ffi::OsStr) -> String {
    format!("{}/.bun/bin/bun", home.to_string_lossy())
}

/// Whether bun is on PATH or present at `bin` (the installer's location).
fn installed(bin: Option<&str>) -> bool {
    which("bun").is_ok() || bin.is_some_and(|b| Path::new(b).exists())
}

impl Module for BunModule {
    fn source(&self) -> Option<&'static str> {
        Some("bun-installer")
    }

    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("bun".to_string())
    }

    fn is_installed(&self, _pm: &dyn PackageManager, _dep: &Dependency) -> Result<bool> {
        Ok(installed(bun_bin().as_deref()))
    }

    fn install(&self, _pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        let status = installers::run_script(
            &installers::BUN,
            installers::Interpreter::Bash,
            &installer_args(dep.version.as_deref()),
            // The installer edits the rc file of the shell `$SHELL` names; bash fills an
            // unset SHELL from the user database, so name a shell it doesn't recognise and
            // it only prints PATH instructions.
            &[("SHELL", "/bin/sh")],
        )?;
        if !status.success() {
            bail!("Bun installation failed — check the output above for details");
        }
        Ok(())
    }

    fn setup_steps(&self, _dep: &Dependency, project_root: &Path) -> Vec<String> {
        super::helpers::step_if_exists(
            project_root,
            "package.json",
            "bun install (package.json lifecycle scripts)",
        )
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
        let lockfile = project_root.join("bun.lockb");
        let package_json = project_root.join("package.json");
        let manifest: &Path = if lockfile.exists() {
            &lockfile
        } else {
            &package_json
        };
        let stamp_path = project_root.join(".devy_bun_stamp");
        if stamp_matches(&stamp_path, manifest) {
            output::skip("Bun dependencies up to date");
            return Ok(());
        }
        let bun = bun_bin()
            .filter(|b| std::path::Path::new(b).exists())
            .unwrap_or_else(|| "bun".into());
        output::step("Running bun install");
        let status = Command::new(&bun)
            .arg("install")
            .current_dir(project_root)
            .status()
            .context("Failed to run `bun install`")?;
        if !status.success() {
            anyhow::bail!("`bun install` failed — check the output above for details");
        }
        write_stamp(&stamp_path, manifest)?;
        output::success("Bun dependencies installed");
        Ok(())
    }

    fn resolved_version(
        &self,
        _pm: &dyn PackageManager,
        _dep: &Dependency,
    ) -> Result<Option<String>> {
        // The installer's binary, else one on PATH outside the project — never a bare
        // name resolved against the full PATH.
        let Some(path) = bun_bin()
            .filter(|b| std::path::Path::new(b).exists())
            .map(std::path::PathBuf::from)
            .or_else(|| crate::fs_safe::which_outside_project("bun"))
        else {
            return Ok(None);
        };
        let out = Command::new(path).arg("--version").output();
        // "1.0.25" — output is just the version string
        Ok(out.ok().and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn bun_module_is_not_a_service() {
        assert!(!BunModule.is_service());
    }

    #[test]
    fn bun_source_is_bun_installer() {
        assert_eq!(BunModule.source(), Some("bun-installer"));
    }

    #[test]
    fn bun_bin_returns_some_with_non_empty_home() {
        // On any normal system HOME is set and non-empty, so bun_bin() should return Some.
        if installers::user_home().is_some() {
            let bin = bun_bin().unwrap();
            assert!(
                bin.contains(".bun/bin/bun"),
                "Expected ~/.bun/bin/bun, got {bin}"
            );
        }
    }

    #[test]
    fn bun_bin_path_contains_home() {
        if let Some(home) = installers::user_home() {
            let bin = bun_bin().unwrap();
            assert!(bin.starts_with(&*home.to_string_lossy()));
        }
    }

    #[test]
    fn bun_is_installed_does_not_panic() {
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("bun");
        let _ = BunModule.is_installed(&pm, &dep);
    }

    #[test]
    fn bun_is_installed_true_when_bin_file_exists_and_not_on_path() {
        // Only meaningful when bun is NOT on PATH. Uses a temporary home rather than
        // the real one, so it never touches the user's ~/.bun and never races the
        // tests that expect no binary there.
        if which("bun").is_ok() {
            return;
        }
        let home = crate::test_support::tmp_dir();
        let bin_path = bun_bin_in(home.as_os_str());
        assert!(!installed(Some(&bin_path)));
        let path = Path::new(&bin_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh").unwrap();
        assert!(
            installed(Some(&bin_path)),
            "Expected installed=true when bun binary exists at {bin_path}"
        );
    }

    #[test]
    fn bun_resolved_version_returns_ok() {
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("bun");
        // Must not error regardless of whether bun is installed.
        assert!(BunModule.resolved_version(&pm, &dep).is_ok());
    }

    #[test]
    fn bun_resolved_version_is_non_empty_when_installed() {
        if which("bun").is_err() {
            return;
        }
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("bun");
        let ver = BunModule.resolved_version(&pm, &dep).unwrap();
        assert!(ver.is_some(), "Expected Some version when bun is on PATH");
        assert!(
            !ver.unwrap().is_empty(),
            "Expected non-empty version string"
        );
    }

    #[test]
    fn bun_is_installed_checks_the_installer_location_in_the_user_home() {
        let home = crate::test_support::tmp_dir();
        let bin = bun_bin_in(home.as_os_str());
        assert!(bin.ends_with("/.bun/bin/bun"), "{bin}");
        // Not there yet: only a bun on PATH can make it count as installed.
        assert_eq!(installed(Some(&bin)), which("bun").is_ok());
        std::fs::create_dir_all(home.join(".bun/bin")).unwrap();
        std::fs::write(&bin, b"").unwrap();
        assert!(
            installed(Some(&bin)),
            "the installer's binary must count as installed"
        );
    }

    #[test]
    fn bun_is_installed_false_when_not_on_path_and_no_bin_file() {
        // Skip if bun is reachable via PATH — can't test the false case then.
        if which("bun").is_ok() {
            return;
        }
        // Also skip if the bun_bin() path already exists (e.g. ~/.bun/bin/bun).
        if let Some(p) = bun_bin()
            && std::path::Path::new(&p).exists()
        {
            return;
        }
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("bun");
        assert!(
            !BunModule.is_installed(&pm, &dep).unwrap(),
            "is_installed must return false when bun is absent from PATH and no bin file"
        );
    }

    #[test]
    fn bun_resolved_version_is_none_when_bun_not_installed() {
        if which("bun").is_ok() {
            return;
        }
        if let Some(p) = bun_bin()
            && std::path::Path::new(&p).exists()
        {
            return;
        }
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("bun");
        let ver = BunModule.resolved_version(&pm, &dep).unwrap();
        assert!(
            ver.is_none(),
            "Expected None version when bun is not installed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bun_install_fails_when_script_exits_nonzero() {
        use crate::installers::test_hooks;
        test_hooks::clear();
        test_hooks::serve(&installers::BUN, b"exit 1\n");
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("bun");
        let result = BunModule.install(&pm, &dep);
        test_hooks::clear();
        assert!(
            result.is_err(),
            "install must return Err when script exits non-zero"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bun_install_runs_verified_installer_with_pinned_version() {
        use crate::installers::{Interpreter, test_hooks};
        test_hooks::clear();
        test_hooks::serve(&installers::BUN, b"exit 0\n");
        let pm = MockPackageManager::default();
        let dep = Dependency {
            version: Some("1.1.0".into()),
            ..Dependency::simple("bun")
        };
        let result = BunModule.install(&pm, &dep);
        let runs = test_hooks::runs();
        let envs = test_hooks::envs();
        test_hooks::clear();
        result.unwrap();
        assert_eq!(envs, vec![("SHELL".to_string(), "/bin/sh".to_string())]);
        assert_eq!(
            runs,
            vec![(
                installers::BUN.name,
                Interpreter::Bash,
                vec!["bun-v1.1.0".into()]
            )]
        );
    }

    #[test]
    fn bun_install_without_verified_download_runs_nothing() {
        use crate::installers::test_hooks;
        test_hooks::clear();
        let pm = MockPackageManager::default();
        assert!(BunModule.install(&pm, &Dependency::simple("bun")).is_err());
        assert!(test_hooks::runs().is_empty());
    }

    #[test]
    fn bun_installer_args() {
        assert!(installer_args(None).is_empty());
        assert_eq!(installer_args(Some("1.1.0")), vec!["bun-v1.1.0"]);
        assert_eq!(installer_args(Some("v1.1.0")), vec!["bun-v1.1.0"]);
        assert_eq!(installer_args(Some("bun-v1.1.0")), vec!["bun-v1.1.0"]);
        assert!(installer_args(Some("latest")).is_empty());
        assert_eq!(installer_args(Some("canary")), vec!["canary"]);
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
    fn bun_post_setup_no_package_json_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default();
        assert!(
            BunModule
                .post_setup(&Dependency::simple("bun"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn bun_post_setup_skips_install_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let pkg_json = dir.join("package.json");
        std::fs::write(&pkg_json, r#"{"name":"test"}"#).unwrap();
        std::fs::write(
            dir.join(".devy_bun_stamp"),
            file_mtime_secs(&pkg_json).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            BunModule
                .post_setup(&Dependency::simple("bun"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn bun_post_setup_prefers_bun_lockb_as_stamp_target() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("package.json"), r#"{"name":"test"}"#).unwrap();
        let lock = dir.join("bun.lockb");
        std::fs::write(&lock, "").unwrap();
        std::fs::write(
            dir.join(".devy_bun_stamp"),
            file_mtime_secs(&lock).to_string(),
        )
        .unwrap();
        let pm = MockPackageManager::default();
        assert!(
            BunModule
                .post_setup(&Dependency::simple("bun"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn bun_resolved_version_contains_dot_when_installed() {
        if which("bun").is_err() {
            if let Some(p) = bun_bin() {
                if !std::path::Path::new(&p).exists() {
                    return;
                }
            } else {
                return;
            }
        }
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("bun");
        let ver = BunModule.resolved_version(&pm, &dep).unwrap();
        if let Some(v) = ver {
            assert!(v.contains('.'), "Expected semver-like version, got: {v}");
            assert_ne!(v, "xyzzy", "Version must not be the placeholder 'xyzzy'");
        }
    }
}
