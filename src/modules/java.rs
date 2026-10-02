use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, pm_dep};

pub struct JavaModule;

fn winget_package_id(dep: &Dependency) -> String {
    let major = dep
        .version
        .as_deref()
        .and_then(|v| v.split('.').next())
        .unwrap_or("21");
    format!("Microsoft.OpenJDK.{major}")
}

/// The JDK nixpkgs installs when no supported version is given.
const NIX_DEFAULT_JDK: &str = "jdk21";

fn pkg_name(pm: &dyn PackageManager, dep: &Dependency) -> String {
    match pm.name() {
        "apt" => "default-jdk".into(),
        "winget" => winget_package_id(dep),
        "nix" => super::nix_install_attr(&JavaModule, dep, NIX_DEFAULT_JDK),
        _ => "openjdk".into(),
    }
}

fn detect_java_home() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let out = Command::new("/usr/libexec/java_home").output().ok()?;
        if out.status.success() {
            let path = String::from_utf8(out.stdout).ok()?.trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
        None
    }
    #[cfg(target_os = "linux")]
    {
        // Prefer the distro-managed symlink first (works on any arch/version).
        if std::path::Path::new("/usr/lib/jvm/default-java").exists() {
            return Some("/usr/lib/jvm/default-java".into());
        }
        // Fall back to deriving JAVA_HOME from the `java` binary on PATH.
        if let Ok(java) = which::which("java")
            && let Ok(resolved) = java.canonicalize()
        {
            // java is typically at $JAVA_HOME/bin/java — go up two levels.
            if let Some(home) = resolved.ancestors().nth(2) {
                return Some(home.display().to_string());
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("JAVA_HOME").ok()
    }
}

impl Module for JavaModule {
    fn nix_versioned_attr(&self, version: &str) -> Option<String> {
        // `21` or `21.0.2` → `jdk21`.
        super::helpers::allowlisted_attr(version, 1, &["8", "11", "17", "21", "25"], |v| {
            format!("jdk{v}")
        })
    }

    fn nix_attr(&self, dep: &Dependency) -> Option<String> {
        Some(super::nix_install_attr(self, dep, NIX_DEFAULT_JDK))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        if pm.name() == "nix" {
            return super::pkg_installed(self, pm, dep, NIX_DEFAULT_JDK);
        }
        pm.is_package_installed(&pm_dep(dep, &pkg_name(pm, dep)))
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.install_package(&pm_dep(dep, &pkg_name(pm, dep)))
    }

    fn env_vars(
        &self,
        _dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        if let Some(home) = detect_java_home() {
            vars.insert("JAVA_HOME".into(), home);
        }
        vars
    }

    fn path_prepends(&self, _dep: &Dependency, _project_root: &std::path::Path) -> Vec<String> {
        detect_java_home()
            .map(|home| vec![format!("{home}/bin")])
            .unwrap_or_default()
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let pom = project_root.join("pom.xml");
        if pom.exists() {
            let stamp_path = project_root.join(".devy_java_stamp");
            if stamp_matches(&stamp_path, &pom) {
                output::skip("Java dependencies up to date");
                return Ok(());
            }
            let mvn = if project_root.join("mvnw").exists() {
                "./mvnw"
            } else {
                "mvn"
            };
            output::step("Running mvn dependency:resolve");
            let status = Command::new(mvn)
                .args(["-B", "dependency:resolve"])
                .current_dir(project_root)
                .status()
                .with_context(|| format!("Failed to run `{mvn} -B dependency:resolve`"))?;
            if !status.success() {
                anyhow::bail!(
                    "`{mvn} dependency:resolve` failed — check the output above for details"
                );
            }
            write_stamp(&stamp_path, &pom);
            output::success("Maven dependencies resolved");
            return Ok(());
        }

        let gradle_kts = project_root.join("build.gradle.kts");
        let gradle = project_root.join("build.gradle");
        let manifest = if gradle_kts.exists() {
            Some(gradle_kts)
        } else if gradle.exists() {
            Some(gradle)
        } else {
            None
        };

        if let Some(manifest) = manifest {
            let stamp_path = project_root.join(".devy_java_stamp");
            if stamp_matches(&stamp_path, &manifest) {
                output::skip("Java dependencies up to date");
                return Ok(());
            }
            let gradlew = if project_root.join("gradlew").exists() {
                "./gradlew"
            } else {
                "gradle"
            };
            output::step("Running gradle dependencies");
            let status = Command::new(gradlew)
                .args(["--no-daemon", "dependencies"])
                .current_dir(project_root)
                .status()
                .with_context(|| format!("Failed to run `{gradlew} dependencies`"))?;
            if !status.success() {
                anyhow::bail!(
                    "`{gradlew} dependencies` failed — check the output above for details"
                );
            }
            write_stamp(&stamp_path, &manifest);
            output::success("Gradle dependencies resolved");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn java_is_not_a_service() {
        assert!(!JavaModule.is_service());
    }

    #[test]
    fn pkg_name_apt() {
        let pm = MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(pkg_name(&pm, &Dependency::simple("java")), "default-jdk");
    }

    #[test]
    fn pkg_name_winget_defaults_to_21() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(
            pkg_name(&pm, &Dependency::simple("java")),
            "Microsoft.OpenJDK.21"
        );
    }

    #[test]
    fn pkg_name_winget_uses_version_field() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        let dep = Dependency {
            version: Some("17".into()),
            ..Dependency::simple("java")
        };
        assert_eq!(pkg_name(&pm, &dep), "Microsoft.OpenJDK.17");
    }

    #[test]
    fn pkg_name_winget_uses_major_from_semver() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        let dep = Dependency {
            version: Some("21.0.2".into()),
            ..Dependency::simple("java")
        };
        assert_eq!(pkg_name(&pm, &dep), "Microsoft.OpenJDK.21");
    }

    #[test]
    fn pkg_name_brew() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(pkg_name(&pm, &Dependency::simple("java")), "openjdk");
    }

    #[test]
    fn java_is_installed_delegates_to_pm() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            JavaModule
                .is_installed(&pm, &Dependency::simple("java"))
                .unwrap()
        );
    }

    #[test]
    fn java_not_installed_when_pm_reports_false() {
        let pm = MockPackageManager::default();
        assert!(
            !JavaModule
                .is_installed(&pm, &Dependency::simple("java"))
                .unwrap()
        );
    }

    #[test]
    fn java_install_propagates_pm_error() {
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        assert!(
            JavaModule
                .install(&pm, &Dependency::simple("java"))
                .is_err()
        );
    }

    #[test]
    fn java_env_vars_returns_map() {
        let dep = Dependency::simple("java");
        // env_vars must not panic; JAVA_HOME presence depends on the environment.
        let vars = JavaModule.env_vars(&dep, std::path::Path::new("/tmp"));
        if let Some(home) = vars.get("JAVA_HOME") {
            assert!(!home.is_empty(), "JAVA_HOME must not be empty if present");
        }
    }

    #[test]
    fn java_path_prepends_contains_java_home_bin_when_detected() {
        let dep = Dependency::simple("java");
        let prepends = JavaModule.path_prepends(&dep, std::path::Path::new("/tmp"));
        if detect_java_home().is_some() {
            assert_eq!(
                prepends.len(),
                1,
                "must have exactly one PATH entry when JAVA_HOME is detected"
            );
            assert!(
                prepends[0].ends_with("/bin"),
                "PATH entry must point to $JAVA_HOME/bin"
            );
        } else {
            assert!(
                prepends.is_empty(),
                "must be empty when JAVA_HOME cannot be detected"
            );
        }
    }

    #[test]
    fn java_post_setup_no_build_file_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let dep = Dependency::simple("java");
        let pm = MockPackageManager::default();
        assert!(
            JavaModule.post_setup(&dep, &pm, &dir).is_ok(),
            "post_setup must return Ok when no pom.xml or build.gradle exists"
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
    fn java_post_setup_skips_maven_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let pom = dir.join("pom.xml");
        std::fs::write(&pom, "<project/>").unwrap();
        std::fs::write(
            dir.join(".devy_java_stamp"),
            file_mtime_secs(&pom).to_string(),
        )
        .unwrap();
        let dep = Dependency::simple("java");
        let pm = MockPackageManager::default();
        // mvn would be invoked and likely fail if stamp check is bypassed.
        assert!(
            JavaModule.post_setup(&dep, &pm, &dir).is_ok(),
            "post_setup must skip mvn when stamp matches"
        );
    }

    #[test]
    fn java_post_setup_skips_gradle_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let build = dir.join("build.gradle");
        std::fs::write(&build, "").unwrap();
        std::fs::write(
            dir.join(".devy_java_stamp"),
            file_mtime_secs(&build).to_string(),
        )
        .unwrap();
        let dep = Dependency::simple("java");
        let pm = MockPackageManager::default();
        assert!(
            JavaModule.post_setup(&dep, &pm, &dir).is_ok(),
            "post_setup must skip gradle when stamp matches"
        );
    }

    #[test]
    fn java_post_setup_prefers_maven_over_gradle_when_both_present() {
        // pom.xml takes priority if both pom.xml and build.gradle exist.
        // We verify this by writing a matching stamp only for pom.xml — if Gradle
        // ran instead, it would try to spawn `gradle`/`./gradlew` and fail.
        let dir = crate::test_support::tmp_dir();
        let pom = dir.join("pom.xml");
        std::fs::write(&pom, "<project/>").unwrap();
        std::fs::write(dir.join("build.gradle"), "").unwrap();
        std::fs::write(
            dir.join(".devy_java_stamp"),
            file_mtime_secs(&pom).to_string(),
        )
        .unwrap();
        let dep = Dependency::simple("java");
        let pm = MockPackageManager::default();
        assert!(
            JavaModule.post_setup(&dep, &pm, &dir).is_ok(),
            "post_setup must use Maven (pom.xml) when both build files are present"
        );
    }
}
