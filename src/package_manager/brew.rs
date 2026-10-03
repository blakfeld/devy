use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use which::which;

use super::{LogSource, PackageManager};
use crate::config::Dependency;
use crate::output;

#[derive(Default)]
pub struct Homebrew;

/// Parses `brew services info --json` output and returns whether the service is running.
/// The JSON is an array; the first element has a `"running"` boolean field.
fn parse_brew_service_info_json(stdout: &[u8]) -> Result<bool> {
    let json: serde_json::Value =
        serde_json::from_slice(stdout).context("Failed to parse `brew services info` JSON")?;
    let arr = json
        .as_array()
        .context("`brew services info` output was not a JSON array")?;
    if arr.is_empty() {
        anyhow::bail!(
            "`brew services info` returned an empty array — service may not be managed by brew"
        );
    }
    Ok(arr[0]["running"].as_bool().unwrap_or(false))
}

/// Reads the log files of service `name` from `brew services info --json` output: its
/// `log_path` (stdout) and then its `error_log_path` (stderr), once each.
fn parse_brew_log_paths(name: &str, stdout: &[u8]) -> Result<LogSource> {
    let json: serde_json::Value =
        serde_json::from_slice(stdout).context("Failed to parse `brew services info` JSON")?;
    let info = json
        .as_array()
        .and_then(|a| a.first())
        .context("`brew services info` returned no service")?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for key in ["log_path", "error_log_path"] {
        if let Some(path) = info[key]
            .as_str()
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    }
    if paths.is_empty() {
        return Ok(LogSource::Unsupported(format!(
            "brew did not report a log path for {name} — check $(brew --prefix)/var/log"
        )));
    }
    Ok(LogSource::Files(paths))
}

/// Parses `brew list --versions` output and extracts the version (second whitespace token).
fn parse_brew_version(line: &str) -> Option<String> {
    line.split_whitespace().nth(1).map(String::from)
}

/// Returns the Homebrew formula name for a dependency.
///
/// Homebrew supports `name@major` (e.g. `node@20`) and `name@major.minor` (e.g. `python@3.14`)
/// formula selectors. Lock-injected resolved versions have 3+ components or a build suffix
/// (e.g. "20.11.0", "3.14.4_1") and are NOT valid formula names.
fn brew_formula_name(dep: &Dependency) -> String {
    match &dep.version {
        Some(v) if is_formula_pin(v) => format!("{}@{}", dep.name, v),
        _ => dep.name.clone(),
    }
}

/// Returns true when `v` looks like a user-specified version pin rather than a resolved version.
/// Formula pins have at most 2 dot-separated components and no build suffix (`_N`).
/// "20" → true, "3.14" → true, "8.0" → true
/// "20.11.0" → false (3 parts), "3.14.4_1" → false (underscore), "3.14.4" → false (3 parts)
fn is_formula_pin(v: &str) -> bool {
    !v.contains('_') && v.split('.').count() <= 2
}

impl Homebrew {
    fn brew_bin(&self) -> PathBuf {
        // Prefer the brew on PATH so brew_bin() and is_available() always agree.
        if let Ok(path) = which("brew") {
            return path;
        }
        if let Ok(prefix) = std::env::var("HOMEBREW_PREFIX")
            && !prefix.is_empty()
        {
            return PathBuf::from(prefix).join("bin").join("brew");
        }
        if cfg!(target_arch = "aarch64") {
            PathBuf::from("/opt/homebrew/bin/brew")
        } else {
            PathBuf::from("/usr/local/bin/brew")
        }
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new(self.brew_bin())
            .args(args)
            .output()
            .with_context(|| format!("Failed to run: brew {}", args.join(" ")))
    }

    fn run_interactive(&self, args: &[&str]) -> Result<()> {
        let status = Command::new(self.brew_bin())
            .args(args)
            .status()
            .with_context(|| format!("Failed to run: brew {}", args.join(" ")))?;
        if !status.success() {
            bail!(
                "`brew {}` failed — check the output above for details",
                args.join(" ")
            );
        }
        Ok(())
    }

    fn fetch_config_dir(&self, service: &str) -> Option<PathBuf> {
        let output = Command::new(self.brew_bin())
            .args(["--prefix", service])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Some(std::path::Path::new(&prefix).join("etc"))
    }
}

impl PackageManager for Homebrew {
    fn name(&self) -> &str {
        "brew"
    }

    fn install_url(&self) -> &str {
        "https://brew.sh"
    }

    fn is_available(&self) -> bool {
        which("brew").is_ok()
    }

