use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep};

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

/// Checks a Ruby version before it is passed to `rbenv install` or `rbenv local`.
///
/// Neither command accepts `--` as an end-of-options marker: `rbenv install` treats
/// everything after `--` as configure arguments, and `rbenv local` would write `--` itself
/// to `.ruby-version`. So instead of a separator, devy accepts only plain version names:
/// an ASCII letter or digit followed by letters, digits, `.`, `_` or `-`. That rejects
/// option-like values and also any path — ruby-build sources a definition argument that
/// names an existing file, so `./x` or `../x` would run a repo-controlled script.
/// (Config validation also checks versions; this is defense in depth.)
fn rbenv_version_arg(version: &str) -> Result<&str> {
    if !crate::validate::toolchain(version) {
        anyhow::bail!("invalid Ruby version '{version}'");
    }
    Ok(version)
}

/// Builds an `rbenv` command for `install`/`prefix` that runs in the filesystem root, so a
/// bare version name can never resolve to a file in the repository or in a world-writable
/// directory such as `/tmp` (ruby-build looks for a definition file relative to the working
/// directory first). Files in `/` are writable only by root.
fn rbenv_outside_project(rbenv: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(rbenv);
    cmd.args(args).current_dir(RBENV_CWD);
    cmd
}

/// `rbenv` on PATH outside the project, or in the verified project nix profile (where the
/// nix backend installs it), so a repository can't plant one that `devy status` or
/// `devy check` would run.
fn rbenv_program() -> Option<PathBuf> {
    crate::fs_safe::which_with_project_profile("rbenv")
}

fn require_rbenv() -> Result<PathBuf> {
    rbenv_program()
        .context("rbenv was not found on PATH outside the project or in the project's nix profile")
}

const RBENV_CWD: &str = "/";

fn rbenv_version_installed(rbenv: &Path, version: &str) -> Result<bool> {
    use std::process::Stdio;
    let status = rbenv_outside_project(rbenv, &["prefix", rbenv_version_arg(version)?])
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
    // `rbenv local` reads `.ruby-version` in the working directory, so this one runs in the
    // project; the program itself still comes from outside it.
    let out = Command::new(rbenv_program()?).arg("local").output().ok()?;
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

    /// winget installs RubyInstaller; elsewhere rbenv builds Ruby, after the backend
    /// installs rbenv (and ruby-build, with `sudo apt-get`) when it is missing.
    fn install_route(&self, backend: &str) -> String {
        match backend {
            "winget" => backend.to_string(),
            _ => format!("rbenv via {backend}"),
        }
    }

    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("ruby".to_string())
    }

    fn backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency> {
        (pm.name() == "winget").then(|| pm_dep(dep, &winget_package_id(dep)))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        if pm.name() == "winget" {
            return super::backend_installed(self, pm, dep);
        }
        let Some(rbenv) = rbenv_program() else {
            return Ok(false);
        };
        match &dep.version {
            Some(v) => rbenv_version_installed(&rbenv, v),
            None => {
                let out = rbenv_outside_project(&rbenv, &["versions", "--bare"])
                    .output()
                    .context("Failed to run `rbenv versions --bare`")?;
                Ok(out.status.success()
                    && rbenv_has_any_version(&String::from_utf8_lossy(&out.stdout)))
            }
        }
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        if pm.name() == "winget" {
            return super::install_backend(self, pm, dep);
        }

        if rbenv_program().is_none() {
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
        let args = ["install", "--skip-existing", rbenv_version_arg(version)?];
        let status = rbenv_outside_project(&require_rbenv()?, &args)
            .status()
            .context("Failed to start `rbenv`")?;
        if !status.success() {
            anyhow::bail!(
                "`rbenv {}` failed — check the output above for details",
                args.join(" ")
            );
        }
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

    fn setup_steps(&self, _dep: &Dependency, project_root: &Path) -> Vec<String> {
        super::helpers::step_if_exists(
            project_root,
            "Gemfile",
            "bundle install (Gemfile, gem native extensions)",
        )
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        if let Some(rbenv) = rbenv_program() {
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
                let status = Command::new(&rbenv)
                    .args(["local", rbenv_version_arg(version)?])
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
        crate::fs_safe::ensure_dir_in(
            project_root,
            stamp_path
                .parent()
                .expect(".bundle/.devy_stamp always has a parent"),
        )
        .context("Failed to create .bundle directory for stamp")?;
        write_stamp(&stamp_path, &manifest)?;
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
    use which::which;

    #[test]
    fn rbenv_version_arg_accepts_versions() {
        assert_eq!(rbenv_version_arg("3.3.6").unwrap(), "3.3.6");
        assert_eq!(rbenv_version_arg("system").unwrap(), "system");
    }

    #[test]
    fn rbenv_version_arg_accepts_named_versions() {
        assert!(rbenv_version_arg("jruby-9.4.5.0").is_ok());
        assert!(rbenv_version_arg("3.4.0-preview1").is_ok());
        assert!(rbenv_version_arg("truffleruby_24").is_ok());
    }

    #[test]
    fn rbenv_version_arg_rejects_option_like_values() {
        assert!(rbenv_version_arg("--unset").is_err());
        assert!(rbenv_version_arg("-f").is_err());
        assert!(rbenv_version_arg("").is_err());
    }

    #[test]
    fn rbenv_version_arg_rejects_paths() {
        for v in [
            "./x",
            "/tmp/x",
            "../x",
            "3.3.6/../x",
            ".hidden",
            "a\\b",
            "3.3 6",
        ] {
            assert!(rbenv_version_arg(v).is_err(), "{v} must be rejected");
        }
    }

    #[test]
    fn rbenv_install_runs_outside_the_project() {
        let cmd = rbenv_outside_project(
            Path::new("/usr/local/bin/rbenv"),
            &["install", "--skip-existing", "3.3.6"],
        );
        assert_eq!(cmd.get_program(), "/usr/local/bin/rbenv");
        let cwd = cmd.get_current_dir().expect("cwd must be set");
        assert_eq!(cwd, std::path::Path::new("/"));
        assert_ne!(cwd, std::env::temp_dir());
        assert_ne!(cwd, std::env::current_dir().unwrap());
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["install", "--skip-existing", "3.3.6"]);
    }

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
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
        };
        let pm = MockPackageManager::default();
        // Only assert if the version is actually installed — this is an environment check.
        if rbenv_program().is_some_and(|rbenv| {
            rbenv_version_installed(&rbenv, DEFAULT_RUBY_VERSION).unwrap_or(false)
        }) {
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
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
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
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
        };
        let ver = RubyModule.resolved_version(&pm, &dep).unwrap();
        assert_eq!(
            ver,
            Some("3.2.0".into()),
            "must return pinned dep.version, not rbenv local"
        );
    }
}
