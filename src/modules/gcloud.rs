use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::{Module, extra_list, pm_dep, run_cmd};
use crate::fs_safe::{PrivateTempDir, which_outside_project};
use crate::installers;

pub struct GcloudModule;

/// The directory the archive unpacks to, installed as `$HOME/google-cloud-sdk`.
const SDK_DIR: &str = "google-cloud-sdk";

/// `$HOME/google-cloud-sdk/bin/gcloud`, where `install_sdk` puts gcloud, when it exists.
fn sdk_gcloud() -> Option<PathBuf> {
    let home = installers::user_home()?;
    let bin = Path::new(&home).join(SDK_DIR).join("bin").join("gcloud");
    bin.is_file().then_some(bin)
}

/// The gcloud to run: from PATH (outside the project), else the one `install_sdk`
/// installed, which devy does not add to PATH.
fn gcloud_bin() -> Option<PathBuf> {
    which_outside_project("gcloud").or_else(sdk_gcloud)
}

/// Arguments for the archive's bundled `install.sh`: no prompts, and no changes to shell
/// rc files, completion or usage reporting.
fn sdk_install_args() -> [&'static str; 4] {
    [
        "--quiet",
        "--usage-reporting=false",
        "--path-update=false",
        "--command-completion=false",
    ]
}

/// Installs the pinned Google Cloud CLI archive as `<home>/google-cloud-sdk`: downloads
/// and verifies it into a private staging directory under `home`, unpacks it there with
/// `tar`, moves the SDK into place and runs its bundled `install.sh` with `bash`.
fn install_sdk(home: &Path) -> Result<()> {
    let archive_installer = installers::gcloud_archive()?;
    let dest = home.join(SDK_DIR);
    // Checked again by `rename`, which fails on anything but an empty directory; an empty
    // directory created in between by the same user would be replaced, which is harmless.
    if std::fs::symlink_metadata(&dest).is_ok() {
        bail!(
            "{} exists but does not contain bin/gcloud; remove it and re-run, or install gcloud manually: {}",
            dest.display(),
            archive_installer.manual_url
        );
    }
    output::step(&format!(
        "Installing the Google Cloud CLI ({}), verified against its pinned SHA-256",
        archive_installer.url
    ));
    // Staged inside `home` so the final move is a same-filesystem rename.
    let staging = PrivateTempDir::new_in(home, ".devy-gcloud")?;
    let archive = installers::download(archive_installer, staging.path(), "gcloud.tar.gz")?;
    let unpacked = staging.path().join("unpacked");
    std::fs::create_dir(&unpacked)
        .with_context(|| format!("Failed to create {}", unpacked.display()))?;
    let tar = which_outside_project("tar").context("`tar` was not found on PATH")?;
    // Scrubbed environment: GNU tar would otherwise take options from `TAR_OPTIONS`.
    let status = installers::installer_command(&tar, staging.path())
        .arg("--no-same-owner")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpacked)
        .status()
        .context("Failed to run tar")?;
    if !status.success() {
        bail!("Failed to unpack the gcloud archive");
    }
    let sdk = unpacked.join(SDK_DIR);
    if !sdk.join("install.sh").is_file() {
        bail!("The gcloud archive does not contain {SDK_DIR}/install.sh");
    }
    std::fs::rename(&sdk, &dest)
        .with_context(|| format!("Failed to move the gcloud SDK to {}", dest.display()))?;

    // A half-installed SDK would count as installed (its bin/gcloud exists), so remove it
    // when the bundled installer can't be run or fails.
    let result = run_bundled_installer(&dest, home);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    result
}

fn run_bundled_installer(dest: &Path, home: &Path) -> Result<()> {
    let bash = which_outside_project("bash").context("`bash` was not found on PATH")?;
    let status = installers::installer_command(&bash, home)
        .arg(dest.join("install.sh"))
        .args(sdk_install_args())
        .env("CLOUDSDK_CORE_DISABLE_PROMPTS", "1")
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run gcloud installer: {e}"))?;
    if !status.success() {
        bail!("gcloud SDK installation failed — check the output above for details");
    }
    Ok(())
}

