#[cfg(any(test, target_os = "macos"))]
mod brew;
#[cfg(target_os = "macos")]
pub use brew::Homebrew;

#[cfg(any(test, target_os = "linux"))]
mod apt;
#[cfg(target_os = "linux")]
pub use apt::Apt;

#[cfg(any(test, target_os = "windows"))]
mod winget;
#[cfg(target_os = "windows")]
pub use winget::WinGet;

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
mod nix;
#[cfg(any(test, target_os = "macos", target_os = "linux"))]
pub use nix::NixPackageManager;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use nix::{OrphanedUnit, UnitPruner};

use anyhow::Result;
use std::path::PathBuf;

use crate::config::{Dependency, DevyConfig, PackageManagerChoice};
use crate::modules::LaunchSpec;

/// Standard system directories a tool is looked for in when it is not on PATH outside the
/// project (e.g. a minimal service-manager environment).
#[cfg(unix)]
const SYSTEM_TOOL_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/usr/local/bin"];
#[cfg(not(unix))]
const SYSTEM_TOOL_DIRS: &[&str] = &[];

/// Resolves a system tool devy runs by name (`docker`, `podman`, `systemctl`,
/// `journalctl`): the first match on PATH outside the project, else the first standard
/// system directory that has it. Never a bare name, which `Command` would resolve against
/// the full PATH — where an activated project environment (or `devy exec`) can put a
/// repository's own `bin` first. An absolute `name` is returned as is.
pub(crate) fn system_tool(name: &str) -> Option<PathBuf> {
    system_tool_in(
        name,
        crate::fs_safe::which_outside_project(name),
        SYSTEM_TOOL_DIRS,
    )
}

/// `system_tool` with the PATH lookup result and the fallback directories injected.
fn system_tool_in(name: &str, on_path: Option<PathBuf>, dirs: &[&str]) -> Option<PathBuf> {
    let path = std::path::Path::new(name);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    on_path.or_else(|| {
        dirs.iter()
            .map(|d| std::path::Path::new(d).join(name))
            .find(|p| p.is_file())
    })
}

/// `system_tool`, or an error naming the tool when it can't be found.
pub(crate) fn require_system_tool(name: &str) -> Result<PathBuf> {
    system_tool(name)
        .ok_or_else(|| anyhow::anyhow!("{}", not_found_message(name, SYSTEM_TOOL_DIRS)))
}

/// The error for a system tool found neither on PATH outside the project nor in `dirs`.
fn not_found_message(name: &str, dirs: &[&str]) -> String {
    if dirs.is_empty() {
        format!("Failed to run `{name}`: it was not found on PATH (outside the project)")
    } else {
        format!(
            "Failed to run `{name}`: it was not found on PATH (outside the project) or in {}",
            dirs.join(", ")
        )
    }
}

/// Where a service's log output can be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogSource {
    /// Files tailed in-process, in display order (e.g. stdout then stderr). Empty when
    /// there is nothing to read yet (e.g. a docker service with no container).
    Files(Vec<PathBuf>),
    /// A command that prints the log, already built for the requested line count and
    /// follow mode.
    Command(LogCommand),
    /// Logs can't be read for this backend; the message is shown as the error.
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogCommand {
    pub program: String,
    pub args: Vec<String>,
    pub kind: LogCommandKind,
}

/// What a log command reads, which decides how its output and failures are interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // journals exist only on Linux
pub enum LogCommandKind {
    /// The user journal (`journalctl --user`).
    UserJournal,
    /// The system journal for `unit` (`journalctl -u`), which may need extra permissions.
    SystemJournal { unit: String },
    /// A container's output (`docker logs`), written to both stdout and stderr.
    Container,
}

