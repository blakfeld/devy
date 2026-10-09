use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{LogSource, PackageManager};
use crate::config::Dependency;
use crate::installers;
use crate::output;

#[derive(Default)]
pub struct Homebrew {
    /// The prefix `package_bin_dir` looks in, found once (`devy exec` asks per dependency).
    /// `formula_bin_dir` checks on every call that it is outside the project.
    prefix: std::sync::OnceLock<Option<PathBuf>>,
}

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
///
/// A version that came from devy.lock never selects a formula: the lock is untrusted input
/// and a value like `"16"` must not silently switch the install to `node@16`.
pub(crate) fn brew_formula_name(dep: &Dependency) -> String {
    match &dep.version {
        Some(v) if !dep.version_from_lock && is_formula_pin(v) => format!("{}@{}", dep.name, v),
        _ => dep.name.clone(),
    }
}

/// `brew list --versions -- <formula>`
fn list_versions_args(formula: &str) -> Vec<String> {
    vec![
        "list".into(),
        "--versions".into(),
        "--".into(),
        formula.to_string(),
    ]
}

/// `brew tap -- <tap>`
fn tap_args(tap: &str) -> Vec<String> {
    vec!["tap".into(), "--".into(), tap.to_string()]
}

/// `brew install -- <formula>`
fn install_args(formula: &str) -> Vec<String> {
    vec!["install".into(), "--".into(), formula.to_string()]
}

/// Returns true when `v` looks like a user-specified version pin rather than a resolved version.
/// Returns true for a Homebrew formula or cask token: `[a-z0-9][a-z0-9@._+-]*`, not ending
/// in `.rb`, `.json`, `.tar.gz` or `.tgz` and not containing `.bottle.`. This keeps
/// `brew install` from loading a local formula file (e.g. `./evil.rb` via a bare
/// `evil.rb`) or a local bottle (`x.arm64_sonoma.bottle.tar.gz`) from the working directory.
fn is_formula_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '@' | '.' | '_' | '+' | '-')
        })
        && ![".rb", ".json", ".tar.gz", ".tgz"]
            .iter()
            .any(|ext| name.ends_with(ext))
        && !name.contains(".bottle.")
}

/// Formula pins are one or two dot-separated groups of ASCII digits.
/// "20" → true, "3.14" → true, "8.0" → true
/// "20.11.0" → false (3 parts), "3.14.4_1" → false (underscore), "evil.rb" → false,
/// "a/b" → false — so a pin can never turn the formula into a path or a tap reference.
fn is_formula_pin(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() <= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Symlinks followed at most when looking for the Homebrew prefix, as the kernel's limit.
const MAX_BREW_LINK_HOPS: usize = 40;

/// The Homebrew prefix of the `brew` at `brew`. When `brew` is a link, the first prefix
/// with an `opt` directory along its chain of links, one hop at a time: a `~/bin/brew`
/// link to `/opt/homebrew/bin/brew`, or to Intel's `/usr/local/bin/brew` (itself a link
/// into `/usr/local/Homebrew`, which has no `opt`), whatever `~/opt` holds. Otherwise,
/// when the real path of `brew` sits in a prefix with `opt` that is not the parent of
/// `brew`'s own `bin`, that prefix; else the parent of `brew`'s `bin`, not canonicalized:
/// on Apple Silicon both are `/opt/homebrew`, and Intel's `/usr/local/bin/brew` gives
/// `/usr/local`. Every candidate must pass `trusted_prefix`; with none, there is no
/// prefix. Whether the prefix is project-local is checked by the caller on every
/// use.
fn brew_prefix_for(brew: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    let owner = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(brew).ok().map(|meta| meta.uid())
    };
    #[cfg(not(unix))]
    let owner = None;
    brew_prefix_with(brew, owner)
}

/// `brew_prefix_for` with the uid owning the real `brew` executable injected.
fn brew_prefix_with(brew: &Path, brew_owner: Option<u32>) -> Option<PathBuf> {
    let trusted = |prefix: &Path| trusted_prefix(prefix, brew_owner);
    let prefix_of = |brew: &Path| Some(brew.parent()?.parent()?.to_path_buf());
    let direct = prefix_of(brew)?;
    let mut hop = brew.to_path_buf();
    for _ in 0..MAX_BREW_LINK_HOPS {
        let Ok(target) = std::fs::read_link(&hop) else {
            break;
        };
        hop = hop.parent()?.join(target);
        if let Some(prefix) = prefix_of(&hop).filter(|p| trusted(p)) {
            // A relative link leaves `..` in the path; PATH and JAVA_HOME get none.
            return prefix.canonicalize().ok();
        }
    }
    let real = brew.canonicalize().ok().and_then(|real| prefix_of(&real));
    match real {
        Some(real) if trusted(&real) && direct.canonicalize().ok().as_ref() != Some(&real) => {
            Some(real)
        }
        _ => trusted(&direct).then_some(direct),
    }
}

