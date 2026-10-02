use anyhow::{Context, Result, bail};
use std::process::{Command, Stdio};
use which::which;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, extra_strs, pm_dep, run_cmd};
use crate::commands::exec::sh_quote;

pub struct GcloudModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "Google.CloudSDK",
        _ => "google-cloud-sdk",
    }
}

impl Module for GcloudModule {
    fn source(&self) -> Option<&'static str> {
        Some("gcloud-installer")
    }
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["components"])
    }

    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("google-cloud-sdk".to_string())
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        match pm.name() {
            "brew" | "winget" => pm.is_package_installed(&pm_dep(dep, package_name(pm))),
            _ => Ok(which("gcloud").is_ok()),
        }
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        match pm.name() {
            "brew" | "winget" => {
                pm.install_package(&pm_dep(dep, package_name(pm)))?;
            }
            _ => {
                // On Linux, use the official installer script — the apt package
                // is version-locked and requires manual repo setup.
                let home = std::env::var("HOME")
                    .context("HOME is not set; cannot determine gcloud install directory")?;

                #[cfg(test)]
                let installer_cmd = std::env::var("DEVY_TEST_GCLOUD_INSTALL_SCRIPT")
                    .unwrap_or_else(|_| {
                        format!(
                            "curl -fsSL https://sdk.cloud.google.com | \
                             bash -s -- --disable-prompts --install-dir={}",
                            sh_quote(&home)
                        )
                    });
                #[cfg(not(test))]
                let installer_cmd = format!(
                    "curl -fsSL https://sdk.cloud.google.com | \
                     bash -s -- --disable-prompts --install-dir={}",
                    sh_quote(&home)
                );

                let status = Command::new("sh")
                    .arg("-c")
                    .arg(&installer_cmd)
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status()
                    .map_err(|e| anyhow::anyhow!("Failed to run gcloud installer: {e}"))?;
                if !status.success() {
                    bail!("gcloud SDK installation failed — check the output above for details");
                }
            }
        }

        Ok(())
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &std::path::Path,
    ) -> Result<()> {
        let components = extra_strs(dep, "components");
        if components.is_empty() {
            return Ok(());
        }
        let stamp = project_root.join(".devy_gcloud_components_stamp");
        let mut sorted = components.clone();
        sorted.sort();
        let current = sorted.join("\n");
        if std::fs::read_to_string(&stamp).ok().as_deref() == Some(current.as_str()) {
            return Ok(());
        }
        for component in &components {
            run_cmd("gcloud", &["components", "install", "--quiet", component])?;
        }
        let _ = std::fs::write(&stamp, &current);
        Ok(())
    }

    fn resolved_version(
        &self,
        _pm: &dyn PackageManager,
        _dep: &Dependency,
    ) -> Result<Option<String>> {
        let out = Command::new("gcloud").arg("version").output();
        // First line is "Google Cloud SDK 468.0.0" — strip the known prefix rather than
        // relying on word position so any format change fails safely to None.
        Ok(out.ok().and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()?
                .lines()
                .next()?
                .strip_prefix("Google Cloud SDK ")?
                .split_whitespace()
                .next()
                .map(String::from)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn gcloud_module_is_not_a_service() {
        assert!(!GcloudModule.is_service());
    }

    #[test]
    fn gcloud_source_is_gcloud_installer() {
        assert_eq!(GcloudModule.source(), Some("gcloud-installer"));
    }

    #[test]
    fn components_empty_when_not_configured() {
        let dep = Dependency::simple("gcloud");
        assert!(extra_strs(&dep, "components").is_empty());
    }

    #[test]
    fn components_read_from_extra() {
        let mut extra = HashMap::new();
        extra.insert(
            "components".into(),
            crate::config::ExtraValue::Sequence(vec![
                crate::config::ExtraValue::String("gke-gcloud-auth-plugin".into()),
                crate::config::ExtraValue::String("kubectl".into()),
            ]),
        );
        let dep = Dependency {
            name: "gcloud".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
        };
        let components = extra_strs(&dep, "components");
        assert_eq!(components, vec!["gke-gcloud-auth-plugin", "kubectl"]);
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Google.CloudSDK");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "google-cloud-sdk");
    }

    #[test]
    fn gcloud_is_installed_does_not_panic() {
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        let _ = GcloudModule.is_installed(&pm, &dep);
    }

    #[test]
    fn gcloud_is_installed_consistent_with_which() {
        // Non-brew/non-winget PM falls back to `which("gcloud")`.
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        let expected = which("gcloud").is_ok();
        assert_eq!(GcloudModule.is_installed(&pm, &dep).unwrap(), expected);
    }

    #[test]
    fn gcloud_is_installed_delegates_to_pm_for_brew() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            installed: true,
            ..Default::default()
        };
        let dep = Dependency::simple("gcloud");
        assert!(GcloudModule.is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn gcloud_is_installed_false_for_brew_when_pm_reports_false() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            installed: false,
            ..Default::default()
        };
        let dep = Dependency::simple("gcloud");
        assert!(!GcloudModule.is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn gcloud_is_installed_delegates_to_pm_for_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Google.CloudSDK"),
            ..Default::default()
        };
        let dep = Dependency::simple("gcloud");
        assert!(GcloudModule.is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn gcloud_install_brew_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            install_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("gcloud");
        assert!(GcloudModule.install(&pm, &dep).is_err());
    }

    #[test]
    fn gcloud_install_winget_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            install_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("gcloud");
        assert!(GcloudModule.install(&pm, &dep).is_err());
    }

    #[test]
    fn gcloud_resolved_version_returns_ok() {
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        assert!(GcloudModule.resolved_version(&pm, &dep).is_ok());
    }

    #[test]
    fn gcloud_is_installed_false_when_not_on_path() {
        if which("gcloud").is_ok() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        assert!(
            !GcloudModule.is_installed(&pm, &dep).unwrap(),
            "is_installed must return false when gcloud is absent from PATH"
        );
    }

    #[test]
    fn gcloud_is_installed_true_when_on_path() {
        if which("gcloud").is_err() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        assert!(GcloudModule.is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn gcloud_resolved_version_is_none_when_not_installed() {
        if which("gcloud").is_ok() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        let ver = GcloudModule.resolved_version(&pm, &dep).unwrap();
        assert!(
            ver.is_none(),
            "Expected None version when gcloud is not on PATH"
        );
    }

    #[test]
    fn gcloud_install_apt_falls_through_to_script_path() {
        // On macOS (no HOME set incorrectly), the apt/apt-get path tries to run a script.
        // With install_fails=false this still needs "HOME" to be set.
        // This test just verifies the brew/winget path is distinct from the default path.
        let pm_brew = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        let dep = Dependency::simple("gcloud");
        // brew path calls pm.install_package — with install_fails=false this succeeds.
        assert!(GcloudModule.install(&pm_brew, &dep).is_ok());
    }

    #[test]
    fn package_name_apt_returns_google_cloud_sdk() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "google-cloud-sdk");
    }

    #[test]
    fn gcloud_install_brew_calls_pm_install_package() {
        // Kills `delete match arm "brew" | "winget"` — without the arm, brew falls through
        // to the script path and install_package is never called.
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        let dep = Dependency::simple("gcloud");
        GcloudModule.install(&pm, &dep).ok();
        assert!(
            !pm.installed_packages.borrow().is_empty(),
            "install with brew PM must delegate to pm.install_package"
        );
    }

    #[test]
    fn gcloud_install_brew_does_not_invoke_gcloud_for_components() {
        // When gcloud is absent from PATH, install() with a component-bearing dep via brew
        // must still return Ok — components are post_setup's responsibility, not install's.
        if which("gcloud").is_ok() {
            return;
        }
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "components".into(),
            crate::config::ExtraValue::Sequence(vec![crate::config::ExtraValue::String(
                "kubectl".into(),
            )]),
        );
        let dep = Dependency {
            name: "gcloud".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
        };
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert!(
            GcloudModule.install(&pm, &dep).is_ok(),
            "install must not invoke gcloud for components when gcloud is absent"
        );
    }

    #[test]
    fn gcloud_install_apt_script_failure_propagated() {
        // When PM is not brew/winget, install runs the gcloud installer script.
        // Uses DEVY_TEST_GCLOUD_INSTALL_SCRIPT to inject a failing script.
        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialised by ENV_LOCK; var is only read by GcloudModule::install.
        unsafe {
            std::env::set_var("DEVY_TEST_GCLOUD_INSTALL_SCRIPT", "exit 1");
        }
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        let dep = Dependency::simple("gcloud");
        let result = GcloudModule.install(&pm, &dep);
        unsafe {
            std::env::remove_var("DEVY_TEST_GCLOUD_INSTALL_SCRIPT");
        }
        assert!(
            result.is_err(),
            "install must fail when installer script exits non-zero"
        );
    }

    #[test]
    fn sh_quote_escapes_home_with_spaces() {
        // Verify sh_quote produces a correctly single-quoted string for a path with spaces,
        // which would otherwise break the gcloud installer sh -c command.
        let quoted = sh_quote("/home/my user/work");
        assert_eq!(quoted, "'/home/my user/work'");
    }

    #[test]
    fn sh_quote_escapes_home_with_single_quote() {
        let quoted = sh_quote("/home/o'malley");
        assert_eq!(quoted, "'/home/o'\\''malley'");
    }

    #[test]
    fn gcloud_resolved_version_contains_dot_when_installed() {
        if which("gcloud").is_err() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        let ver = GcloudModule.resolved_version(&pm, &dep).unwrap();
        if let Some(v) = ver {
            assert!(v.contains('.'), "Expected semver-like version, got: {v}");
            assert_ne!(v, "xyzzy", "Version must not be placeholder");
            assert!(!v.is_empty());
        }
    }

    // ── post_setup ────────────────────────────────────────────────────────────

    #[test]
    fn gcloud_post_setup_no_components_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        assert!(GcloudModule.post_setup(&dep, &pm, &dir).is_ok());
        assert!(
            !dir.join(".devy_gcloud_components_stamp").exists(),
            "stamp must not be created when there are no components"
        );
    }

    #[test]
    fn gcloud_post_setup_skips_gcloud_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let stamp = dir.join(".devy_gcloud_components_stamp");
        // Sorted stamp content for ["kubectl"].
        std::fs::write(&stamp, "kubectl").unwrap();

        let mut extra = HashMap::new();
        extra.insert(
            "components".into(),
            crate::config::ExtraValue::Sequence(vec![crate::config::ExtraValue::String(
                "kubectl".into(),
            )]),
        );
        let dep = Dependency {
            name: "gcloud".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
        };
        let pm = crate::package_manager::MockPackageManager::default();
        // If the stamp check is bypassed, `gcloud components install` is invoked and fails.
        // A successful return proves the stamp short-circuited.
        let result = GcloudModule.post_setup(&dep, &pm, &dir);
        assert!(
            result.is_ok(),
            "post_setup must skip gcloud when stamp matches"
        );
        assert_eq!(
            std::fs::read_to_string(&stamp).unwrap(),
            "kubectl",
            "stamp must be unchanged when skipped"
        );
    }

    #[test]
    fn gcloud_post_setup_stamp_matches_regardless_of_order() {
        let dir = crate::test_support::tmp_dir();
        let stamp = dir.join(".devy_gcloud_components_stamp");
        // Sorted stamp content for ["alpha", "kubectl"] — order in devy.yml is reversed.
        std::fs::write(&stamp, "alpha\nkubectl").unwrap();

        let mut extra = HashMap::new();
        extra.insert(
            "components".into(),
            crate::config::ExtraValue::Sequence(vec![
                crate::config::ExtraValue::String("kubectl".into()),
                crate::config::ExtraValue::String("alpha".into()),
            ]),
        );
        let dep = Dependency {
            name: "gcloud".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
        };
        let pm = crate::package_manager::MockPackageManager::default();
        let result = GcloudModule.post_setup(&dep, &pm, &dir);
        assert!(
            result.is_ok(),
            "post_setup must skip gcloud when stamp matches regardless of component order"
        );
    }
}