impl LogSource {
    /// `journalctl [--user] -u <unit> -n <lines> --no-pager -o cat [-f]`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // journals exist only on Linux
    pub fn journal(unit: &str, user: bool, lines: u32, follow: bool) -> Self {
        let mut args: Vec<String> = Vec::new();
        if user {
            args.push("--user".into());
        }
        let lines = lines.to_string();
        args.extend(["-u", unit, "-n", &lines, "--no-pager", "-o", "cat"].map(String::from));
        if follow {
            args.push("-f".into());
        }
        let kind = if user {
            LogCommandKind::UserJournal
        } else {
            LogCommandKind::SystemJournal { unit: unit.into() }
        };
        Self::Command(LogCommand {
            program: "journalctl".into(),
            args,
            kind,
        })
    }
}

pub trait PackageManager {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn bootstrap(&self) -> Result<()>;
    fn is_package_installed(&self, dep: &Dependency) -> Result<bool>;
    fn install_package(&self, dep: &Dependency) -> Result<()>;
    fn is_service_running(&self, name: &str) -> Result<bool>;
    /// Starts a service. `launch` is how to run it under backends that launch the
    /// process themselves (nix); others use the package's own service definition.
    fn start_service(&self, name: &str, launch: Option<&LaunchSpec>) -> Result<()>;
    fn stop_service(&self, name: &str) -> Result<()>;
    /// Moves service `name` off outdated unit names before it is stopped (nix: stops and
    /// removes this project's legacy-named and renamed-slug units). `start_service` does
    /// this itself, just before it starts the unit. Others do nothing.
    fn migrate_service(&self, _name: &str) -> Result<()> {
        Ok(())
    }
    /// Whether service `name` has any unit under an outdated name (legacy or a renamed
    /// project slug) that `migrate_service` would migrate. Read-only.
    fn needs_service_migration(&self, _name: &str) -> bool {
        false
    }
    /// Whether this project still runs service `name` under its legacy, project-less unit
    /// name (`sh.devy.<name>`). Renamed-slug units don't count. Read-only.
    fn uses_legacy_service_name(&self, _name: &str) -> bool {
        false
    }
    /// Returns the exact version string currently installed, e.g. "20.11.0" or "7.2.3".
    fn resolved_version(&self, dep: &Dependency) -> Result<Option<String>>;

    /// Returns the directory where service config files should be written, if supported.
    /// Returns `None` if the platform does not support writing config for the given service.
    fn service_config_dir(&self, _service: &str) -> Option<PathBuf> {
        None
    }

    /// Validates dependency configuration before install (e.g. tap allowlist).
    /// Called by `devy check` so issues surface without triggering any installs.
    fn validate_config(&self, _dep: &Dependency) -> Result<()> {
        Ok(())
    }

    /// Returns paths to prepend to PATH for this package manager's installed binaries.
    /// Used by `devy up` to wire the environment so project-local binaries are found first.
    fn path_prepends(&self, _project_root: &std::path::Path) -> Vec<String> {
        vec![]
    }

    /// Where the logs of service `name` (the backend service name) can be read: its last
    /// `lines` lines and, with `follow`, new output as it is written.
    fn log_source(&self, _name: &str, _lines: u32, _follow: bool) -> Result<LogSource> {
        Ok(LogSource::Unsupported(format!(
            "Logs are not available for {}-managed services",
            self.name()
        )))
    }

    /// URL for manual installation instructions. Empty string means no URL is shown.
    fn install_url(&self) -> &str {
        ""
    }

    fn ensure_available(&self, allow_bootstrap: bool) -> Result<()> {
        if !self.is_available() {
            if allow_bootstrap {
                self.bootstrap()
            } else {
                let hint = match self.install_url() {
                    "" => String::new(),
                    url => format!("\n             Install manually: {url}"),
                };
                anyhow::bail!(
                    "{} is not installed. Re-run with --bootstrap to install automatically.{}",
                    self.name(),
                    hint
                )
            }
        } else {
            Ok(())
        }
    }
}

/// devy.yml's `name`, or `project` when it has none: with the project root, it makes the
/// project slug in nix unit names and docker container names.
pub(crate) fn project_name(config: &DevyConfig) -> &str {
    config.name.as_deref().unwrap_or("project")
}