/// Whether formulae may be taken from `<prefix>/opt`: the prefix directory and `opt`
/// (both followed through links) are directories owned by a trusted owner and not
/// world-writable, and when `opt` is a link, the link itself has a trusted owner. So
/// another local user can't plant formulae, or a link to swap later, through a shared
/// directory such as `/tmp`. Trusted owners are the current user, root and the owner of
/// the real `brew` devy runs (on a shared Mac, the account that installed Homebrew).
/// Group-writable only for the `wheel` or `admin` group (see
/// `writable_only_by_trusted`).
#[cfg(unix)]
fn trusted_prefix(prefix: &Path, brew_owner: Option<u32>) -> bool {
    use std::os::unix::fs::MetadataExt;
    let owner_ok = |uid: u32| crate::fs_safe::is_user_or_root(uid) || Some(uid) == brew_owner;
    let dir_ok = |path: &Path| {
        std::fs::metadata(path).is_ok_and(|meta| {
            meta.is_dir()
                && owner_ok(meta.uid())
                && writable_only_by_trusted(meta.mode(), meta.gid())
        })
    };
    let opt = prefix.join("opt");
    let link_ok = std::fs::symlink_metadata(&opt)
        .is_ok_and(|meta| !meta.file_type().is_symlink() || owner_ok(meta.uid()));
    dir_ok(prefix) && link_ok && dir_ok(&opt)
}

/// Whether a directory with `mode` and group `gid` can be written only by its owner and
/// trusted groups: never by everyone, and by its group only when that is `wheel` (0) or,
/// on macOS, `admin` (80), as Intel Homebrew sets up `/usr/local`. Every macOS account
/// is in `staff`, so a `staff`-writable directory is open to all local users.
#[cfg(unix)]
fn writable_only_by_trusted(mode: u32, gid: u32) -> bool {
    const TRUSTED_GROUPS: &[u32] = &[0, 80];
    mode & 0o002 == 0 && (mode & 0o020 == 0 || TRUSTED_GROUPS.contains(&gid))
}

#[cfg(not(unix))]
fn trusted_prefix(prefix: &Path, _brew_owner: Option<u32>) -> bool {
    prefix.join("opt").is_dir()
}

/// `<prefix>/opt/<formula>/bin` for the formula devy installs for `pkg`, when it is a
/// directory. A relative or project-local prefix or directory (an `opt` link into the
/// project), or a name that is not a formula token, gives `None`.
fn formula_bin_dir(prefix: &Path, pkg: &Dependency) -> Option<PathBuf> {
    let formula = brew_formula_name(pkg);
    if !is_formula_name(&formula) || crate::fs_safe::is_project_local(prefix) {
        return None;
    }
    let dir = prefix.join("opt").join(formula).join("bin");
    (dir.is_dir() && !crate::fs_safe::is_project_local(&dir)).then_some(dir)
}

/// See `Homebrew::command`.
fn brew_command(brew: PathBuf) -> Command {
    let mut cmd = Command::new(brew);
    cmd.env("HOMEBREW_FORBID_PACKAGES_FROM_PATHS", "1")
        .current_dir("/");
    cmd
}

impl Homebrew {
    fn brew_bin(&self) -> PathBuf {
        self.brew_bin_with(std::env::var("HOMEBREW_PREFIX").ok())
    }

    /// `brew_bin` with the `$HOMEBREW_PREFIX` value injected.
    fn brew_bin_with(&self, homebrew_prefix: Option<String>) -> PathBuf {
        // Prefer the brew on PATH (outside the project) so brew_bin() and is_available()
        // always agree.
        if let Some(path) = crate::fs_safe::which_outside_project("brew") {
            return path;
        }
        // An activated project environment can set HOMEBREW_PREFIX; never let it point
        // devy at a brew inside the project (or relative to the current directory).
        if let Some(prefix) = homebrew_prefix
            && !prefix.is_empty()
            && !crate::fs_safe::is_project_local(std::path::Path::new(&prefix))
        {
            return PathBuf::from(prefix).join("bin").join("brew");
        }
        if cfg!(target_arch = "aarch64") {
            PathBuf::from("/opt/homebrew/bin/brew")
        } else {
            PathBuf::from("/usr/local/bin/brew")
        }
    }