    fn bootstrap(&self) -> Result<()> {
        output::step("Bootstrapping Homebrew (fetching install script from GitHub via bash)");
        output::warn("No hash verification is performed. See https://brew.sh for manual install.");
        let status = Command::new("sh")
            .arg("-c")
            .arg(concat!(
                "curl --connect-timeout 30 --max-time 300 -fsSL ",
                "https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh",
                " | bash"
            ))
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("Failed to run Homebrew install script")?;

        if !status.success() {
            bail!("Homebrew installation failed");
        }
        Ok(())
    }

    fn is_package_installed(&self, dep: &Dependency) -> Result<bool> {
        let pkg = brew_formula_name(dep);
        let output = self.run(&["list", "--versions", &pkg])?;
        Ok(output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty())
    }

    fn install_package(&self, dep: &Dependency) -> Result<()> {
        if let Some(tap) = &dep.tap {
            validate_tap(tap)?;
            self.run_interactive(&["tap", tap])?;
        }
        self.run_interactive(&["install", &brew_formula_name(dep)])
    }

    fn is_service_running(&self, name: &str) -> Result<bool> {
        let output = self.run(&["services", "info", "--json", name])?;
        if !output.status.success() {
            return Ok(false);
        }
        parse_brew_service_info_json(&output.stdout)
    }

    fn start_service(
        &self,
        name: &str,
        _launch: Option<&crate::modules::LaunchSpec>,
    ) -> Result<()> {
        self.run_interactive(&["services", "start", name])
    }

    fn stop_service(&self, name: &str) -> Result<()> {
        self.run_interactive(&["services", "stop", name])
    }

    fn resolved_version(&self, dep: &Dependency) -> Result<Option<String>> {
        let output = self.run(&["list", "--versions", &brew_formula_name(dep)])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.trim();
        // Output: "formula 1.2.3" or "formula@major 1.2.3_4" — take second token.
        Ok(parse_brew_version(line))
    }

    fn service_config_dir(&self, service: &str) -> Option<PathBuf> {
        self.fetch_config_dir(service)
    }

    fn log_source(&self, name: &str, _lines: u32, _follow: bool) -> Result<LogSource> {
        let output = self.run(&["services", "info", "--json", name])?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "`brew services info {name}` failed: {}",
                stderr.trim().lines().last().unwrap_or("no output")
            );
        }
        parse_brew_log_paths(name, &output.stdout)
    }

    fn validate_config(&self, dep: &Dependency) -> Result<()> {
        if let Some(ref tap) = dep.tap {
            validate_tap(tap).with_context(|| format!("{}: invalid tap", dep.name))?;
        }
        Ok(())
    }
}