/// Detect or select the active package manager.
///
/// The choice comes from `package_manager:` in `devy.yml`. Unknown values are rejected
/// by serde at parse time; this function only handles the valid enum variants. The
/// warning about defaulting to nix is skipped when no dependency uses the package
/// manager (every dependency is a docker-managed service).
///
/// `project_root` is used by the Nix backend to scope the profile to the
/// project directory rather than the shell's current working directory.
pub fn detect(
    config: &DevyConfig,
    project_root: &std::path::Path,
) -> Result<Box<dyn PackageManager>> {
    match config.package_manager {
        PackageManagerChoice::Nix => {
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            anyhow::bail!("package_manager: nix is not supported on Windows");
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            return Ok(Box::new(NixPackageManager::for_project(
                project_root,
                project_name(config),
            )));
        }
        PackageManagerChoice::Brew => {
            #[cfg(not(target_os = "macos"))]
            anyhow::bail!("package_manager: brew is only available on macOS");
            #[cfg(target_os = "macos")]
            return Ok(Box::new(Homebrew));
        }
        PackageManagerChoice::Apt => {
            #[cfg(not(target_os = "linux"))]
            anyhow::bail!("package_manager: apt is only available on Linux");
            #[cfg(target_os = "linux")]
            return Ok(Box::new(Apt::new()));
        }
        PackageManagerChoice::Auto =>
        {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            if !config.docker_only() {
                crate::output::warn(
                    "No package_manager set in devy.yml — defaulting to nix. \
                 Add `package_manager: brew` (macOS) or `package_manager: apt` (Linux) \
                 to keep using your system package manager.",
                );
            }
        }
    }

    // Nix is always used on macOS and Linux: it installs packages into a
    // project-local profile (.devy/nix-profile) and will be bootstrapped by
    // `devy up` if it is not yet installed. Users who explicitly want their
    // system package manager should set `package_manager: brew` or
    // `package_manager: apt` in devy.yml.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    return Ok(Box::new(NixPackageManager::for_project(
        project_root,
        project_name(config),
    )));

    #[cfg(target_os = "windows")]
    return Ok(Box::new(WinGet::new()));

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    anyhow::bail!("No supported package manager for this operating system")
}

/// A configurable PackageManager implementation for use in unit tests.
/// Available in all `#[cfg(test)]` contexts via `crate::package_manager::MockPackageManager`.
#[cfg(test)]
pub struct MockPackageManager {
    pub name: &'static str,
    pub installed: bool,
    pub service_running: bool,
    pub install_fails: bool,
    pub start_service_fails: bool,
    pub stop_service_fails: bool,
    pub is_running_fails: bool,
    pub config_dir: Option<std::path::PathBuf>,
    /// When set, `is_package_installed` returns true only when `dep.name == installed_pkg`.
    pub installed_pkg: Option<&'static str>,
    /// Tracks which service names were passed to `start_service`.
    pub started_services: std::cell::RefCell<Vec<String>>,
    /// Tracks the launch spec passed with each `start_service` call.
    pub started_launches: std::cell::RefCell<Vec<Option<LaunchSpec>>>,
    /// Tracks which service names were passed to `stop_service`.
    pub stopped_services: std::cell::RefCell<Vec<String>>,
    /// Tracks which service names were passed to `migrate_service`.
    pub migrated_services: std::cell::RefCell<Vec<String>>,
    /// Service names `uses_legacy_service_name` and `needs_service_migration` report as
    /// legacy.
    pub legacy_services: Vec<&'static str>,
    /// Tracks every package name passed to `install_package` (in dep.name form).
    pub installed_packages: std::cell::RefCell<Vec<String>>,
    /// The subset of `installed_packages` installed with `allow_unfree` set.
    pub unfree_packages: std::cell::RefCell<Vec<String>>,
    /// The subset of `installed_packages` installed with `allow_insecure` set.
    pub insecure_packages: std::cell::RefCell<Vec<String>>,
    /// When set, `resolved_version` returns this value instead of Ok(None) (only for
    /// `installed_pkg` when that is set).
    pub version: Option<String>,
    /// Tracks every package name passed to `resolved_version`.
    pub version_queries: std::cell::RefCell<Vec<String>>,
    /// When true, `validate_config` returns an error.
    pub validate_config_fails: bool,
    /// Paths returned by `path_prepends`. Defaults to empty.
    pub path_prepends_result: Vec<String>,
    /// When true, `is_available` returns false, so `ensure_available` fails without
    /// `--bootstrap`.
    pub unavailable: bool,
    /// Returned by `log_source`; `None` keeps the trait default.
    pub log_source_result: Option<LogSource>,
    /// Tracks every `(name, lines, follow)` passed to `log_source`.
    pub log_queries: std::cell::RefCell<Vec<(String, u32, bool)>>,
}

