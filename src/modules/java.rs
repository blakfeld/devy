use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

/// The JDK home devy exports for `dep`. Under brew and nix it is the home of the JDK
/// devy installed for `dep`, or `None` when that one isn't installed: another JDK on the
/// system is never presented as the requested one. apt and winget detect the system JDK.
fn java_home(pm: &dyn PackageManager, dep: &Dependency) -> Option<String> {
    let installed = |store: Option<&Path>| {
        let bin = pm.package_bin_dir(&JavaModule.backend_package(pm, dep)?)?;
        installed_java_home(&bin, store)
    };
    match pm.name() {
        // Never a JDK inside the project (an `opt` link into it).
        "brew" => installed(None).filter(|home| !crate::fs_safe::is_project_local(home)),
        "nix" => installed(Some(Path::new("/nix/store"))),
        _ => detect_java_home().map(PathBuf::from),
    }
    .map(|home| home.to_string_lossy().into_owned())
}

/// The home of the JDK whose `java` is in `bin` (`<prefix>/opt/<formula>/bin`, or the nix
/// profile `bin`): two levels above the real path of `java`, which must contain
/// `bin/java`. Resolving every link covers each layout (Homebrew's
/// `libexec/openjdk.jdk/Contents/Home`, Linuxbrew's `libexec`, nixpkgs' `lib/openjdk`).
///
/// The home is given through `bin`'s parent, which stays put across upgrades, at the
/// same place relative to it as the real home is in the package: `<prefix>/opt/openjdk/
/// libexec/openjdk.jdk/Contents/Home` rather than the versioned Cellar path. Under nix
/// (`store` set), the real home must be inside `store` (and not `store` itself); the
/// profile path is used when the profile has it and it leads to the same JDK, otherwise
/// the real home; under brew a rebased path leading elsewhere gives `None`.
fn installed_java_home(bin: &Path, store: Option<&Path>) -> Option<PathBuf> {
    let has_java = |home: &Path| home.join("bin").join("java").is_file();
    let java = bin.join("java").canonicalize().ok()?;
    let home = java.ancestors().nth(2)?;
    let stable_root = bin.parent()?;
    let package_root = match store {
        None => stable_root.canonicalize().ok()?,
        Some(store) => {
            let store = store.canonicalize().unwrap_or_else(|_| store.to_path_buf());
            let object = home.strip_prefix(&store).ok()?.components().next()?;
            store.join(object)
        }
    };
    // The rebased home must resolve to the very JDK checked above. Under nix a store
    // object that is itself the home would rebase onto the whole merged profile, so the
    // store path is used instead.
    let stable = home
        .strip_prefix(&package_root)
        .ok()
        .filter(|rel| store.is_none() || !rel.as_os_str().is_empty())
        .map(|rel| stable_root.join(rel))
        .filter(|stable| has_java(stable) && stable.canonicalize().ok().as_deref() == Some(home));
    match (stable, store) {
        (Some(stable), _) => Some(stable),
        (None, Some(_)) if has_java(home) => Some(home.to_path_buf()),
        _ => None,
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

    fn backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency> {
        Some(pm_dep(dep, &pkg_name(pm, dep)))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        if pm.name() == "nix" {
            return super::pkg_installed(self, pm, dep, NIX_DEFAULT_JDK);
        }
        super::backend_installed(self, pm, dep)
    }

    fn resolved_version(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
    ) -> Result<Option<String>> {
        super::pkg_resolved_version(self, pm, dep, NIX_DEFAULT_JDK)
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        super::install_backend(self, pm, dep)
    }

    fn backend_env_vars(
        &self,
        dep: &Dependency,
        pm: &dyn PackageManager,
        _project_root: &Path,
        _mode: crate::commands::ports::PortMode,
    ) -> HashMap<String, String> {
        java_home(pm, dep)
            .map(|home| HashMap::from([("JAVA_HOME".to_string(), home)]))
            .unwrap_or_default()
    }

    fn backend_path_prepends(
        &self,
        dep: &Dependency,
        pm: &dyn PackageManager,
        _project_root: &Path,
    ) -> Vec<String> {
        java_home(pm, dep)
            .map(|home| vec![format!("{home}/bin")])
            .unwrap_or_default()
    }

    fn setup_steps(&self, _dep: &Dependency, project_root: &Path) -> Vec<String> {
        if project_root.join("pom.xml").exists() {
            return if project_root.join("mvnw").exists() {
                vec!["./mvnw -B dependency:resolve (repository script)".to_string()]
            } else {
                vec!["mvn -B dependency:resolve (pom.xml plugins)".to_string()]
            };
        }
        super::helpers::gradle_steps(project_root)
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
            write_stamp(&stamp_path, &pom)?;
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
            write_stamp(&stamp_path, &manifest)?;
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

    /// Everything the module contributes for `dep` under `pm`: `JAVA_HOME` and its PATH
    /// entries, from both the backend-independent and the backend-aware hooks.
    fn contributions(pm: &dyn PackageManager, dep: &Dependency) -> (Option<String>, Vec<String>) {
        let root = std::path::Path::new("/tmp");
        let mut vars = JavaModule.env_vars(dep, root);
        vars.extend(JavaModule.backend_env_vars(
            dep,
            pm,
            root,
            crate::commands::ports::PortMode::ReadOnly,
        ));
        let mut paths = JavaModule.path_prepends(dep, root);
        paths.extend(JavaModule.backend_path_prepends(dep, pm, root));
        (vars.get("JAVA_HOME").cloned(), paths)
    }

    /// A brew mock whose formula bin dirs are `<prefix>/opt/<formula>/bin`, with the
    /// formula Homebrew installs (`name@version` for a devy.yml pin).
    fn brew_pm(prefix: &std::path::Path) -> MockPackageManager {
        let prefix = prefix.to_path_buf();
        MockPackageManager {
            name: "brew",
            package_bin_dir: Some(Box::new(move |pkg: &Dependency| {
                let formula = crate::package_manager::brew_formula_name(pkg);
                let dir = prefix.join("opt").join(formula).join("bin");
                dir.is_dir().then_some(dir)
            })),
            ..Default::default()
        }
    }

    /// Builds a Homebrew keg for `formula` at `version` under `prefix`, laid out as
    /// macOS Homebrew installs openjdk: `opt/<formula>` links to the Cellar keg, whose
    /// `bin/java` links into `libexec/openjdk.jdk/Contents/Home`. Returns that home as
    /// reached through `opt/<formula>`.
    #[cfg(unix)]
    fn fake_brew_jdk(prefix: &std::path::Path, formula: &str, version: &str) -> String {
        use std::os::unix::fs::symlink;
        let keg = prefix.join("Cellar").join(formula).join(version);
        let home = keg.join("libexec/openjdk.jdk/Contents/Home");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/java"), "").unwrap();
        std::fs::create_dir_all(keg.join("bin")).unwrap();
        symlink(
            "../libexec/openjdk.jdk/Contents/Home/bin/java",
            keg.join("bin/java"),
        )
        .unwrap();
        std::fs::create_dir_all(prefix.join("opt")).unwrap();
        symlink(
            format!("../Cellar/{formula}/{version}"),
            prefix.join("opt").join(formula),
        )
        .unwrap();
        prefix
            .join("opt")
            .join(formula)
            .join("libexec/openjdk.jdk/Contents/Home")
            .display()
            .to_string()
    }

    #[cfg(unix)]
    #[test]
    fn java_home_follows_the_brew_installed_jdk() {
        let prefix = crate::test_support::tmp_dir();
        let home = fake_brew_jdk(&prefix, "openjdk", "21.0.5");
        let (java_home, paths) = contributions(&brew_pm(&prefix), &Dependency::simple("java"));
        assert_eq!(java_home.as_deref(), Some(home.as_str()));
        assert_eq!(paths, vec![format!("{home}/bin")]);
    }

    #[cfg(unix)]
    #[test]
    fn java_home_follows_the_pinned_brew_formula() {
        let prefix = crate::test_support::tmp_dir();
        fake_brew_jdk(&prefix, "openjdk", "21.0.5");
        let home17 = fake_brew_jdk(&prefix, "openjdk@17", "17.0.13");
        let dep = Dependency {
            version: Some("17".into()),
            ..Dependency::simple("java")
        };
        let (java_home, paths) = contributions(&brew_pm(&prefix), &dep);
        assert_eq!(java_home.as_deref(), Some(home17.as_str()));
        assert_eq!(paths, vec![format!("{home17}/bin")]);
    }

    #[test]
    fn java_contributes_nothing_when_the_brew_jdk_is_not_installed() {
        // Another JDK on the system must never stand in for the one devy installs.
        let prefix = crate::test_support::tmp_dir();
        let (java_home, paths) = contributions(&brew_pm(&prefix), &Dependency::simple("java"));
        assert_eq!(java_home, None);
        assert!(paths.is_empty(), "{paths:?}");
    }

    #[cfg(unix)]
    #[test]
    fn java_home_under_brew_refuses_a_jdk_inside_the_project() {
        let project = crate::test_support::tmp_dir();
        crate::fs_safe::set_project_root(&project);
        fake_brew_jdk(&project, "openjdk", "21.0.5");
        let (java_home, paths) = contributions(&brew_pm(&project), &Dependency::simple("java"));
        assert_eq!(java_home, None);
        assert!(paths.is_empty(), "{paths:?}");
    }

    /// A nix profile at `<root>/.devy/nix-profile`, linking to a profile object in the
    /// fake store `<root>/store` that links `bin/java` and (with `link_lib`) `lib/openjdk`
    /// into a JDK store object laid out as nixpkgs' openjdk on Linux. Returns the store,
    /// the profile `bin` and the JDK home inside the store, canonicalized.
    #[cfg(unix)]
    fn fake_nix_jdk(
        root: &std::path::Path,
        link_lib: bool,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::symlink;
        let store = root.join("store");
        let jdk = store.join("abc123-openjdk-21.0.5");
        let home = jdk.join("lib/openjdk");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/java"), "").unwrap();
        std::fs::create_dir_all(jdk.join("bin")).unwrap();
        symlink("../lib/openjdk/bin/java", jdk.join("bin/java")).unwrap();
        let profile = store.join("def456-profile");
        std::fs::create_dir_all(profile.join("bin")).unwrap();
        symlink(jdk.join("bin/java"), profile.join("bin/java")).unwrap();
        if link_lib {
            std::fs::create_dir_all(profile.join("lib")).unwrap();
            symlink(&home, profile.join("lib/openjdk")).unwrap();
        }
        std::fs::create_dir_all(root.join(".devy")).unwrap();
        symlink(&profile, root.join(".devy/nix-profile")).unwrap();
        (
            store,
            root.join(".devy/nix-profile/bin"),
            home.canonicalize().unwrap(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn nix_java_home_is_reached_through_the_profile() {
        let root = crate::test_support::tmp_dir();
        let (store, bin, _) = fake_nix_jdk(&root, true);
        assert_eq!(
            installed_java_home(&bin, Some(&store)),
            Some(root.join(".devy/nix-profile/lib/openjdk"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn nix_java_home_falls_back_to_the_store_path() {
        // A profile without `lib/openjdk` (another package's `lib` won the merge).
        let root = crate::test_support::tmp_dir();
        let (store, bin, home) = fake_nix_jdk(&root, false);
        assert_eq!(installed_java_home(&bin, Some(&store)), Some(home));
    }

    #[cfg(unix)]
    #[test]
    fn nix_java_home_ignores_a_profile_path_to_another_jdk() {
        // The profile's `lib/openjdk` belongs to a different JDK than its `bin/java`.
        let root = crate::test_support::tmp_dir();
        let (store, bin, home) = fake_nix_jdk(&root, false);
        let other = store.join("zzz999-openjdk-17.0.13/lib/openjdk");
        std::fs::create_dir_all(other.join("bin")).unwrap();
        std::fs::write(other.join("bin/java"), "").unwrap();
        let profile = root.join(".devy/nix-profile");
        std::fs::create_dir_all(profile.join("lib")).unwrap();
        std::os::unix::fs::symlink(&other, profile.join("lib/openjdk")).unwrap();
        assert_eq!(installed_java_home(&bin, Some(&store)), Some(home));
    }

    #[cfg(unix)]
    #[test]
    fn nix_java_home_is_the_store_object_when_it_is_the_home() {
        // A JDK whose store object is its home rebases onto the whole profile; the store
        // path is exported instead.
        use std::os::unix::fs::symlink;
        let root = crate::test_support::tmp_dir();
        let store = root.join("store");
        let jdk = store.join("abc123-zulu-21");
        std::fs::create_dir_all(jdk.join("bin")).unwrap();
        std::fs::write(jdk.join("bin/java"), "").unwrap();
        let profile = store.join("def456-profile");
        std::fs::create_dir_all(profile.join("bin")).unwrap();
        symlink(jdk.join("bin/java"), profile.join("bin/java")).unwrap();
        std::fs::create_dir_all(root.join(".devy")).unwrap();
        symlink(&profile, root.join(".devy/nix-profile")).unwrap();
        assert_eq!(
            installed_java_home(&root.join(".devy/nix-profile/bin"), Some(&store)),
            Some(jdk.canonicalize().unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn nix_java_home_must_be_inside_and_not_the_store() {
        let root = crate::test_support::tmp_dir();
        let (_, bin, _) = fake_nix_jdk(&root, true);
        // The fake store is not /nix/store.
        assert_eq!(
            installed_java_home(&bin, Some(std::path::Path::new("/nix/store"))),
            None
        );
        // A `java` at `<store>/bin/java` gives the store itself as the home.
        let store = root.join("flat-store");
        std::fs::create_dir_all(store.join("bin")).unwrap();
        std::fs::write(store.join("bin/java"), "").unwrap();
        assert_eq!(installed_java_home(&store.join("bin"), Some(&store)), None);
    }

    #[cfg(unix)]
    #[test]
    fn java_home_under_nix_refuses_a_home_outside_the_nix_store() {
        // Nothing else (/usr/lib/jvm/default-java, `java` on PATH) stands in for it.
        let root = crate::test_support::tmp_dir();
        let (_, bin, _) = fake_nix_jdk(&root, true);
        let pm = MockPackageManager {
            name: "nix",
            package_bin_dir: Some(Box::new(move |_: &Dependency| Some(bin.clone()))),
            ..Default::default()
        };
        let (java_home, paths) = contributions(&pm, &Dependency::simple("java"));
        assert_eq!(java_home, None);
        assert!(paths.is_empty(), "{paths:?}");
    }

    #[test]
    fn java_home_under_apt_and_winget_never_asks_for_a_package_bin_dir() {
        for name in ["apt", "winget"] {
            let pm = MockPackageManager {
                name,
                package_bin_dir: Some(Box::new(|_: &Dependency| {
                    panic!("apt and winget have no JDK bin dir")
                })),
                ..Default::default()
            };
            contributions(&pm, &Dependency::simple("java"));
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
