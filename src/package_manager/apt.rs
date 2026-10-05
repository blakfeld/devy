use anyhow::{Context, Result, bail};
use std::cmp::Reverse;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{LogSource, PackageManager};
use crate::config::Dependency;

pub struct Apt;

/// Absolute path of `apt-get`. Using a fixed path (rather than a PATH lookup) means a
/// project-local `apt-get` shim can never be run under sudo.
const APT_GET: &str = "/usr/bin/apt-get";
/// Absolute path of `sudo`, for the same reason.
const SUDO: &str = "/usr/bin/sudo";

/// `systemctl` for the privileged `sudo systemctl start/stop`: a fixed root-owned path,
/// never a PATH lookup (which could find a user-writable `~/bin/systemctl` and run it as
/// root). Debian and Ubuntu ship it in `/usr/bin` (merged /usr) or `/bin`.
fn privileged_systemctl() -> &'static str {
    privileged_systemctl_from(|p| std::path::Path::new(p).is_file())
}

fn privileged_systemctl_from(exists: impl Fn(&str) -> bool) -> &'static str {
    ["/usr/bin/systemctl", "/bin/systemctl"]
        .into_iter()
        .find(|p| exists(p))
        .unwrap_or("/usr/bin/systemctl")
}

/// `dpkg-query` found on PATH outside the project (so a project-local shim can't answer
/// "is it installed?"), or its standard location.
fn dpkg_query() -> PathBuf {
    crate::fs_safe::which_outside_project("dpkg-query")
        .unwrap_or_else(|| PathBuf::from("/usr/bin/dpkg-query"))
}

/// Returns true for a Debian package name, optionally with an `:arch` qualifier
/// (`[a-z0-9][a-z0-9.+-]*`, e.g. `libssl3:arm64`). This excludes paths and `.deb` files,
/// which apt-get would otherwise install from disk — as root — even after `--`.
fn is_debian_package_name(name: &str) -> bool {
    let (pkg, arch) = match name.split_once(':') {
        Some((pkg, arch)) => (pkg, Some(arch)),
        None => (name, None),
    };
    let pkg_ok = pkg
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && pkg
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '+' | '-'));
    let arch_ok = arch
        .is_none_or(|a| !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    // A trailing `-` tells `apt-get install` to *remove* the package, so it is refused;
    // a trailing `+` means install and must keep working (`g++`). Debian names are at
    // least two characters long.
    pkg_ok && arch_ok && pkg.len() >= 2 && !pkg.ends_with('-') && !pkg.ends_with(".deb")
}

/// Returns true for a Debian version string (`[A-Za-z0-9.+~:-]`, starting with a digit).
/// A trailing `-` is refused: apt-get reads it on the whole `name=version` argument as
/// "remove", and a Debian revision after the last hyphen is never empty anyway.
fn is_debian_version(version: &str) -> bool {
    !version.ends_with('-')
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '~' | ':' | '-'))
}

/// Builds the `apt-get` argv (excluding the program) used to install `dep`.
/// Package specs follow `--` so a value can never be parsed as an option, and the name
/// and version are checked against Debian's character sets so they can never name a
/// local file.
pub(crate) fn install_args(dep: &Dependency) -> Result<Vec<String>> {
    if !is_debian_package_name(&dep.name) {
        bail!("invalid apt package name '{}'", dep.name);
    }
    if let Some(ver) = &dep.version
        && !is_debian_version(ver)
    {
        bail!("{}: invalid apt version '{}'", dep.name, ver);
    }
    // apt version pinning requires exact Debian version strings; the devy version field
    // is passed through as-is. Partial versions (e.g. "20") may not resolve — users
    // relying on PPAs or NodeSource repos should omit the version field and rely on
    // devy.lock to pin the installed version across machines.
    let pkg_spec = match &dep.version {
        Some(ver) => format!("{}={}", dep.name, ver),
        None => dep.name.clone(),
    };
    Ok(vec!["-y".into(), "install".into(), "--".into(), pkg_spec])
}

