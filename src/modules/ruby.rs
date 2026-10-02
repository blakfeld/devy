use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use which::which;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep, run_cmd};

// Last updated: 2026-05 — bump when a new Ruby stable is released.
// Check: https://www.ruby-lang.org/en/downloads/
const DEFAULT_RUBY_VERSION: &str = "3.3.6";

pub struct RubyModule;

fn rbenv_root() -> Option<String> {
    if let Ok(root) = std::env::var("RBENV_ROOT")
        && !root.is_empty()
    {
        return Some(root);
    }
    std::env::var("HOME").ok().map(|h| format!("{h}/.rbenv"))
}

fn rbenv_version_installed(version: &str) -> Result<bool> {
    use std::process::Stdio;
    let status = Command::new("rbenv")
        .args(["prefix", version])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to run `rbenv prefix`")?;
    Ok(status.success())
}

pub(crate) fn rbenv_has_any_version(stdout: &str) -> bool {
    !stdout.trim().is_empty()
}

fn rbenv_local() -> Option<String> {
    let out = Command::new("rbenv").arg("local").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn winget_package_id(dep: &Dependency) -> String {
    let major = dep
        .version
        .as_deref()
        .and_then(|v| v.split('.').next())
        .unwrap_or("3");
    format!("RubyInstallerTeam.Ruby.{major}")
}

impl Module for RubyModule {
    fn source(&self) -> Option<&'static str> {
        Some("rbenv")
    }

    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("ruby".to_string())
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        if pm.name() == "winget" {
            return pm.is_package_installed(&pm_dep(dep, &winget_package_id(dep)));
        }
        if which("rbenv").is_err() {
            return Ok(false);
        }
        match &dep.version {
            Some(v) => rbenv_version_installed(v),
            None => {
                let out = Command::new("rbenv")
                    .args(["versions", "--bare"])
                    .output()
                    .context("Failed to run `rbenv versions --bare`")?;
                Ok(out.status.success()
                    && rbenv_has_any_version(&String::from_utf8_lossy(&out.stdout)))
            }
        }
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        if pm.name() == "winget" {
            return pm.install_package(&pm_dep(dep, &winget_package_id(dep)));
        }

        if which("rbenv").is_err() {
            match pm.name() {
                "apt" => {
                    pm.install_package(&Dependency::simple("rbenv"))?;
                    pm.install_package(&Dependency::simple("ruby-build"))?;
                }
                _ => {
                    pm.install_package(&Dependency::simple("rbenv"))?;
                }
            }
        }

        let version = dep.version.as_deref().unwrap_or(DEFAULT_RUBY_VERSION);
        run_cmd("rbenv", &["install", "--skip-existing", version])?;
        Ok(())
    }

    fn env_vars(
        &self,
        _dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        if let Some(root) = rbenv_root() {
            vars.insert("RBENV_ROOT".into(), root);
        }
        vars
    }

    fn path_prepends(&self, _dep: &Dependency, _project_root: &std::path::Path) -> Vec<String> {
        rbenv_root()
            .map(|root| vec![format!("{root}/bin"), format!("{root}/shims")])
            .unwrap_or_default()
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        if which("rbenv").is_ok() {
            let version_to_set = match dep.version.as_deref() {
                Some(v) => Some(v),
                None => {
                    if project_root.join(".ruby-version").exists() {
                        None
                    } else {
                        Some(DEFAULT_RUBY_VERSION)
                    }
                }
            };
            if let Some(version) = version_to_set {
                let status = Command::new("rbenv")
                    .args(["local", version])
                    .current_dir(project_root)
                    .status()
                    .with_context(|| format!("Failed to run `rbenv local {version}`"))?;
                if !status.success() {
                    anyhow::bail!(
                        "`rbenv local {version}` failed — run `rbenv install {version}` first"
                    );
                }
            }
        }

        if !project_root.join("Gemfile").exists() {
            return Ok(());
        }

        // Prefer Gemfile.lock as the stamp target (more stable than Gemfile itself).
        let manifest = {
            let lock = project_root.join("Gemfile.lock");
            if lock.exists() {
                lock
            } else {
                project_root.join("Gemfile")
            }
        };
        let stamp_path = project_root.join(".bundle").join(".devy_stamp");
        if stamp_matches(&stamp_path, &manifest) {
            output::skip("bundle dependencies up to date");
            return Ok(());
        }

        // Use the shims path directly to avoid PATH ordering issues during `devy up`:
        // shadowenv hasn't been activated yet so rbenv shims may not be on $PATH.
        let bundle = rbenv_root()
            .map(|root| format!("{root}/shims/bundle"))
            .filter(|p| Path::new(p).exists())
            .unwrap_or_else(|| "bundle".into());

        output::step("Running bundle install");
        let status = Command::new(&bundle)
            .arg("install")
            .current_dir(project_root)
            .status()
            .context("Failed to run `bundle install`")?;
        if !status.success() {
            anyhow::bail!("`bundle install` failed — check the output above for details");
        }
        std::fs::create_dir_all(
            stamp_path
                .parent()
                .expect(".bundle/.devy_stamp always has a parent"),
        )
        .context("Failed to create .bundle directory for stamp")?;
        write_stamp(&stamp_path, &manifest);
        output::success("bundle install complete");
        Ok(())
    }

    fn resolved_version(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
    ) -> Result<Option<String>> {
        if pm.name() == "winget" {
            return pm.resolved_version(dep);
        }
        // Prefer the pinned version from devy.yml; fall back to whatever rbenv global reports.
        if dep.version.is_some() {
            return Ok(dep.version.clone());
        }
        Ok(rbenv_local())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    // ── rbenv_has_any_version ─────────────────────────────────────────────────

    #[test]
    fn rbenv_has_any_version_returns_false_for_empty_output() {
        assert!(!rbenv_has_any_version(""));
    }

    #[test]
    fn rbenv_has_any_version_returns_false_for_whitespace_only() {
        assert!(!rbenv_has_any_version("   \n  "));
    }

    #[test]
    fn rbenv_has_any_version_returns_true_when_versions_listed() {
        assert!(rbenv_has_any_version("3.3.6\n3.2.0\n"));
        assert!(rbenv_has_any_version("3.3.6"));
    }

    // ── default_ruby_version ──────────────────────────────────────────────────

    #[test]
    fn default_ruby_version_is_semver() {
        let parts: Vec<&str> = DEFAULT_RUBY_VERSION.split('.').collect();
        assert_eq!(
            parts.len(),
            3,
            "DEFAULT_RUBY_VERSION must have three components"
        );
        for part in parts {
            part.parse::<u32>()
                .expect("each component of DEFAULT_RUBY_VERSION must be a valid u32");
        }
    }

    #[test]
    fn ruby_module_is_not_a_service() {
        assert!(!RubyModule.is_service());
    }

    #[test]
    fn ruby_source_is_rbenv() {
        assert_eq!(RubyModule.source(), Some("rbenv"));
    }

    #[test]
    fn ruby_winget_is_installed_delegates_to_pm() {
        let pm = MockPackageManager {
            name: "winget",
            installed: true,
            ..Default::default()
        };
        assert!(
            RubyModule
                .is_installed(&pm, &Dependency::simple("ruby"))
                .unwrap()
        );
    }

    #[test]
    fn ruby_winget_not_installed_when_pm_reports_false() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert!(
            !RubyModule
                .is_installed(&pm, &Dependency::simple("ruby"))
                .unwrap()
        );
    }

    #[test]
    fn ruby_winget_install_delegates_to_pm() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert!(RubyModule.install(&pm, &Dependency::simple("ruby")).is_ok());
        assert!(!pm.installed_packages.borrow().is_empty());
    }

    #[test]
    fn ruby_winget_install_propagates_pm_error() {
        let pm = MockPackageManager {
            name: "winget",
            install_fails: true,
            ..Default::default()
        };
        assert!(
            RubyModule
                .install(&pm, &Dependency::simple("ruby"))
                .is_err()
        );
    }

    #[test]
    fn ruby_env_vars_contains_rbenv_root_when_home_set() {
        if std::env::var("HOME").is_err() {
            return;
        }
        let dep = Dependency::simple("ruby");
        let vars = RubyModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert!(vars.contains_key("RBENV_ROOT"), "RBENV_ROOT must be set");
        assert!(!vars["RBENV_ROOT"].is_empty());
    }

    #[test]
    fn ruby_path_prepends_contains_bin_and_shims() {
        if std::env::var("HOME").is_err() {
            return;
        }
        let dep = Dependency::simple("ruby");
        let prepends = RubyModule.path_prepends(&dep, std::path::Path::new("/tmp"));
        assert_eq!(prepends.len(), 2);
        assert!(prepends[0].ends_with("/bin"));
        assert!(prepends[1].ends_with("/shims"));
    }

    #[test]
    fn ruby_post_setup_preserves_existing_ruby_version_when_no_dep_version() {
        // When dep.version is None and .ruby-version already exists, post_setup must
        // NOT overwrite it — even if DEFAULT_RUBY_VERSION differs from what's in the file.
        if which("rbenv").is_err() {
            return;
        }
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join(".ruby-version"), "3.2.0\n").unwrap();
        let dep = Dependency::simple("ruby"); // no version
        let pm = MockPackageManager::default();
        let _ = RubyModule.post_setup(&dep, &pm, &dir); // result may vary; file must be unchanged
        let content = std::fs::read_to_string(dir.join(".ruby-version")).unwrap();
        assert_eq!(
            content.trim(),
            "3.2.0",
            ".ruby-version must not be overwritten"
        );
    }

    #[test]
    fn ruby_post_setup_no_gemfile_skips_bundle_when_rbenv_absent() {
        // When rbenv is not on PATH, post_setup skips both rbenv local and bundle install.
        if which("rbenv").is_ok() {
            return;
        }
        let dir = crate::test_support::tmp_dir();
        let dep = Dependency::simple("ruby");
        let pm = MockPackageManager::default();
        assert!(RubyModule.post_setup(&dep, &pm, &dir).is_ok());
    }

    #[test]
    fn ruby_post_setup_writes_ruby_version_file() {
        // post_setup must write .ruby-version when a specific version is requested and rbenv
        // is available. Skipped when rbenv is absent (covered by the no-rbenv test above).
        if which("rbenv").is_err() {
            return;
        }
        let dir = crate::test_support::tmp_dir();
        let dep = Dependency {
            name: "ruby".into(),
            version: Some(DEFAULT_RUBY_VERSION.into()),
            tap: None,
            after_install: None,
            shell: None,
            extra: std::collections::HashMap::new(),
            version_from_lock: false,
        };
        let pm = MockPackageManager::default();
        // Only assert if the version is actually installed — this is an environment check.
        if rbenv_version_installed(DEFAULT_RUBY_VERSION).unwrap_or(false) {
            RubyModule.post_setup(&dep, &pm, &dir).unwrap();
            let content = std::fs::read_to_string(dir.join(".ruby-version"))
                .expect(".ruby-version must be written by post_setup");
            assert_eq!(content.trim(), DEFAULT_RUBY_VERSION);
        }
    }

    #[test]
    fn winget_package_id_defaults_to_ruby3() {
        let dep = Dependency::simple("ruby");
        assert_eq!(winget_package_id(&dep), "RubyInstallerTeam.Ruby.3");
    }

    #[test]
    fn winget_package_id_derives_major_from_version() {
        let dep = Dependency {
            name: "ruby".into(),
            version: Some("4.0.0".into()),
            tap: None,
            after_install: None,
            shell: None,
            extra: std::collections::HashMap::new(),
            version_from_lock: false,
        };
        assert_eq!(winget_package_id(&dep), "RubyInstallerTeam.Ruby.4");
    }

    #[test]
    fn ruby_resolved_version_winget_delegates_to_pm() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        let dep = Dependency::simple("ruby");
        assert!(RubyModule.resolved_version(&pm, &dep).is_ok());
    }

    #[test]
    fn ruby_resolved_version_returns_pinned_version_when_set() {
        // When dep.version is Some, resolved_version must return that exact version
        // rather than querying rbenv global, so the lock file records the correct version.
        let pm = MockPackageManager::default();
        let dep = Dependency {
            name: "ruby".into(),
            version: Some("3.2.0".into()),
            tap: None,
            after_install: None,
            shell: None,
            extra: std::collections::HashMap::new(),
            version_from_lock: false,
        };
        let ver = RubyModule.resolved_version(&pm, &dep).unwrap();
        assert_eq!(
            ver,
            Some("3.2.0".into()),
            "must return pinned dep.version, not rbenv local"
        );
    }
}