    /// A `brew` command that never loads a formula, cask or bottle from a local file
    /// (`HOMEBREW_FORBID_PACKAGES_FROM_PATHS`) and runs from `/`, so a bare name can't
    /// resolve to a file in the project.
    fn command(&self) -> Command {
        brew_command(self.brew_bin())
    }

    fn run<S: AsRef<str>>(&self, args: &[S]) -> Result<std::process::Output> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        self.command()
            .args(&args)
            .output()
            .with_context(|| format!("Failed to run: brew {}", args.join(" ")))
    }

    fn run_interactive<S: AsRef<str>>(&self, args: &[S]) -> Result<()> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        let status = self
            .command()
            .args(&args)
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
        let output = self
            .command()
            .args(config_prefix_args(service))
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // Never a relative (or empty) prefix, which would resolve against the cwd.
        let prefix = std::path::Path::new(&prefix);
        (prefix.is_absolute() && !prefix.to_string_lossy().chars().any(char::is_control))
            .then(|| prefix.join("etc"))
    }
}

/// `brew` arguments printing the prefix whose `etc` holds `service`'s config. brew's
/// mysqld and mariadbd read `$(brew --prefix)/etc/my.cnf`, not the keg's `etc` (which
/// is ignored and replaced on upgrade), and the kafka, zookeeper and rabbitmq formulae's
/// services read `$(brew --prefix)/etc/<service>/`, so they use the global prefix.
fn config_prefix_args(service: &str) -> Vec<&str> {
    match service {
        "mysql" | "mariadb" | "kafka" | "zookeeper" | "rabbitmq" => vec!["--prefix"],
        _ => vec!["--prefix", "--", service],
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
        crate::fs_safe::which_outside_project("brew").is_some()
    }

    fn bootstrap(&self) -> Result<()> {
        output::step(&format!(
            "Bootstrapping Homebrew ({}), verified against its pinned SHA-256",
            installers::HOMEBREW.url
        ));
        let status = installers::run_script(
            &installers::HOMEBREW,
            installers::Interpreter::Bash,
            &[],
            &[],
        )?;
        if !status.success() {
            bail!("Homebrew installation failed");
        }
        Ok(())
    }

    fn is_package_installed(&self, dep: &Dependency) -> Result<bool> {
        let output = self.run(&list_versions_args(&brew_formula_name(dep)))?;
        Ok(output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty())
    }

    fn install_package(&self, dep: &Dependency) -> Result<()> {
        // Dependency names are validated at config load and cannot contain `/`, so the
        // formula name can never address a tap (`org/tap/formula`). Re-check here as
        // defense in depth so no such name ever reaches `brew install`.
        if dep.name.contains('/') {
            bail!(
                "Invalid formula name '{}': taps must be set with the `tap` field",
                dep.name
            );
        }
        if !is_formula_name(&dep.name) {
            bail!("Invalid formula name '{}'", dep.name);
        }
        if let Some(tap) = &dep.tap {
            validate_tap(tap)?;
            self.run_interactive(&tap_args(tap))?;
        }
        self.run_interactive(&install_args(&brew_formula_name(dep)))
    }

    fn is_service_running(&self, name: &str) -> Result<bool> {
        let output = self.run(&["services", "info", "--json", "--", name])?;
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
        self.run_interactive(&["services", "start", "--", name])
    }

    fn stop_service(&self, name: &str) -> Result<()> {
        self.run_interactive(&["services", "stop", "--", name])
    }

    fn resolved_version(&self, dep: &Dependency) -> Result<Option<String>> {
        let output = self.run(&list_versions_args(&brew_formula_name(dep)))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.trim();
        // Output: "formula 1.2.3" or "formula@major 1.2.3_4" — take second token.
        Ok(parse_brew_version(line))
    }

    fn service_config_dir(&self, service: &str) -> Option<PathBuf> {
        self.fetch_config_dir(service)
    }

    fn log_source(&self, name: &str, _lines: u32, _follow: bool) -> Result<LogSource> {
        let output = self.run(&["services", "info", "--json", "--", name])?;
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

    /// `<prefix>/opt/<formula>/bin`, found on disk without running `brew`, in the prefix
    /// of the `brew` devy runs (see `brew_prefix_for`).
    fn package_bin_dir(&self, pkg: &Dependency) -> Option<PathBuf> {
        let prefix = self
            .prefix
            .get_or_init(|| brew_prefix_for(&self.brew_bin()));
        formula_bin_dir(prefix.as_deref()?, pkg)
    }
}

/// Validates that a tap string has the form `org/repo`: exactly one `/`, each part
/// non-empty, starting with an ASCII letter or digit, limited to ASCII letters, digits,
/// `-`, `_` and `.`, and not `.` or `..`. This rejects URLs, extra path components,
/// option-like values and shell metacharacters. It does not restrict *which* GitHub
/// repository is tapped: any well-formed `org/repo` is accepted. Taps appear in the
/// executable entry listing (AI init/doctor review).
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
        if *part == "." || *part == ".." {
            bail!(
                "Invalid tap '{}': org and repo must not be '.' or '..'",
                tap
            );
        }
        if !part.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            bail!(
                "Invalid tap '{}': org and repo must start with a letter or digit",
                tap
            );
        }
        if !part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
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
    use which::which;

    #[cfg(unix)]
    #[test]
    fn bootstrap_runs_pinned_install_sh_with_bash() {
        use crate::installers::{self, Interpreter, test_hooks};
        test_hooks::clear();
        test_hooks::serve(&installers::HOMEBREW, b"exit 0\n");
        let result = Homebrew::default().bootstrap();
        let runs = test_hooks::runs();
        test_hooks::clear();
        result.unwrap();
        assert_eq!(
            runs,
            vec![(installers::HOMEBREW.name, Interpreter::Bash, vec![])]
        );
    }

    #[cfg(unix)]
    #[test]
    fn bootstrap_failure_reports_homebrew_installation_failed() {
        use crate::installers::{self, test_hooks};
        test_hooks::clear();
        test_hooks::serve(&installers::HOMEBREW, b"exit 1\n");
        let err = Homebrew::default().bootstrap().unwrap_err();
        test_hooks::clear();
        assert_eq!(err.to_string(), "Homebrew installation failed");
    }

    #[test]
    fn mysql_family_config_uses_global_prefix_not_keg() {
        assert_eq!(config_prefix_args("mysql"), vec!["--prefix"]);
        assert_eq!(config_prefix_args("mariadb"), vec!["--prefix"]);
        assert_eq!(
            config_prefix_args("postgresql"),
            vec!["--prefix", "--", "postgresql"]
        );
    }

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

    #[test]
    fn validate_tap_rejects_dot_parts() {
        assert!(validate_tap("./repo").is_err());
        assert!(validate_tap("org/..").is_err());
        assert!(validate_tap("../..").is_err());
    }

    #[test]
    fn validate_tap_requires_alphanumeric_first_character() {
        assert!(validate_tap("-org/repo").is_err());
        assert!(validate_tap("org/.repo").is_err());
        assert!(validate_tap("_org/repo").is_err());
        assert!(validate_tap("org/-repo").is_err());
    }

    #[test]
    fn validate_tap_rejects_non_ascii() {
        assert!(validate_tap("org\u{e9}/repo").is_err());
    }

    #[test]
    fn validate_config_rejects_invalid_tap() {
        // Scenario: Invalid tap -- `tap: "evil; rm -rf /"` fails before anything installs.
        let mut dep = Dependency::simple("mongodb-community");
        dep.tap = Some("evil; rm -rf /".into());
        assert!(Homebrew::default().validate_config(&dep).is_err());
    }

    #[test]
    fn valid_tap_and_install_argv_use_separators() {
        // Scenario: Valid tap -- `brew tap -- mongodb/brew`, then install the formula.
        let mut dep = Dependency::simple("mongodb-community");
        dep.tap = Some("mongodb/brew".into());
        assert!(Homebrew::default().validate_config(&dep).is_ok());
        assert_eq!(tap_args("mongodb/brew"), ["tap", "--", "mongodb/brew"]);
        assert_eq!(
            install_args(&brew_formula_name(&dep)),
            ["install", "--", "mongodb-community"]
        );
    }

    #[test]
    fn install_refuses_name_with_slash_before_running_brew() {
        // Scenario: Tap smuggled through the name. Config loading rejects it first; this is
        // the backend's own guard, which bails before any `brew` process is spawned.
        let dep = Dependency::simple("evilorg/tap/formula");
        let err = Homebrew::default()
            .install_package(&dep)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Invalid formula name"), "{err}");
    }

    #[test]
    fn list_versions_args_use_separator() {
        assert_eq!(
            list_versions_args("node@20"),
            ["list", "--versions", "--", "node@20"]
        );
    }

    // ── brew_bin ──────────────────────────────────────────────────────────────

    #[test]
    fn brew_bin_returns_non_empty_path() {
        let b = Homebrew::default().brew_bin();
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
        // HOMEBREW_PREFIX is injected, not set in the environment all tests share.
        let Ok(which_path) = which("brew") else {
            // brew not on PATH; this behaviour is untestable without PATH manipulation
            return;
        };
        let result = Homebrew::default().brew_bin_with(Some("/bogus/homebrew".into()));
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
        // An absolute path outside the project on every platform ("/custom/…" is
        // relative on Windows, so it would be refused as project-local).
        let project = crate::test_support::tmp_dir();
        crate::fs_safe::set_project_root(&project);
        let prefix = crate::test_support::tmp_dir();
        let result = Homebrew::default().brew_bin_with(Some(prefix.to_string_lossy().into_owned()));
        assert_eq!(result, prefix.join("bin").join("brew"));
    }

    #[test]
    fn brew_bin_refuses_a_project_local_homebrew_prefix() {
        // A project's environment can set HOMEBREW_PREFIX; it must never select a brew
        // inside the project, a relative one, or an empty prefix.
        if which("brew").is_ok() {
            return;
        }
        let project = crate::test_support::tmp_dir();
        crate::fs_safe::set_project_root(&project);
        let inside = project.join("homebrew");
        for prefix in [
            inside.to_string_lossy().into_owned(),
            "rel/homebrew".to_string(),
            String::new(),
        ] {
            let result = Homebrew::default().brew_bin_with(Some(prefix.clone()));
            assert_ne!(
                result,
                PathBuf::from(&prefix).join("bin").join("brew"),
                "{prefix:?}"
            );
            assert!(!result.starts_with(&*project), "{prefix:?}: {result:?}");
        }
    }

    #[test]
    fn brew_bin_falls_back_to_hardcoded_when_prefix_absent() {
        let result = Homebrew::default().brew_bin_with(None);
        let s = result.to_string_lossy();
        assert!(s.contains("homebrew") || s.contains("local"));
        assert!(s.ends_with("/bin/brew"));
    }

    // ── formula_bin_dir ───────────────────────────────────────────────────────

    #[test]
    fn formula_bin_dir_returns_an_existing_opt_bin() {
        let prefix = crate::test_support::tmp_dir();
        let bin = prefix.join("opt/jq/bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert_eq!(
            formula_bin_dir(&prefix, &Dependency::simple("jq")),
            Some(bin)
        );
    }

    #[test]
    fn formula_bin_dir_is_none_when_the_formula_is_not_installed() {
        let prefix = crate::test_support::tmp_dir();
        assert_eq!(formula_bin_dir(&prefix, &Dependency::simple("jq")), None);
        // A file where the directory would be is not a bin dir either.
        std::fs::create_dir_all(prefix.join("opt/jq")).unwrap();
        std::fs::write(prefix.join("opt/jq/bin"), "").unwrap();
        assert_eq!(formula_bin_dir(&prefix, &Dependency::simple("jq")), None);
    }

    #[test]
    fn formula_bin_dir_uses_the_pinned_formula() {
        let prefix = crate::test_support::tmp_dir();
        std::fs::create_dir_all(prefix.join("opt/postgresql/bin")).unwrap();
        let pinned = prefix.join("opt/postgresql@16/bin");
        std::fs::create_dir_all(&pinned).unwrap();
        let dep = Dependency {
            version: Some("16".into()),
            ..Dependency::simple("postgresql")
        };
        assert_eq!(formula_bin_dir(&prefix, &dep), Some(pinned));
    }

    #[test]
    fn formula_bin_dir_ignores_a_lock_sourced_version() {
        let prefix = crate::test_support::tmp_dir();
        let bin = prefix.join("opt/node/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(prefix.join("opt/node@16/bin")).unwrap();
        let dep = Dependency {
            version: Some("16".into()),
            version_from_lock: true,
            ..Dependency::simple("node")
        };
        assert_eq!(formula_bin_dir(&prefix, &dep), Some(bin));
    }

    #[test]
    fn formula_bin_dir_refuses_a_project_local_prefix() {
        let project = crate::test_support::tmp_dir();
        crate::fs_safe::set_project_root(&project);
        let prefix = project.join("homebrew");
        std::fs::create_dir_all(prefix.join("opt/jq/bin")).unwrap();
        assert_eq!(formula_bin_dir(&prefix, &Dependency::simple("jq")), None);
        assert_eq!(
            formula_bin_dir(Path::new("rel/homebrew"), &Dependency::simple("jq")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn formula_bin_dir_refuses_an_opt_link_into_the_project() {
        let project = crate::test_support::tmp_dir();
        crate::fs_safe::set_project_root(&project);
        std::fs::create_dir_all(project.join("evil/bin")).unwrap();
        let prefix = crate::test_support::tmp_dir();
        std::fs::create_dir_all(prefix.join("opt")).unwrap();
        std::os::unix::fs::symlink(project.join("evil"), prefix.join("opt/jq")).unwrap();
        assert_eq!(formula_bin_dir(&prefix, &Dependency::simple("jq")), None);
    }

    #[test]
    fn formula_bin_dir_refuses_a_prefix_above_the_project() {
        // The prefix is an ancestor of the project, not inside it, but the bin dir is.
        let prefix = crate::test_support::tmp_dir();
        std::fs::create_dir_all(prefix.join("opt/jq/bin")).unwrap();
        crate::fs_safe::set_project_root(&prefix.join("opt/jq"));
        assert_eq!(formula_bin_dir(&prefix, &Dependency::simple("jq")), None);
    }

    /// A Homebrew prefix at `prefix` with `opt` and a `bin/brew` file.
    #[cfg(unix)]
    fn fake_prefix(prefix: &Path) {
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(prefix.join("opt")).unwrap();
        std::fs::write(prefix.join("bin/brew"), "").unwrap();
        set_mode(prefix, 0o755);
        set_mode(&prefix.join("opt"), 0o755);
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn brew_prefix_apple_silicon_layout() {
        let tmp = crate::test_support::tmp_dir();
        let prefix = tmp.join("opt/homebrew");
        fake_prefix(&prefix);
        assert_eq!(brew_prefix_for(&prefix.join("bin/brew")), Some(prefix));
    }

    #[cfg(unix)]
    #[test]
    fn brew_prefix_intel_layout_keeps_usr_local() {
        // `/usr/local/bin/brew` → `../Homebrew/bin/brew`; `opt` is in `/usr/local`.
        let tmp = crate::test_support::tmp_dir();
        let local = tmp.join("usr/local");
        std::fs::create_dir_all(local.join("Homebrew/bin")).unwrap();
        std::fs::write(local.join("Homebrew/bin/brew"), "").unwrap();
        std::fs::create_dir_all(local.join("bin")).unwrap();
        std::fs::create_dir_all(local.join("opt")).unwrap();
        set_mode(&local, 0o755);
        set_mode(&local.join("opt"), 0o755);
        std::os::unix::fs::symlink("../Homebrew/bin/brew", local.join("bin/brew")).unwrap();
        assert_eq!(brew_prefix_for(&local.join("bin/brew")), Some(local));
    }

    #[cfg(unix)]
    #[test]
    fn brew_prefix_follows_a_brew_link_to_intel_usr_local() {
        // `~/bin/brew` → `/usr/local/bin/brew` → `../Homebrew/bin/brew`: the real path's
        // prefix has no `opt`, but the first hop's does.
        let tmp = crate::test_support::tmp_dir();
        let local = tmp.join("usr/local");
        std::fs::create_dir_all(local.join("Homebrew/bin")).unwrap();
        std::fs::write(local.join("Homebrew/bin/brew"), "").unwrap();
        std::fs::create_dir_all(local.join("bin")).unwrap();
        std::fs::create_dir_all(local.join("opt")).unwrap();
        set_mode(&local, 0o755);
        set_mode(&local.join("opt"), 0o755);
        std::os::unix::fs::symlink("../Homebrew/bin/brew", local.join("bin/brew")).unwrap();
        let home = tmp.join("home");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::create_dir_all(home.join("opt")).unwrap();
        std::os::unix::fs::symlink(local.join("bin/brew"), home.join("bin/brew")).unwrap();
        assert_eq!(
            brew_prefix_for(&home.join("bin/brew")),
            Some(local.canonicalize().unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn brew_prefix_follows_a_brew_link_outside_its_prefix() {
        // `~/bin/brew` → `/opt/homebrew/bin/brew`, with or without an unrelated `~/opt`.
        let tmp = crate::test_support::tmp_dir();
        let prefix = tmp.join("homebrew");
        fake_prefix(&prefix);
        let home = tmp.join("home");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::os::unix::fs::symlink(prefix.join("bin/brew"), home.join("bin/brew")).unwrap();
        let expected = Some(prefix.canonicalize().unwrap());
        assert_eq!(brew_prefix_for(&home.join("bin/brew")), expected);
        std::fs::create_dir_all(home.join("opt")).unwrap();
        assert_eq!(brew_prefix_for(&home.join("bin/brew")), expected);
    }

    #[cfg(unix)]
    #[test]
    fn brew_prefix_skips_a_world_writable_opt() {
        use std::os::unix::fs::PermissionsExt;
        // `~/bin/brew` → `<tmp>/bin/brew` → the real prefix, where `<tmp>` is sticky and
        // world-writable like /tmp: another user could plant `<tmp>/opt/<formula>`.
        let tmp = crate::test_support::tmp_dir();
        let prefix = tmp.join("homebrew");
        fake_prefix(&prefix);
        let shared = tmp.join("shared");
        std::fs::create_dir_all(shared.join("bin")).unwrap();
        std::fs::create_dir_all(shared.join("opt")).unwrap();
        std::fs::set_permissions(shared.join("opt"), std::fs::Permissions::from_mode(0o1777))
            .unwrap();
        std::os::unix::fs::symlink(prefix.join("bin/brew"), shared.join("bin/brew")).unwrap();
        let home = tmp.join("home");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::os::unix::fs::symlink(shared.join("bin/brew"), home.join("bin/brew")).unwrap();
        assert_eq!(
            brew_prefix_for(&home.join("bin/brew")),
            Some(prefix.canonicalize().unwrap())
        );

        // With the real prefix's `opt` world-writable too, nothing qualifies.
        std::fs::set_permissions(prefix.join("opt"), std::fs::Permissions::from_mode(0o777))
            .unwrap();
        assert_eq!(brew_prefix_for(&home.join("bin/brew")), None);
        assert_eq!(brew_prefix_for(&prefix.join("bin/brew")), None);
    }

    #[cfg(unix)]
    #[test]
    fn group_write_is_trusted_only_for_wheel_and_admin() {
        assert!(writable_only_by_trusted(0o755, 20));
        assert!(writable_only_by_trusted(0o775, 0));
        assert!(writable_only_by_trusted(0o775, 80));
        assert!(!writable_only_by_trusted(0o775, 20));
        assert!(!writable_only_by_trusted(0o757, 0));
        assert!(!writable_only_by_trusted(0o1777, 80));
    }

    #[cfg(unix)]
    #[test]
    fn brew_prefix_refuses_an_opt_writable_by_an_ordinary_group() {
        use std::os::unix::fs::MetadataExt;
        let tmp = crate::test_support::tmp_dir();
        let prefix = tmp.join("homebrew");
        fake_prefix(&prefix);
        let gid = std::fs::metadata(prefix.join("opt")).unwrap().gid();
        if [0, 80].contains(&gid) {
            return; // The temp dir's group is trusted here; nothing to refuse.
        }
        set_mode(&prefix.join("opt"), 0o775);
        assert_eq!(brew_prefix_for(&prefix.join("bin/brew")), None);
    }

    #[cfg(unix)]
    #[test]
    fn brew_prefix_refuses_an_opt_link_in_a_world_writable_prefix() {
        // `/tmp/opt` → a trusted `opt`: whoever can write `/tmp` can swap the link later.
        let tmp = crate::test_support::tmp_dir();
        let trusted = tmp.join("homebrew");
        fake_prefix(&trusted);
        let shared = tmp.join("shared");
        std::fs::create_dir_all(shared.join("bin")).unwrap();
        std::fs::write(shared.join("bin/brew"), "").unwrap();
        std::os::unix::fs::symlink(trusted.join("opt"), shared.join("opt")).unwrap();
        set_mode(&shared, 0o1777);
        assert_eq!(brew_prefix_for(&shared.join("bin/brew")), None);
        set_mode(&shared, 0o755);
        assert_eq!(brew_prefix_for(&shared.join("bin/brew")), Some(shared));
    }

    #[cfg(unix)]
    #[test]
    fn brew_prefix_trusts_only_the_user_root_or_the_brew_owner() {
        let tmp = crate::test_support::tmp_dir();
        let prefix = tmp.join("homebrew");
        fake_prefix(&prefix);
        let brew = prefix.join("bin/brew");
        let me = crate::fs_safe::current_uid();
        if me == 0 {
            return; // Root is always trusted, so nothing can be presented as untrusted.
        }
        // Present the prefix as another account's (the faked current user is not `me`).
        let other = me + 1;
        crate::fs_safe::with_fake_owner(other, || {
            assert_eq!(brew_prefix_with(&brew, Some(other)), None);
            assert_eq!(brew_prefix_with(&brew, None), None);
            // On a shared Mac, the account owning `brew` also owns the prefix.
            assert_eq!(brew_prefix_with(&brew, Some(me)), Some(prefix.clone()));
        });
    }

    #[cfg(unix)]
    #[test]
    fn package_bin_dir_checks_the_cached_prefix_on_every_call() {
        let tmp = crate::test_support::tmp_dir();
        let prefix = tmp.join("homebrew");
        fake_prefix(&prefix);
        std::fs::create_dir_all(prefix.join("opt/jq/bin")).unwrap();
        let brew = Homebrew::default();
        brew.prefix
            .set(brew_prefix_for(&prefix.join("bin/brew")))
            .unwrap();
        let jq = Dependency::simple("jq");
        crate::fs_safe::set_project_root(&tmp.join("elsewhere"));
        assert_eq!(brew.package_bin_dir(&jq), Some(prefix.join("opt/jq/bin")));
        // The same cached prefix, now inside the project root.
        crate::fs_safe::set_project_root(&tmp);
        assert_eq!(brew.package_bin_dir(&jq), None);
    }

    // ── name ──────────────────────────────────────────────────────────────────

    #[test]
    fn brew_name_is_brew() {
        assert_eq!(Homebrew::default().name(), "brew");
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
        let pm = Homebrew::default();
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

    #[test]
    fn brew_formula_name_refuses_lock_formula_selector() {
        // A lock `resolved_version` of "16" must not switch the install to node@16.
        let mut dep = Dependency::simple("node");
        dep.version = Some("16".into());
        dep.version_from_lock = true;
        assert_eq!(brew_formula_name(&dep), "node");
        assert_eq!(
            install_args(&brew_formula_name(&dep)),
            ["install", "--", "node"]
        );
    }

    #[test]
    fn brew_formula_name_keeps_config_formula_selector() {
        let mut dep = Dependency::simple("node");
        dep.version = Some("16".into());
        assert_eq!(brew_formula_name(&dep), "node@16");
    }

    #[test]
    fn lock_resolved_version_is_not_refused() {
        let mut dep = Dependency::simple("node");
        dep.version = Some("20.11.0".into());
        dep.version_from_lock = true;
        assert_eq!(brew_formula_name(&dep), "node");
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
    fn is_formula_name_rejects_local_files() {
        for ok in ["jq", "postgresql@16", "libxml2", "gtk+3", "node.js-ish"] {
            assert!(is_formula_name(ok), "{ok} must be accepted");
        }
        for bad in [
            "evil.rb",
            "evil.json",
            "Evil",
            "a:b",
            "a\\b",
            "",
            "-x",
            ".rb",
        ] {
            assert!(!is_formula_name(bad), "{bad} must be rejected");
        }
        let err = Homebrew::default()
            .install_package(&Dependency::simple("evil.rb"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Invalid formula name"), "{err}");
    }

    #[test]
    fn is_formula_pin_rejects_non_numeric() {
        assert!(!is_formula_pin("evil.rb"));
        assert!(!is_formula_pin("a/b"));
        assert!(!is_formula_pin("3."));
        assert!(!is_formula_pin(""));
        assert!(!is_formula_pin("latest"));
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

    #[test]
    fn formula_name_rejects_local_bottles_and_archives() {
        for bad in [
            "x.arm64_sonoma.bottle.tar.gz",
            "x.bottle.1.tar.gz",
            "x.bottle.json",
            "evil.tar.gz",
            "evil.tgz",
            "evil.rb",
        ] {
            assert!(!is_formula_name(bad), "{bad} must be rejected");
            assert!(
                !crate::validate::dep_name(bad),
                "{bad} must fail config validation"
            );
        }
        for good in ["python@3.14", "gcc", "libpq", "c++utils", "node.js"] {
            assert!(is_formula_name(good), "{good}");
        }
    }

    #[test]
    fn brew_commands_forbid_local_packages_and_run_from_root() {
        let cmd = brew_command(PathBuf::from("/opt/homebrew/bin/brew"));
        assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new("/")));
        assert!(cmd.get_envs().any(|(k, v)| {
            k == "HOMEBREW_FORBID_PACKAGES_FROM_PATHS" && v == Some(std::ffi::OsStr::new("1"))
        }));
    }
}