/// Builds the full privileged argv (`/usr/bin/sudo /usr/bin/apt-get <args>`).
pub(crate) fn sudo_apt_get_argv(args: &[String]) -> Vec<String> {
    let mut argv = vec![SUDO.to_string(), APT_GET.to_string()];
    argv.extend(args.iter().cloned());
    argv
}

/// Builds the `dpkg-query -W` argv (excluding the program) with the given format.
pub(crate) fn dpkg_query_args(format: &str, name: &str) -> Vec<String> {
    vec![
        "-W".into(),
        format!("-f={format}"),
        "--".into(),
        name.to_string(),
    ]
}

/// Returns true when systemctl reports a service as "active".
pub(crate) fn parse_systemctl_status(stdout: &str) -> bool {
    stdout.trim() == "active"
}

/// Parses dpkg-query -W version output into an optional version string.
pub(crate) fn parse_dpkg_version(status_success: bool, stdout: &str) -> Option<String> {
    if !status_success {
        return None;
    }
    let ver = stdout.trim().to_string();
    if ver.is_empty() { None } else { Some(ver) }
}

/// Returns true when the installed version satisfies the required version.
/// Uses exact-match semantics matching apt's `name=version` install spec.
pub(crate) fn installed_version_matches(installed: Option<&str>, required: &str) -> bool {
    installed.map(|v| v == required).unwrap_or(false)
}

fn service_config_dir_impl(service: &str, pg_base: &std::path::Path) -> Option<PathBuf> {
    match service {
        "mysql" | "mariadb" => Some(PathBuf::from("/etc/mysql/conf.d")),
        // Their configs live in per-service directories under /etc (see
        // `modules::loopback`).
        "kafka" | "zookeeper" | "rabbitmq" => Some(PathBuf::from("/etc")),
        "postgresql" | "postgres" => {
            let mut versions: Vec<(u32, PathBuf)> = std::fs::read_dir(pg_base)
                .ok()?
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let ver: u32 = e.file_name().to_str()?.parse().ok()?;
                    Some((ver, e.path()))
                })
                .collect();
            versions.sort_by_key(|(v, _)| Reverse(*v));
            let (_, version_dir) = versions.into_iter().next()?;
            Some(version_dir.join("main").join("conf.d"))
        }
        _ => None,
    }
}

impl Apt {
    pub fn new() -> Self {
        Self
    }

    fn run_apt_interactive(&self, args: &[String]) -> Result<()> {
        let argv = sudo_apt_get_argv(args);
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("Failed to run: sudo apt-get {}", args.join(" ")))?;
        if !status.success() {
            bail!(
                "`sudo apt-get {}` failed — check the output above for details",
                args.join(" ")
            );
        }
        Ok(())
    }
}

impl Default for Apt {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageManager for Apt {
    fn name(&self) -> &str {
        "apt"
    }

    fn is_available(&self) -> bool {
        std::path::Path::new(APT_GET).is_file()
    }

    fn bootstrap(&self) -> Result<()> {
        bail!("apt-get is not available; please ensure Ubuntu/Debian is properly installed")
    }

    fn is_package_installed(&self, dep: &Dependency) -> Result<bool> {
        let output = Command::new(dpkg_query())
            .args(dpkg_query_args("${Status}|${Version}", &dep.name))
            .output()
            .with_context(|| format!("Failed to query dpkg for {}", dep.name))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut parts = stdout.splitn(2, '|');
        let status = parts.next().unwrap_or("").trim();
        if status != "install ok installed" {
            return Ok(false);
        }
        if let Some(ver) = &dep.version {
            let installed_ver = parts.next().unwrap_or("").trim();
            return Ok(installed_version_matches(Some(installed_ver), ver));
        }
        Ok(true)
    }

    fn install_package(&self, dep: &Dependency) -> Result<()> {
        self.run_apt_interactive(&install_args(dep)?)
    }

