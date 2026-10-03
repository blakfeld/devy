use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use which::which;

use super::{LogSource, PackageManager};
use crate::config::Dependency;

pub struct WinGet;

/// Parses the winget list output to find the version of a specific package ID.
/// The output format is: "Name  Id  Version  Available"
/// Returns the version string (the token immediately after the id in the matching line).
pub(crate) fn parse_winget_version(stdout: &str, name: &str) -> Option<String> {
    for line in stdout.lines() {
        if line.contains(name) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            for (i, &part) in parts.iter().enumerate() {
                if part == name {
                    return parts.get(i + 1).map(|s| s.to_string());
                }
            }
        }
    }
    None
}

impl WinGet {
    pub fn new() -> Self {
        Self
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new("winget")
            .args(args)
            .output()
            .with_context(|| format!("Failed to run: winget {}", args.join(" ")))
    }

    fn run_interactive(&self, args: &[&str]) -> Result<()> {
        let status = Command::new("winget")
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("Failed to run: winget {}", args.join(" ")))?;
        if !status.success() {
            bail!(
                "`winget {}` failed — check the output above for details",
                args.join(" ")
            );
        }
        Ok(())
    }
}

impl PackageManager for WinGet {
    fn name(&self) -> &str {
        "winget"
    }

    fn is_available(&self) -> bool {
        which("winget").is_ok()
    }

    fn bootstrap(&self) -> Result<()> {
        bail!(
            "winget is not available. Install App Installer from the Microsoft Store \
             or update to a recent version of Windows 10/11."
        )
    }

    fn is_package_installed(&self, dep: &Dependency) -> Result<bool> {
        let output = self.run(&["list", "--id", &dep.name, "--exact"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains(dep.name.as_str()))
    }

    fn install_package(&self, dep: &Dependency) -> Result<()> {
        let mut args = vec![
            "install",
            "--id",
            dep.name.as_str(),
            "--exact",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ];
        let version = dep.version.clone();
        if let Some(ref ver) = version {
            args.push("--version");
            args.push(ver.as_str());
        }
        self.run_interactive(&args)
    }

    fn is_service_running(&self, name: &str) -> Result<bool> {
        let output = Command::new("sc")
            .args(["query", name])
            .output()
            .with_context(|| format!("Failed to query Windows service: {name}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains("RUNNING"))
    }

    fn start_service(
        &self,
        name: &str,
        _launch: Option<&crate::modules::LaunchSpec>,
    ) -> Result<()> {
        let status = Command::new("net")
            .args(["start", name])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("Failed to start Windows service: {name}"))?;
        if !status.success() {
            bail!("`net start {name}` failed — check the output above for details");
        }
        Ok(())
    }

    fn stop_service(&self, name: &str) -> Result<()> {
        let status = Command::new("net")
            .args(["stop", name])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("Failed to stop Windows service: {name}"))?;
        if !status.success() {
            bail!("`net stop {name}` failed — check the output above for details");
        }
        Ok(())
    }

    fn resolved_version(&self, dep: &Dependency) -> Result<Option<String>> {
        let output = self.run(&["list", "--id", &dep.name, "--exact"])?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_winget_version(&stdout, dep.name.as_str()))
    }

    fn service_config_dir(&self, _service: &str) -> Option<PathBuf> {
        None
    }

    fn log_source(&self, _name: &str, _lines: u32, _follow: bool) -> Result<LogSource> {
        Ok(LogSource::Unsupported(UNSUPPORTED_LOGS.into()))
    }
}

const UNSUPPORTED_LOGS: &str = "Logs are not available for winget-managed services — check Windows Event Viewer or the service's own log directory";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winget_logs_are_unsupported() {
        assert_eq!(
            WinGet::new().log_source("mysql", 100, false).unwrap(),
            LogSource::Unsupported(
                "Logs are not available for winget-managed services — check Windows Event Viewer or the service's own log directory".into()
            )
        );
    }

    // ── name ──────────────────────────────────────────────────────────────────

    #[test]
    fn winget_name_is_winget() {
        assert_eq!(WinGet::new().name(), "winget");
    }

    // ── bootstrap ─────────────────────────────────────────────────────────────

    #[test]
    fn winget_bootstrap_always_bails() {
        assert!(WinGet::new().bootstrap().is_err());
    }

    // ── parse_winget_version ──────────────────────────────────────────────────

    #[test]
    fn parse_winget_version_finds_version_after_id() {
        let stdout = "Name              Id              Version   Available\r\n\
                      Git for Windows   Git.Git         2.43.0    2.44.0\r\n";
        assert_eq!(
            parse_winget_version(stdout, "Git.Git"),
            Some("2.43.0".into())
        );
    }

    #[test]
    fn parse_winget_version_returns_none_when_id_not_found() {
        let stdout = "Name  Id  Version\r\nSomePkg  Other.Id  1.0\r\n";
        assert!(parse_winget_version(stdout, "Missing.Id").is_none());
    }

    #[test]
    fn parse_winget_version_returns_none_when_id_has_no_next_token() {
        let stdout = "Name  Dangling.Id\r\n";
        assert!(parse_winget_version(stdout, "Dangling.Id").is_none());
    }

    #[test]
    fn parse_winget_version_handles_exact_id_match() {
        // The loop matches `part == name` (exact token), not just contains.
        // "Foo.Bar.Baz" should not match "Foo.Bar".
        let stdout = "Foo.Bar.Baz  1.0\r\n";
        assert!(parse_winget_version(stdout, "Foo.Bar").is_none());
    }

    #[test]
    fn parse_winget_version_returns_token_after_id() {
        let stdout = "row  MyApp.ID  2.0.1  available\r\n";
        assert_eq!(
            parse_winget_version(stdout, "MyApp.ID"),
            Some("2.0.1".into())
        );
    }

    // ── service_config_dir ────────────────────────────────────────────────────

    #[test]
    fn winget_service_config_dir_always_none() {
        assert!(WinGet::new().service_config_dir("mysql").is_none());
        assert!(WinGet::new().service_config_dir("redis").is_none());
        assert!(WinGet::new().service_config_dir("anything").is_none());
    }
}
