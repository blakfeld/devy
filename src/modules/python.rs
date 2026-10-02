use anyhow::{Context, Result};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::Module;
use super::helpers::{stamp_matches, write_stamp};

pub struct PythonModule;

fn pkg_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "apt" => "python3",
        "winget" => "Python.Python.3",
        "nix" => "python3",
        _ => "python",
    }
}

fn venv_path(dep: &Dependency) -> Cow<'_, str> {
    dep.extra
        .get("venv_path")
        .and_then(|v| v.as_str())
        .map(Cow::Borrowed)
        .unwrap_or(Cow::Borrowed(".venv"))
}

fn venv_bin_dir() -> &'static str {
    if cfg!(target_os = "windows") {
        "Scripts"
    } else {
        "bin"
    }
}

enum InstallCmd {
    /// Run via `sh -c` (user-supplied string).
    Shell(String),
    /// Run directly without a shell (program + args).
    Direct(std::path::PathBuf, Vec<&'static str>),
}

/// Returns the active manifest file (pyproject.toml preferred over requirements.txt).
/// Single source of truth for manifest precedence, shared by stamp tracking and install detection.
fn find_manifest(project_root: &Path) -> Option<std::path::PathBuf> {
    let pyproject = project_root.join("pyproject.toml");
    if pyproject.exists() {
        return Some(pyproject);
    }
    let req = project_root.join("requirements.txt");
    if req.exists() {
        return Some(req);
    }
    None
}

/// Returns the manifest to track for stamp purposes, or None if a custom install_cmd is set.
fn manifest_for_stamp(dep: &Dependency, project_root: &Path) -> Option<std::path::PathBuf> {
    if dep.extra.contains_key("install_cmd") {
        return None; // custom shell command — can't track a manifest
    }
    find_manifest(project_root)
}

fn detect_install_cmd(
    dep: &Dependency,
    project_root: &Path,
    venv_dir: &Path,
) -> Option<InstallCmd> {
    if let Some(cmd) = dep.extra.get("install_cmd").and_then(|v| v.as_str()) {
        return Some(InstallCmd::Shell(cmd.to_string()));
    }
    let pip = venv_dir.join(venv_bin_dir()).join("pip");
    match find_manifest(project_root)?.file_name()?.to_str()? {
        "pyproject.toml" => Some(InstallCmd::Direct(pip, vec!["install", "-e", "."])),
        _ => Some(InstallCmd::Direct(
            pip,
            vec!["install", "-r", "requirements.txt"],
        )),
    }
}

impl Module for PythonModule {
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["venv_path", "install_cmd"])
    }

    fn nix_versioned_attr(&self, version: &str) -> Option<String> {
        // `3.12` or `3.12.4` → `python312`.
        super::helpers::allowlisted_attr(version, 2, &["3.11", "3.12", "3.13", "3.14"], |v| {
            format!("python{}", v.replace('.', ""))
        })
    }

    fn nix_attr(&self, dep: &Dependency) -> Option<String> {
        Some(super::nix_install_attr(self, dep, "python3"))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        super::pkg_installed(self, pm, dep, pkg_name(pm))
    }

    fn resolved_version(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
    ) -> Result<Option<String>> {
        super::pkg_resolved_version(self, pm, dep, pkg_name(pm))
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.install_package(&super::pkg_dep(self, pm, dep, pkg_name(pm)))
    }

    fn env_vars(
        &self,
        dep: &Dependency,
        project_root: &std::path::Path,
    ) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        let abs_venv = project_root.join(&*venv_path(dep));
        vars.insert("VIRTUAL_ENV".into(), abs_venv.display().to_string());
        vars
    }

    fn path_prepends(&self, dep: &Dependency, project_root: &std::path::Path) -> Vec<String> {
        let venv_dir = project_root.join(&*venv_path(dep));
        vec![venv_dir.join(venv_bin_dir()).display().to_string()]
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let venv_dir = project_root.join(&*venv_path(dep));

        if !venv_dir.join("pyvenv.cfg").exists() {
            let python_bin = which::which("python3")
                .or_else(|_| which::which("python"))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "python3".into());

            let ok = Command::new(&python_bin)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                anyhow::bail!(
                    "Python installation appears incomplete — `{python_bin} --version` failed"
                );
            }

            output::step("Creating Python virtualenv");
            let venv_str = venv_dir
                .to_str()
                .context("venv path contains non-UTF-8 bytes")?;
            let status = Command::new(&python_bin)
                .args(["-m", "venv", venv_str])
                .status()
                .context("Failed to create Python virtualenv")?;
            if !status.success() {
                anyhow::bail!("`{python_bin} -m venv` failed — check the output above for details");
            }
            output::success("Virtualenv created");
        }

        let Some(install) = detect_install_cmd(dep, project_root, &venv_dir) else {
            return Ok(());
        };

        let stamp_path = venv_dir.join(".devy_stamp");
        let manifest = manifest_for_stamp(dep, project_root);
        if let Some(ref m) = manifest
            && stamp_matches(&stamp_path, m)
        {
            output::skip("Python dependencies up to date");
            return Ok(());
        }

        let status = match &install {
            InstallCmd::Shell(cmd) => {
                output::step(&format!("Running {cmd}"));
                // Intentional: install_cmd is documented as shell-executed user code.
                Command::new(crate::config::default_shell())
                    .args(["-c", cmd])
                    .current_dir(project_root)
                    .status()
                    .with_context(|| format!("Failed to run `{cmd}`"))?
            }
            InstallCmd::Direct(prog, args) => {
                let label = format!("{} {}", prog.display(), args.join(" "));
                output::step(&format!("Running {label}"));
                Command::new(prog)
                    .args(args)
                    .current_dir(project_root)
                    .status()
                    .with_context(|| format!("Failed to run `{label}`"))?
            }
        };

        if !status.success() {
            anyhow::bail!("Python dependency install failed — check the output above for details");
        }
        if let Some(ref m) = manifest {
            write_stamp(&stamp_path, m);
        }
        output::success("Python dependencies installed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;
    use std::collections::HashMap;

    #[test]
    fn python_is_not_a_service() {
        assert!(!PythonModule.is_service());
    }

    #[test]
    fn pkg_name_apt() {
        let pm = MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(pkg_name(&pm), "python3");
    }

    #[test]
    fn pkg_name_winget() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(pkg_name(&pm), "Python.Python.3");
    }

    #[test]
    fn pkg_name_brew() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(pkg_name(&pm), "python");
    }

    #[test]
    fn python_is_installed_delegates_to_pm() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            PythonModule
                .is_installed(&pm, &Dependency::simple("python"))
                .unwrap()
        );
    }

    #[test]
    fn python_not_installed_when_pm_reports_false() {
        let pm = MockPackageManager::default();
        assert!(
            !PythonModule
                .is_installed(&pm, &Dependency::simple("python"))
                .unwrap()
        );
    }

    #[test]
    fn python_install_propagates_pm_error() {
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        assert!(
            PythonModule
                .install(&pm, &Dependency::simple("python"))
                .is_err()
        );
    }

    #[test]
    fn venv_path_default_is_dot_venv() {
        let dep = Dependency::simple("python");
        assert_eq!(venv_path(&dep), ".venv");
    }

    #[test]
    fn venv_path_custom_from_extra() {
        let mut extra = HashMap::new();
        extra.insert(
            "venv_path".into(),
            crate::config::ExtraValue::String("venv".into()),
        );
        let dep = Dependency {
            name: "python".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
        };
        assert_eq!(venv_path(&dep), "venv");
    }

    #[test]
    fn python_env_vars_contains_virtual_env() {
        let dep = Dependency::simple("python");
        let vars = PythonModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert!(vars.contains_key("VIRTUAL_ENV"), "VIRTUAL_ENV must be set");
        assert!(vars["VIRTUAL_ENV"].ends_with("/.venv") || vars["VIRTUAL_ENV"].contains(".venv"));
    }

    #[test]
    fn python_path_prepends_points_into_venv() {
        let dep = Dependency::simple("python");
        let prepends = PythonModule.path_prepends(&dep, std::path::Path::new("/tmp"));
        assert_eq!(prepends.len(), 1);
        assert!(prepends[0].contains(".venv"));
    }

    #[test]
    fn post_setup_no_requirements_no_pyproject_is_noop() {
        // A temp dir with no requirements.txt or pyproject.toml and an existing
        // pyvenv.cfg — post_setup must return Ok without running pip.
        let dir = crate::test_support::tmp_dir();
        let venv = dir.join(".venv");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "").unwrap();

        let dep = Dependency::simple("python");
        let pm = MockPackageManager::default();
        assert!(PythonModule.post_setup(&dep, &pm, &dir).is_ok());
    }

    #[test]
    fn detect_install_cmd_explicit_override_returns_shell_variant() {
        let mut extra = HashMap::new();
        extra.insert(
            "install_cmd".into(),
            crate::config::ExtraValue::String("pip install mypackage".into()),
        );
        let dep = Dependency {
            name: "python".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
        };
        let dir = std::env::temp_dir();
        let venv = dir.join(".venv");
        match detect_install_cmd(&dep, &dir, &venv).unwrap() {
            InstallCmd::Shell(s) => assert_eq!(s, "pip install mypackage"),
            InstallCmd::Direct(..) => panic!("expected Shell variant"),
        }
    }

    #[test]
    fn detect_install_cmd_requirements_txt_returns_direct_variant() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("requirements.txt"), "").unwrap();

        let dep = Dependency::simple("python");
        let venv = dir.join(".venv");
        match detect_install_cmd(&dep, &dir, &venv).unwrap() {
            InstallCmd::Direct(_, args) => assert!(args.contains(&"requirements.txt")),
            InstallCmd::Shell(_) => panic!("expected Direct variant"),
        }
    }

    #[test]
    fn detect_install_cmd_pyproject_toml_preferred_over_requirements() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("pyproject.toml"), "").unwrap();
        std::fs::write(dir.join("requirements.txt"), "").unwrap();

        let dep = Dependency::simple("python");
        let venv = dir.join(".venv");
        match detect_install_cmd(&dep, &dir, &venv).unwrap() {
            InstallCmd::Direct(_, args) => {
                assert!(args.contains(&"-e"), "pyproject.toml must take priority");
            }
            InstallCmd::Shell(_) => panic!("expected Direct variant"),
        }
    }

    #[test]
    fn manifest_for_stamp_and_detect_install_cmd_agree_on_precedence() {
        // Both functions must select pyproject.toml when both files are present.
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("pyproject.toml"), "").unwrap();
        std::fs::write(dir.join("requirements.txt"), "").unwrap();

        let dep = Dependency::simple("python");
        let venv = dir.join(".venv");

        let stamp_manifest = manifest_for_stamp(&dep, &dir).unwrap();
        let install_cmd = detect_install_cmd(&dep, &dir, &venv).unwrap();

        assert!(
            stamp_manifest.ends_with("pyproject.toml"),
            "stamp should track pyproject.toml"
        );
        match install_cmd {
            InstallCmd::Direct(_, args) => {
                assert!(
                    args.contains(&"-e"),
                    "install_cmd should use pyproject.toml path"
                );
            }
            InstallCmd::Shell(_) => panic!("expected Direct variant"),
        }
    }
}