    fn is_service_running(&self, name: &str) -> Result<bool> {
        let output = Command::new(super::require_system_tool("systemctl")?)
            .args(["is-active", "--", name])
            .output()
            .with_context(|| format!("Failed to check systemctl status for {name}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_systemctl_status(&stdout))
    }

    fn start_service(
        &self,
        name: &str,
        _launch: Option<&crate::modules::LaunchSpec>,
    ) -> Result<()> {
        let status = Command::new(SUDO)
            .arg(privileged_systemctl())
            .args(["start", "--", name])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("Failed to start service: {name}"))?;
        if !status.success() {
            bail!("`systemctl start {name}` failed — check the output above for details");
        }
        Ok(())
    }

    fn stop_service(&self, name: &str) -> Result<()> {
        let status = Command::new(SUDO)
            .arg(privileged_systemctl())
            .args(["stop", "--", name])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("Failed to stop service: {name}"))?;
        if !status.success() {
            bail!("`systemctl stop {name}` failed — check the output above for details");
        }
        Ok(())
    }

    fn resolved_version(&self, dep: &Dependency) -> Result<Option<String>> {
        let output = Command::new(dpkg_query())
            .args(dpkg_query_args("${Version}", &dep.name))
            .output()
            .with_context(|| format!("Failed to query version for {}", dep.name))?;
        Ok(parse_dpkg_version(
            output.status.success(),
            &String::from_utf8_lossy(&output.stdout),
        ))
    }

    fn service_config_dir(&self, service: &str) -> Option<PathBuf> {
        service_config_dir_impl(service, std::path::Path::new("/etc/postgresql"))
    }