#[cfg(test)]
impl Default for MockPackageManager {
    fn default() -> Self {
        Self {
            name: "mock",
            installed: false,
            service_running: false,
            install_fails: false,
            start_service_fails: false,
            stop_service_fails: false,
            is_running_fails: false,
            config_dir: None,
            installed_pkg: None,
            started_services: std::cell::RefCell::new(Vec::new()),
            started_launches: std::cell::RefCell::new(Vec::new()),
            stopped_services: std::cell::RefCell::new(Vec::new()),
            migrated_services: std::cell::RefCell::new(Vec::new()),
            legacy_services: Vec::new(),
            installed_packages: std::cell::RefCell::new(Vec::new()),
            unfree_packages: std::cell::RefCell::new(Vec::new()),
            insecure_packages: std::cell::RefCell::new(Vec::new()),
            version: None,
            version_queries: std::cell::RefCell::new(Vec::new()),
            validate_config_fails: false,
            path_prepends_result: Vec::new(),
            unavailable: false,
            log_source_result: None,
            log_queries: std::cell::RefCell::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl PackageManager for MockPackageManager {
    fn name(&self) -> &str {
        self.name
    }
    fn is_available(&self) -> bool {
        !self.unavailable
    }
    fn bootstrap(&self) -> Result<()> {
        Ok(())
    }
    fn is_package_installed(&self, dep: &Dependency) -> Result<bool> {
        if let Some(pkg) = self.installed_pkg {
            Ok(dep.name == pkg)
        } else {
            Ok(self.installed)
        }
    }
    fn install_package(&self, dep: &Dependency) -> Result<()> {
        self.installed_packages.borrow_mut().push(dep.name.clone());
        if dep.allow_unfree {
            self.unfree_packages.borrow_mut().push(dep.name.clone());
        }
        if dep.allow_insecure {
            self.insecure_packages.borrow_mut().push(dep.name.clone());
        }
        if self.install_fails {
            anyhow::bail!("mock install failure")
        } else {
            Ok(())
        }
    }
    fn is_service_running(&self, name: &str) -> Result<bool> {
        if self.is_running_fails {
            anyhow::bail!("mock is_service_running failure")
        }
        // Return false if this service was already stopped (simulates real stop behaviour).
        if self.stopped_services.borrow().contains(&name.to_string()) {
            return Ok(false);
        }
        Ok(self.service_running)
    }
    fn start_service(&self, name: &str, launch: Option<&LaunchSpec>) -> Result<()> {
        self.started_services.borrow_mut().push(name.to_string());
        self.started_launches.borrow_mut().push(launch.cloned());
        if self.start_service_fails {
            anyhow::bail!("mock start_service failure")
        } else {
            Ok(())
        }
    }
    fn stop_service(&self, name: &str) -> Result<()> {
        self.stopped_services.borrow_mut().push(name.to_string());
        if self.stop_service_fails {
            anyhow::bail!("mock stop_service failure")
        } else {
            Ok(())
        }
    }
    fn migrate_service(&self, name: &str) -> Result<()> {
        self.migrated_services.borrow_mut().push(name.to_string());
        Ok(())
    }
    fn needs_service_migration(&self, name: &str) -> bool {
        self.legacy_services.contains(&name)
    }
    fn uses_legacy_service_name(&self, name: &str) -> bool {
        self.legacy_services.contains(&name)
    }
    fn resolved_version(&self, dep: &Dependency) -> Result<Option<String>> {
        self.version_queries.borrow_mut().push(dep.name.clone());
        if self.installed_pkg.is_some_and(|pkg| dep.name != pkg) {
            return Ok(None);
        }
        Ok(self.version.clone())
    }
    fn service_config_dir(&self, _: &str) -> Option<std::path::PathBuf> {
        self.config_dir.clone()
    }
    fn validate_config(&self, _dep: &Dependency) -> Result<()> {
        if self.validate_config_fails {
            anyhow::bail!("mock validate_config failure")
        } else {
            Ok(())
        }
    }
    fn path_prepends(&self, _project_root: &std::path::Path) -> Vec<String> {
        self.path_prepends_result.clone()
    }
    fn log_source(&self, name: &str, lines: u32, follow: bool) -> Result<LogSource> {
        self.log_queries
            .borrow_mut()
            .push((name.to_string(), lines, follow));
        match &self.log_source_result {
            Some(source) => Ok(source.clone()),
            None => Ok(LogSource::Unsupported(format!(
                "Logs are not available for {}-managed services",
                self.name
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Dependency;
    use serde_norway as yaml;

    struct AvailablePm;
    impl PackageManager for AvailablePm {
        fn name(&self) -> &str {
            "available"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn bootstrap(&self) -> Result<()> {
            panic!("bootstrap should not be called")
        }
        fn is_package_installed(&self, _: &Dependency) -> Result<bool> {
            Ok(false)
        }
        fn install_package(&self, _: &Dependency) -> Result<()> {
            Ok(())
        }
        fn is_service_running(&self, _: &str) -> Result<bool> {
            Ok(false)
        }
        fn start_service(&self, _: &str, _: Option<&LaunchSpec>) -> Result<()> {
            Ok(())
        }
        fn stop_service(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn resolved_version(&self, _: &Dependency) -> Result<Option<String>> {
            Ok(None)
        }
    }

    struct UnavailablePm {
        bootstrap_called: std::cell::Cell<bool>,
    }
    impl UnavailablePm {
        fn new() -> Self {
            Self {
                bootstrap_called: std::cell::Cell::new(false),
            }
        }
    }
    impl PackageManager for UnavailablePm {
        fn name(&self) -> &str {
            "unavailable"
        }
        fn is_available(&self) -> bool {
            false
        }
        fn bootstrap(&self) -> Result<()> {
            self.bootstrap_called.set(true);
            Ok(())
        }
        fn is_package_installed(&self, _: &Dependency) -> Result<bool> {
            Ok(false)
        }
        fn install_package(&self, _: &Dependency) -> Result<()> {
            Ok(())
        }
        fn is_service_running(&self, _: &str) -> Result<bool> {
            Ok(false)
        }
        fn start_service(&self, _: &str, _: Option<&LaunchSpec>) -> Result<()> {
            Ok(())
        }
        fn stop_service(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn resolved_version(&self, _: &Dependency) -> Result<Option<String>> {
            Ok(None)
        }
    }

    #[test]
    fn detect_auto_is_ok() {
        let root = std::path::Path::new("/tmp");
        let config = crate::test_support::make_config(&[], Default::default());
        assert!(
            detect(&config, root).is_ok(),
            "PackageManagerChoice::Auto must succeed on the current platform"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn detect_auto_warns_only_when_a_dependency_uses_the_package_manager() {
        let root = std::path::Path::new("/tmp");
        let warnings = |yaml: &str| {
            let config: DevyConfig = yaml::from_str(yaml).unwrap();
            crate::output::with_warn_messages(|| {
                detect(&config, root).unwrap();
            })
        };
        assert!(
            warnings("service_manager: docker\ndependencies:\n  - redis\n").is_empty(),
            "a docker-only project never uses nix"
        );
        assert_eq!(
            warnings("service_manager: docker\ndependencies:\n  - redis\n  - jq\n").len(),
            1
        );
        assert_eq!(warnings("dependencies:\n  - redis\n").len(), 1);
    }

    #[test]
    fn ensure_available_skips_bootstrap_when_already_available() {
        let pm = AvailablePm;
        assert!(pm.ensure_available(false).is_ok());
    }

    #[test]
    fn ensure_available_calls_bootstrap_when_not_available_and_allowed() {
        let pm = UnavailablePm::new();
        pm.ensure_available(true).unwrap();
        assert!(pm.bootstrap_called.get());
    }

    #[test]
    fn ensure_available_returns_err_when_not_available_and_not_allowed() {
        let pm = UnavailablePm::new();
        let err = pm.ensure_available(false).unwrap_err();
        assert!(
            err.to_string().contains("--bootstrap"),
            "error must mention --bootstrap"
        );
    }

    #[test]
    fn default_service_config_dir_returns_none() {
        assert!(AvailablePm.service_config_dir("mysql").is_none());
        assert!(AvailablePm.service_config_dir("redis").is_none());
    }

    #[test]
    fn default_path_prepends_returns_empty() {
        // Any PM that doesn't override path_prepends must return an empty vec.
        assert!(
            AvailablePm
                .path_prepends(std::path::Path::new("/tmp"))
                .is_empty()
        );
    }

    #[test]
    fn mock_path_prepends_result_is_returned() {
        let pm = MockPackageManager {
            path_prepends_result: vec!["/custom/bin".into()],
            ..Default::default()
        };
        let result = pm.path_prepends(std::path::Path::new("/tmp"));
        assert_eq!(result, vec!["/custom/bin"]);
    }

    #[test]
    fn default_log_source_is_unsupported() {
        assert_eq!(
            AvailablePm.log_source("redis", 100, false).unwrap(),
            LogSource::Unsupported("Logs are not available for available-managed services".into())
        );
    }

    #[test]
    fn journal_source_builds_argv() {
        let LogSource::Command(cmd) = LogSource::journal("redis-server", false, 20, true) else {
            panic!("expected a command");
        };
        assert_eq!(cmd.program, "journalctl");
        assert_eq!(
            cmd.args,
            [
                "-u",
                "redis-server",
                "-n",
                "20",
                "--no-pager",
                "-o",
                "cat",
                "-f"
            ]
        );
        assert_eq!(
            cmd.kind,
            LogCommandKind::SystemJournal {
                unit: "redis-server".into()
            }
        );
    }

    #[test]
    fn system_tool_skips_project_paths_and_never_returns_a_bare_name() {
        let sys = crate::test_support::tmp_dir();
        std::fs::write(sys.join("devy-tool"), "").unwrap();
        let sys_dir = sys.to_str().unwrap();
        // Absolute names are kept; a PATH hit wins; otherwise a system dir; else none.
        #[cfg(unix)]
        assert_eq!(
            system_tool_in("/bin/launchctl", None, &[]),
            Some(PathBuf::from("/bin/launchctl"))
        );
        assert_eq!(
            system_tool_in("devy-tool", Some("/x/devy-tool".into()), &[sys_dir]),
            Some(PathBuf::from("/x/devy-tool"))
        );
        assert_eq!(
            system_tool_in("devy-tool", None, &[sys_dir]),
            Some(sys.join("devy-tool"))
        );
        assert_eq!(system_tool_in("devy-missing-tool", None, &[sys_dir]), None);
        let err = require_system_tool("devy-no-such-tool-xyz")
            .unwrap_err()
            .to_string();
        assert!(err.contains("`devy-no-such-tool-xyz`"), "{err}");
        assert_eq!(
            not_found_message("t", &["/usr/bin", "/bin"]),
            "Failed to run `t`: it was not found on PATH (outside the project) or in /usr/bin, /bin"
        );
        assert_eq!(
            not_found_message("t", &[]),
            "Failed to run `t`: it was not found on PATH (outside the project)"
        );
    }
}
