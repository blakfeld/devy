use anyhow::{Context, Result, bail};
use std::cmp::Reverse;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use which::which;

use super::PackageManager;
use crate::config::Dependency;

pub struct Apt;

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

    fn run_apt_interactive(&self, args: &[&str]) -> Result<()> {
        let status = Command::new("sudo")
            .arg("apt-get")
            .args(args)
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
        which("apt-get").is_ok()
    }

    fn bootstrap(&self) -> Result<()> {
        bail!("apt-get is not available; please ensure Ubuntu/Debian is properly installed")
    }

    fn is_package_installed(&self, dep: &Dependency) -> Result<bool> {
        let output = Command::new("dpkg-query")
            .args(["-W", "-f=${Status}|${Version}", &dep.name])
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
        // apt version pinning requires exact Debian version strings; the devy version field
        // is passed through as-is. Partial versions (e.g. "20") may not resolve — users
        // relying on PPAs or NodeSource repos should omit the version field and rely on
        // devy.lock to pin the installed version across machines.
        let pkg_spec = match &dep.version {
            Some(ver) => format!("{}={}", dep.name, ver),
            None => dep.name.clone(),
        };
        self.run_apt_interactive(&["-y", "install", &pkg_spec])
    }

    fn is_service_running(&self, name: &str) -> Result<bool> {
        let output = Command::new("systemctl")
            .args(["is-active", name])
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
        let status = Command::new("sudo")
            .args(["systemctl", "start", name])
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
        let status = Command::new("sudo")
            .args(["systemctl", "stop", name])
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
        let output = Command::new("dpkg-query")
            .args(["-W", "-f=${Version}", &dep.name])
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