    /// The system journal, read as the current user: devy never uses sudo for logs.
    fn log_source(&self, name: &str, lines: u32, follow: bool) -> Result<LogSource> {
        Ok(LogSource::journal(name, false, lines, follow))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apt_logs_read_the_system_journal_without_sudo() {
        let LogSource::Command(cmd) = Apt::new().log_source("redis-server", 100, false).unwrap()
        else {
            panic!("expected a command");
        };
        assert_eq!(cmd.program, "journalctl");
        assert_eq!(
            cmd.args,
            ["-u", "redis-server", "-n", "100", "--no-pager", "-o", "cat"]
        );
    }

    #[test]
    fn apt_install_uses_absolute_sudo_and_separator() {
        // Scenario: apt receives a separator.
        let mut dep = Dependency::simple("redis-server");
        dep.version = Some("7.0.15-1".into());
        assert_eq!(
            sudo_apt_get_argv(&install_args(&dep).unwrap()),
            [
                "/usr/bin/sudo",
                "/usr/bin/apt-get",
                "-y",
                "install",
                "--",
                "redis-server=7.0.15-1"
            ]
        );
    }

    /// Scenario "Planted sudo": `<project_root>/bin/sudo` first on PATH is never run.
    #[cfg(unix)]
    #[test]
    fn planted_sudo_on_project_path_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_support::tmp_dir();
        let system = crate::test_support::tmp_dir();
        let project_bin = root.join("bin");
        let system_bin = system.join("bin");
        for (dir, tools) in [
            (&project_bin, &["sudo", "apt-get", "dpkg-query"][..]),
            (&system_bin, &["dpkg-query"][..]),
        ] {
            std::fs::create_dir(dir).unwrap();
            for tool in tools {
                let script = dir.join(tool);
                std::fs::write(&script, "#!/bin/sh\ntouch \"$0.ran\"\n").unwrap();
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        // The project's bin comes first on PATH, as an activated environment would put it.
        let path = std::env::join_paths([project_bin.as_path(), system_bin.as_path()]).unwrap();

        // The privileged argv names absolute programs, so PATH is never consulted: running
        // it with the hostile PATH could only ever reach /usr/bin/sudo.
        let argv = sudo_apt_get_argv(&install_args(&Dependency::simple("jq")).unwrap());
        assert_eq!(&argv[..2], [SUDO, APT_GET]);
        assert!(
            argv[..2]
                .iter()
                .all(|p| std::path::Path::new(p).is_absolute())
        );

        // The unprivileged lookup skips the project's shim and finds the system one.
        let found = crate::fs_safe::which_outside_project_in("dpkg-query", &path, Some(&root))
            .expect("system dpkg-query");
        assert_eq!(found, system_bin.join("dpkg-query"));
        // Through sh rather than exec'ing the just-written file, which can fail with
        // ETXTBSY while another test thread forks.
        let status = Command::new("/bin/sh").arg(&found).status().unwrap();
        assert!(status.success());
        assert!(system_bin.join("dpkg-query.ran").exists());
        for tool in ["sudo", "apt-get", "dpkg-query"] {
            assert!(
                !project_bin.join(format!("{tool}.ran")).exists(),
                "planted {tool} must never run"
            );
        }
    }

    #[test]
    fn apt_install_without_version_passes_bare_name() {
        let dep = Dependency::simple("redis-server");
        assert_eq!(
            install_args(&dep).unwrap(),
            ["-y", "install", "--", "redis-server"]
        );
    }

    #[test]
    fn apt_install_accepts_arch_and_epoch() {
        let mut dep = Dependency::simple("libssl3:arm64");
        dep.version = Some("1:3.0.13-0ubuntu3~22.04+b1".into());
        assert_eq!(
            install_args(&dep).unwrap()[3],
            "libssl3:arm64=1:3.0.13-0ubuntu3~22.04+b1"
        );
        assert!(install_args(&Dependency::simple("g++")).is_ok());
    }

    #[test]
    fn apt_install_rejects_local_package_files() {
        // Scenario: Local package file rejected — sudo is never invoked.
        for name in [
            "./evil.deb",
            "evil.deb",
            "/tmp/x",
            "-oDPkg::Pre-Invoke=id",
            "Redis",
            "",
            "redis-server-",
            "postgresql-:amd64",
            "x",
        ] {
            assert!(
                install_args(&Dependency::simple(name)).is_err(),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn apt_install_rejects_path_like_versions() {
        for ver in ["../x.deb", "1.0/x", "1.0;id", "-1", "", "1-", "1.0-1-"] {
            let mut dep = Dependency::simple("redis");
            dep.version = Some(ver.into());
            assert!(install_args(&dep).is_err(), "{ver} must be rejected");
        }
    }

    #[test]
    fn dpkg_query_args_use_separator() {
        assert_eq!(
            dpkg_query_args("${Status}|${Version}", "redis-server"),
            ["-W", "-f=${Status}|${Version}", "--", "redis-server"]
        );
    }

    fn tmp_dir() -> crate::test_support::TempDir {
        crate::test_support::tmp_dir()
    }

    // ── name ──────────────────────────────────────────────────────────────────

    #[test]
    fn apt_name_is_apt() {
        assert_eq!(Apt::new().name(), "apt");
    }

    // ── bootstrap ─────────────────────────────────────────────────────────────

    #[test]
    fn apt_bootstrap_always_bails() {
        assert!(Apt::new().bootstrap().is_err());
    }

    // ── installed_version_matches ─────────────────────────────────────────────

    #[test]
    fn installed_version_matches_returns_true_on_exact_match() {
        assert!(installed_version_matches(
            Some("16.3.1-1ubuntu1"),
            "16.3.1-1ubuntu1"
        ));
        assert!(installed_version_matches(Some("2:8.0.36-1"), "2:8.0.36-1"));
    }

    #[test]
    fn installed_version_matches_returns_false_on_version_mismatch() {
        assert!(!installed_version_matches(Some("15.0"), "16.0"));
        assert!(!installed_version_matches(Some("16.0.0"), "16.0"));
    }

    #[test]
    fn installed_version_matches_returns_false_when_not_installed() {
        assert!(!installed_version_matches(None, "16.0"));
    }

    // ── parse_systemctl_status ────────────────────────────────────────────────

    #[test]
    fn parse_systemctl_status_active_returns_true() {
        assert!(parse_systemctl_status("active"));
        assert!(parse_systemctl_status("active\n"));
        assert!(parse_systemctl_status("  active  "));
    }

    #[test]
    fn parse_systemctl_status_inactive_returns_false() {
        assert!(!parse_systemctl_status("inactive"));
        assert!(!parse_systemctl_status("failed"));
        assert!(!parse_systemctl_status("activating"));
        assert!(!parse_systemctl_status(""));
    }

    // ── parse_dpkg_version ────────────────────────────────────────────────────

    #[test]
    fn parse_dpkg_version_returns_none_on_failure() {
        assert!(parse_dpkg_version(false, "1.2.3").is_none());
        assert!(parse_dpkg_version(false, "").is_none());
    }

    #[test]
    fn parse_dpkg_version_returns_none_on_empty_stdout() {
        assert!(parse_dpkg_version(true, "").is_none());
        assert!(parse_dpkg_version(true, "   ").is_none());
    }

    #[test]
    fn parse_dpkg_version_returns_version_on_success() {
        assert_eq!(parse_dpkg_version(true, "1.2.3"), Some("1.2.3".into()));
        assert_eq!(
            parse_dpkg_version(true, "2:20.04+dfsg1-0ubuntu3\n"),
            Some("2:20.04+dfsg1-0ubuntu3".into())
        );
    }

    // ── service_config_dir ────────────────────────────────────────────────────

    #[test]
    fn apt_service_config_dir_mysql_returns_etc_mysql() {
        let dir = Apt::new().service_config_dir("mysql");
        assert_eq!(dir, Some(PathBuf::from("/etc/mysql/conf.d")));
    }

    #[test]
    fn apt_service_config_dir_mariadb_returns_etc_mysql() {
        let dir = Apt::new().service_config_dir("mariadb");
        assert_eq!(dir, Some(PathBuf::from("/etc/mysql/conf.d")));
    }

    #[test]
    fn apt_service_config_dir_unknown_returns_none() {
        let dir = Apt::new().service_config_dir("redis");
        assert!(dir.is_none());
    }

    #[test]
    fn service_config_dir_impl_postgresql_returns_highest_version() {
        let base = tmp_dir();
        std::fs::create_dir_all(base.join("14").join("main").join("conf.d")).unwrap();
        std::fs::create_dir_all(base.join("13").join("main").join("conf.d")).unwrap();
        let result = service_config_dir_impl("postgresql", &base);
        assert!(result.is_some(), "Expected Some path for postgresql");
        let path = result.unwrap();
        assert!(
            path.to_str().unwrap().contains("14"),
            "Expected highest version (14) directory, got: {}",
            path.display()
        );
    }

    #[test]
    fn service_config_dir_impl_postgres_alias_returns_highest_version() {
        let base = tmp_dir();
        std::fs::create_dir_all(base.join("15").join("main").join("conf.d")).unwrap();
        let result = service_config_dir_impl("postgres", &base);
        assert!(result.is_some());
    }

    #[test]
    fn service_config_dir_impl_postgresql_returns_none_when_no_versions() {
        let base = tmp_dir();
        let result = service_config_dir_impl("postgresql", &base);
        assert!(result.is_none(), "Expected None when no version dirs exist");
    }

    #[test]
    fn service_config_dir_impl_unknown_service_returns_none() {
        let base = tmp_dir();
        assert!(service_config_dir_impl("redis", &base).is_none());
    }

    #[test]
    fn service_config_dir_impl_mysql_ignores_base() {
        let base = tmp_dir();
        assert_eq!(
            service_config_dir_impl("mysql", &base),
            Some(PathBuf::from("/etc/mysql/conf.d"))
        );
    }

    #[test]
    fn privileged_systemctl_is_a_fixed_absolute_path() {
        assert_eq!(privileged_systemctl_from(|_| true), "/usr/bin/systemctl");
        assert_eq!(
            privileged_systemctl_from(|p| p == "/bin/systemctl"),
            "/bin/systemctl"
        );
        assert_eq!(privileged_systemctl_from(|_| false), "/usr/bin/systemctl");
        assert!(privileged_systemctl().starts_with('/'));
    }
}