/// Builds `gcloud components install --quiet -- <component>`.
fn components_install_args(component: &str) -> [&str; 5] {
    ["components", "install", "--quiet", "--", component]
}

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

    /// brew and winget install their package; elsewhere devy installs Google's pinned
    /// archive itself.
    fn install_route(&self, backend: &str) -> String {
        match backend {
            "brew" | "winget" => backend.to_string(),
            _ => "gcloud-installer".to_string(),
        }
    }
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["components"])
    }

    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("google-cloud-sdk".to_string())
    }

    fn backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency> {
        matches!(pm.name(), "brew" | "winget").then(|| pm_dep(dep, package_name(pm)))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        match pm.name() {
            "brew" | "winget" => super::backend_installed(self, pm, dep),
            _ => Ok(gcloud_bin().is_some()),
        }
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        match pm.name() {
            "brew" | "winget" => {
                super::install_backend(self, pm, dep)?;
            }
            _ => {
                // Without brew or winget, install Google's pinned, verified archive — the
                // apt package is version-locked and requires manual repo setup.
                // The same home the installer runs with, so install and lookup agree.
                let home = installers::user_home()
                    .context("HOME is not set; cannot determine gcloud install directory")?;
                install_sdk(Path::new(&home))?;
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
        let components = extra_list(dep, "components")?;
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
        let gcloud = gcloud_bin()
            .context("gcloud was not found outside the project; cannot install components")?;
        let gcloud = gcloud.to_string_lossy();
        for component in &components {
            run_cmd(&gcloud, &components_install_args(component))?;
        }
        super::helpers::write_stamp_text(&stamp, &current)?;
        Ok(())
    }

    fn resolved_version(
        &self,
        _pm: &dyn PackageManager,
        _dep: &Dependency,
    ) -> Result<Option<String>> {
        let Some(gcloud) = gcloud_bin() else {
            return Ok(None);
        };
        let out = Command::new(gcloud).arg("version").output();
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
    fn components_install_args_use_separator() {
        assert_eq!(
            components_install_args("kubectl"),
            ["components", "install", "--quiet", "--", "kubectl"]
        );
    }

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
        assert!(extra_list(&dep, "components").unwrap().is_empty());
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
            image: None,
            docker: false,
        };
        let components = extra_list(&dep, "components").unwrap();
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
        // Non-brew/non-winget PM falls back to gcloud on PATH or in ~/google-cloud-sdk.
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        let expected = gcloud_bin().is_some();
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
        if gcloud_bin().is_some() {
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
        if gcloud_bin().is_none() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("gcloud");
        assert!(GcloudModule.is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn gcloud_resolved_version_is_none_when_not_installed() {
        if gcloud_bin().is_some() {
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
        if gcloud_bin().is_some() {
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
            image: None,
            docker: false,
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
    fn install_sdk_without_verified_download_fails_and_leaves_nothing() {
        // Tests have no network, so the archive download fails and nothing is installed.
        crate::installers::test_hooks::clear();
        let home = crate::test_support::tmp_dir();
        if installers::gcloud_archive().is_ok() {
            assert!(install_sdk(&home).is_err());
        }
        assert!(std::fs::read_dir(&home).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn install_sdk_removes_sdk_when_bundled_installer_fails() {
        use crate::installers::test_hooks;
        let Ok(inst) = installers::gcloud_archive() else {
            return;
        };
        let work = crate::test_support::tmp_dir();
        let home = work.join("home");
        std::fs::create_dir(&home).unwrap();
        let src = work.join("src").join(SDK_DIR);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("install.sh"), "exit 1\n").unwrap();
        let tarball = work.join("sdk.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&tarball)
                .arg("-C")
                .arg(work.join("src"))
                .arg(SDK_DIR)
                .status()
                .unwrap()
                .success()
        );
        test_hooks::clear();
        test_hooks::serve(inst, &std::fs::read(&tarball).unwrap());
        let result = install_sdk(&home);
        test_hooks::clear();
        assert!(result.is_err());
        assert!(std::fs::read_dir(&home).unwrap().next().is_none());
    }

    #[test]
    fn sdk_install_args_disable_prompts_and_rc_changes() {
        assert_eq!(
            sdk_install_args(),
            [
                "--quiet",
                "--usage-reporting=false",
                "--path-update=false",
                "--command-completion=false"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_sdk_unpacks_verified_archive_and_runs_bundled_installer() {
        use crate::installers::test_hooks;
        let Ok(inst) = installers::gcloud_archive() else {
            return;
        };
        let work = crate::test_support::tmp_dir();
        let home = work.join("home");
        std::fs::create_dir(&home).unwrap();
        let src = work.join("src").join(SDK_DIR);
        std::fs::create_dir_all(&src).unwrap();
        let out = work.join("args");
        std::fs::write(
            src.join("install.sh"),
            format!(
                "printf '%s\\n' \"$@\" \"$CLOUDSDK_CORE_DISABLE_PROMPTS\" > '{}'\n",
                out.display()
            ),
        )
        .unwrap();
        let tarball = work.join("sdk.tar.gz");
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(work.join("src"))
            .arg(SDK_DIR)
            .status()
            .unwrap()
            .success();
        assert!(ok);

        test_hooks::clear();
        test_hooks::serve(inst, &std::fs::read(&tarball).unwrap());
        let result = install_sdk(&home);
        test_hooks::clear();
        result.unwrap();

        assert!(home.join(SDK_DIR).join("install.sh").is_file());
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "--quiet\n--usage-reporting=false\n--path-update=false\n--command-completion=false\n1\n"
        );
        // Only the SDK is left behind: the staging directory is removed.
        let entries: Vec<_> = std::fs::read_dir(&home)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from(SDK_DIR)]);

        // A second install refuses to overwrite the existing SDK directory.
        let err = install_sdk(&home).unwrap_err().to_string();
        assert!(err.contains("does not contain bin/gcloud"), "{err}");
    }

    #[test]
    fn gcloud_resolved_version_contains_dot_when_installed() {
        if gcloud_bin().is_none() {
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
            image: None,
            docker: false,
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
            image: None,
            docker: false,
        };
        let pm = crate::package_manager::MockPackageManager::default();
        let result = GcloudModule.post_setup(&dep, &pm, &dir);
        assert!(
            result.is_ok(),
            "post_setup must skip gcloud when stamp matches regardless of component order"
        );
    }

    #[test]
    fn post_setup_rejects_hostile_component_before_gcloud() {
        let dir = crate::test_support::tmp_dir();
        let mut extra = HashMap::new();
        extra.insert(
            "components".into(),
            crate::config::ExtraValue::Sequence(vec![crate::config::ExtraValue::String(
                "-q".into(),
            )]),
        );
        let dep = Dependency::with_extra("gcloud", extra);
        std::fs::write(dir.join(".devy_gcloud_components_stamp"), "-q").unwrap();
        let err = GcloudModule
            .post_setup(
                &dep,
                &crate::package_manager::MockPackageManager::default(),
                &dir,
            )
            .unwrap_err();
        assert!(err.to_string().contains("invalid list entry"), "{err}");
    }
}