/// Validates that a tap string has the form `org/repo` with no path components,
/// URL schemes, or shell-special characters. Prevents arbitrary GitHub repos from
/// being added via a malicious devy.yml tap field.
pub(crate) fn validate_tap(tap: &str) -> Result<()> {
    let parts: Vec<&str> = tap.split('/').collect();
    if parts.len() != 2 {
        bail!(
            "Invalid tap '{}': must be 'org/repo' (exactly one '/')",
            tap
        );
    }
    for part in &parts {
        if part.is_empty() {
            bail!("Invalid tap '{}': org and repo must not be empty", tap);
        }
        if !part
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            bail!(
                "Invalid tap '{}': only alphanumeric characters, hyphens, underscores, and dots are allowed",
                tap
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brew_log_paths_with_both_keys() {
        let json = br#"[{"name":"redis","log_path":"/opt/homebrew/var/log/redis.log","error_log_path":"/opt/homebrew/var/log/redis.err"}]"#;
        assert_eq!(
            parse_brew_log_paths("redis", json).unwrap(),
            LogSource::Files(vec![
                PathBuf::from("/opt/homebrew/var/log/redis.log"),
                PathBuf::from("/opt/homebrew/var/log/redis.err"),
            ])
        );
    }

    #[test]
    fn brew_log_paths_with_one_key_or_the_same_file_twice() {
        let json = br#"[{"name":"redis","error_log_path":"/v/redis.log"}]"#;
        assert_eq!(
            parse_brew_log_paths("redis", json).unwrap(),
            LogSource::Files(vec![PathBuf::from("/v/redis.log")])
        );
        let same = br#"[{"log_path":"/v/redis.log","error_log_path":"/v/redis.log"}]"#;
        assert_eq!(
            parse_brew_log_paths("redis", same).unwrap(),
            LogSource::Files(vec![PathBuf::from("/v/redis.log")])
        );
    }

    #[test]
    fn brew_log_paths_with_no_keys_is_unsupported() {
        let json = br#"[{"name":"redis","log_path":null}]"#;
        assert_eq!(
            parse_brew_log_paths("redis", json).unwrap(),
            LogSource::Unsupported(
                "brew did not report a log path for redis — check $(brew --prefix)/var/log".into()
            )
        );
        assert!(parse_brew_log_paths("redis", b"[]").is_err());
    }

    #[test]
    fn validate_tap_accepts_valid_org_repo() {
        assert!(validate_tap("hashicorp/tap").is_ok());
        assert!(validate_tap("my-org/my-repo").is_ok());
        assert!(validate_tap("org.name/repo_name").is_ok());
    }

    #[test]
    fn validate_tap_rejects_path_components() {
        assert!(validate_tap("org/repo/extra").is_err());
        assert!(validate_tap("org").is_err());
    }

    #[test]
    fn validate_tap_rejects_empty_parts() {
        assert!(validate_tap("/repo").is_err());
        assert!(validate_tap("org/").is_err());
    }

    #[test]
    fn validate_tap_rejects_shell_special_chars() {
        assert!(validate_tap("org/repo;rm -rf /").is_err());
        assert!(validate_tap("org/repo$(evil)").is_err());
        assert!(validate_tap("https://github.com/org/repo").is_err());
    }

    // ── brew_bin ──────────────────────────────────────────────────────────────

    #[test]
    fn brew_bin_returns_non_empty_path() {
        let b = Homebrew.brew_bin();
        assert!(!b.as_os_str().is_empty(), "brew_bin must not be empty");
        assert!(
            b.to_string_lossy().contains("brew"),
            "Expected path to contain 'brew', got: {}",
            b.display()
        );
    }

    #[test]
    fn brew_bin_prefers_which_when_prefix_set() {
        // which("brew") takes priority over HOMEBREW_PREFIX — the two must always agree.
        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Ok(which_path) = which("brew") else {
            // brew not on PATH; this behaviour is untestable without PATH manipulation
            return;
        };
        let prev = std::env::var("HOMEBREW_PREFIX").ok();
        // SAFETY: serialised by ENV_LOCK; HOMEBREW_PREFIX is only read by brew_bin().
        unsafe { std::env::set_var("HOMEBREW_PREFIX", "/bogus/homebrew") };
        let result = Homebrew.brew_bin();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOMEBREW_PREFIX", v),
                None => std::env::remove_var("HOMEBREW_PREFIX"),
            }
        }
        assert_eq!(
            result, which_path,
            "brew_bin must return the PATH-resolved brew, not the HOMEBREW_PREFIX path"
        );
    }

    #[test]
    fn brew_bin_uses_homebrew_prefix_when_brew_not_on_path() {
        // HOMEBREW_PREFIX is used only when brew is absent from PATH.
        // Skip when brew is installed since we cannot safely manipulate PATH in tests.
        if which("brew").is_ok() {
            return;
        }
        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("HOMEBREW_PREFIX").ok();
        // SAFETY: serialised by ENV_LOCK; HOMEBREW_PREFIX is only read by brew_bin().
        unsafe { std::env::set_var("HOMEBREW_PREFIX", "/custom/homebrew") };
        let result = Homebrew.brew_bin();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOMEBREW_PREFIX", v),
                None => std::env::remove_var("HOMEBREW_PREFIX"),
            }
        }
        assert_eq!(result, PathBuf::from("/custom/homebrew/bin/brew"));
    }

    #[test]
    fn brew_bin_falls_back_to_hardcoded_when_prefix_absent() {
        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("HOMEBREW_PREFIX").ok();
        // SAFETY: serialised by ENV_LOCK; HOMEBREW_PREFIX is only read by brew_bin().
        unsafe { std::env::remove_var("HOMEBREW_PREFIX") };
        let result = Homebrew.brew_bin();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOMEBREW_PREFIX", v),
                None => std::env::remove_var("HOMEBREW_PREFIX"),
            }
        }
        let s = result.to_string_lossy();
        assert!(s.contains("homebrew") || s.contains("local"));
        assert!(s.ends_with("/bin/brew"));
    }

    // ── name ──────────────────────────────────────────────────────────────────

    #[test]
    fn brew_name_is_brew() {
        assert_eq!(Homebrew.name(), "brew");
    }

    // ── parse_brew_service_info_json ─────────────────────────────────────────

    #[test]
    fn parse_brew_service_info_json_returns_true_when_running() {
        let json = br#"[{"name":"mysql","running":true}]"#;
        assert!(parse_brew_service_info_json(json).unwrap());
    }

    #[test]
    fn parse_brew_service_info_json_returns_false_when_stopped() {
        let json = br#"[{"name":"mysql","running":false}]"#;
        assert!(!parse_brew_service_info_json(json).unwrap());
    }

    #[test]
    fn parse_brew_service_info_json_returns_false_on_malformed_json() {
        let bad = b"not json";
        assert!(parse_brew_service_info_json(bad).is_err());
    }

    #[test]
    fn parse_brew_service_info_json_returns_false_when_running_absent() {
        // "running" key missing — default to false (service status unknown = not running)
        let json = br#"[{"name":"mysql","status":"stopped"}]"#;
        assert!(!parse_brew_service_info_json(json).unwrap());
    }

    #[test]
    fn parse_brew_service_info_json_returns_err_on_empty_array() {
        // Empty array means brew does not manage the service — must error, not silently return false.
        let json = b"[]";
        assert!(
            parse_brew_service_info_json(json).is_err(),
            "empty array must return Err, not silently return false"
        );
    }

    #[test]
    fn parse_brew_service_info_json_returns_err_on_non_array() {
        let json = br#"{"name":"mysql","running":true}"#;
        assert!(
            parse_brew_service_info_json(json).is_err(),
            "non-array JSON must return Err"
        );
    }

    // ── bootstrap ────────────────────────────────────────────────────────────

    #[test]
    fn ensure_available_returns_err_without_bootstrap_flag() {
        // Homebrew is not installed in CI; ensure_available(false) must return an error
        // mentioning --bootstrap rather than running the installer.
        // If Homebrew happens to be installed in the test environment, skip the assertion.
        let pm = Homebrew;
        if pm.is_available() {
            return;
        }
        let err = pm.ensure_available(false).unwrap_err();
        assert!(
            err.to_string().contains("--bootstrap"),
            "error must mention --bootstrap"
        );
    }

    // ── brew_formula_name ─────────────────────────────────────────────────────

    #[test]
    fn brew_formula_name_with_no_version_returns_base_name() {
        let dep = Dependency::simple("redis");
        assert_eq!(brew_formula_name(&dep), "redis");
    }

    #[test]
    fn brew_formula_name_with_major_pin_appends_version() {
        let mut dep = Dependency::simple("node");
        dep.version = Some("20".into());
        assert_eq!(brew_formula_name(&dep), "node@20");
    }

    #[test]
    fn brew_formula_name_with_resolved_version_returns_base_name() {
        // Lock-injected versions like "7.2.4" contain dots — must NOT become "redis@7.2.4".
        let mut dep = Dependency::simple("redis");
        dep.version = Some("7.2.4".into());
        assert_eq!(brew_formula_name(&dep), "redis");
    }

    #[test]
    fn brew_formula_name_with_full_node_lock_version_returns_base_name() {
        let mut dep = Dependency::simple("node");
        dep.version = Some("20.11.0".into());
        assert_eq!(brew_formula_name(&dep), "node");
    }

    #[test]
    fn brew_formula_name_with_major_minor_pin_appends_version() {
        // "3.14" is a valid brew major.minor pin (e.g. python@3.14).
        let mut dep = Dependency::simple("python");
        dep.version = Some("3.14".into());
        assert_eq!(brew_formula_name(&dep), "python@3.14");
    }

    #[test]
    fn brew_formula_name_with_python_lock_version_returns_base_name() {
        // Lock-injected version "3.14.4_1" must NOT become "python@3.14.4_1".
        let mut dep = Dependency::simple("python");
        dep.version = Some("3.14.4_1".into());
        assert_eq!(brew_formula_name(&dep), "python");
    }

    #[test]
    fn brew_formula_name_with_three_part_version_returns_base_name() {
        let mut dep = Dependency::simple("python");
        dep.version = Some("3.14.4".into());
        assert_eq!(brew_formula_name(&dep), "python");
    }

    // ── is_formula_pin ────────────────────────────────────────────────────────

    #[test]
    fn is_formula_pin_major_only() {
        assert!(is_formula_pin("20"));
        assert!(is_formula_pin("3"));
    }

    #[test]
    fn is_formula_pin_major_minor() {
        assert!(is_formula_pin("3.14"));
        assert!(is_formula_pin("8.0"));
    }

    #[test]
    fn is_formula_pin_rejects_three_parts() {
        assert!(!is_formula_pin("3.14.4"));
        assert!(!is_formula_pin("20.11.0"));
    }

    #[test]
    fn is_formula_pin_rejects_build_suffix() {
        assert!(!is_formula_pin("3.14.4_1"));
        assert!(!is_formula_pin("8.0.36_1"));
    }

    // ── parse_brew_version ────────────────────────────────────────────────────

    #[test]
    fn parse_brew_version_extracts_second_token() {
        assert_eq!(parse_brew_version("mysql 8.0.36"), Some("8.0.36".into()));
        assert_eq!(
            parse_brew_version("node@20 20.11.0"),
            Some("20.11.0".into())
        );
    }

    #[test]
    fn parse_brew_version_returns_none_for_single_token() {
        assert!(parse_brew_version("mysql").is_none());
        assert!(parse_brew_version("").is_none());
    }
}
