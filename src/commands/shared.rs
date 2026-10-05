use anyhow::Result;
use colored::Colorize;
use serde::Serialize;
use std::collections::HashMap;

use crate::ai::redact;
use crate::config::Dependency;
use crate::modules;
use crate::output::clean_line;
use crate::service_runner::{self, Runners, ServiceRunner};

use super::ports::ResolvedPort;

/// What installs and runs a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Package,
    Docker,
}

impl Backend {
    pub fn of(runner: &dyn ServiceRunner) -> Self {
        match runner.label() {
            Some(_) => Self::Docker,
            None => Self::Package,
        }
    }
}

/// The address services listen on, as exported in `<SERVICE>_HOST`.
pub const SERVICE_HOST: &str = "127.0.0.1";

/// A service's backend, running state and port, as reported by `--json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceState {
    pub backend: Backend,
    pub running: bool,
    pub host: &'static str,
    pub port: Option<u16>,
    /// `explicit`, `lock`, `default` or `unassigned`; `None` when the service has no
    /// configurable port.
    pub port_source: Option<&'static str>,
}

impl ServiceState {
    pub fn new(backend: Backend, running: bool, port: Option<ResolvedPort>) -> Self {
        Self {
            backend,
            running,
            host: SERVICE_HOST,
            port: port.and_then(ResolvedPort::port),
            port_source: port.map(ResolvedPort::source),
        }
    }
}

/// One row of the dependency status table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepRow {
    /// The dependency's name as written in devy.yml.
    pub dep: String,
    /// The label shown in the table, e.g. `node@22` or `redis (docker)`.
    pub label: String,
    pub installed: bool,
    /// Whether an installed service is running; `None` for non-services and for
    /// services that are not installed.
    pub running: Option<bool>,
    pub backend: Backend,
    /// The service's resolved port; `None` for deps without a configurable port and
    /// when ports were not resolved.
    pub port: Option<ResolvedPort>,
}

impl DepRow {
    /// The problem this row represents: not installed, or a stopped service.
    pub fn issue(&self) -> Option<String> {
        if !self.installed {
            Some(format!("{}: not installed", self.label))
        } else if self.running == Some(false) {
            Some(format!("{}: service stopped", self.label))
        } else {
            None
        }
    }
}

/// Queries each dependency's install and service state. Docker-managed services are
/// labeled `(docker)`; for them, a missing image counts as not installed and a missing
/// container as stopped. Rows gathered before an error are returned alongside it.
///
/// `ports` holds one entry per dep from `ports::resolve_ports`, or is empty when the
/// caller did not resolve ports.
pub fn dep_rows(
    deps: &[Dependency],
    ports: &[Option<ResolvedPort>],
    runners: &Runners,
) -> (Vec<DepRow>, Option<anyhow::Error>) {
    let mut rows = Vec::with_capacity(deps.len());
    for (i, dep) in deps.iter().enumerate() {
        match dep_row(dep, ports.get(i).copied().flatten(), runners) {
            Ok(row) => rows.push(row),
            Err(e) => return (rows, Some(e)),
        }
    }
    (rows, None)
}

fn dep_row(dep: &Dependency, port: Option<ResolvedPort>, runners: &Runners) -> Result<DepRow> {
    let runner = runners.runner_for(dep);
    let installed = runner.is_installed(dep)?;
    let running = if installed && modules::get(&dep.name).is_service() {
        Some(runner.is_running(dep)?)
    } else {
        None
    };
    Ok(DepRow {
        dep: dep.name.clone(),
        label: service_runner::display_name(&dep.versioned_name(), runner),
        installed,
        running,
        backend: Backend::of(runner),
        port,
    })
}

/// Prints `rows` as the dependency status table and returns the number of issues.
pub fn render_dep_rows(rows: &[DepRow], bold_errors: bool) -> usize {
    let labels: Vec<String> = rows
        .iter()
        .map(|r| clean_line(&r.label).into_owned())
        .collect();
    let name_col = labels.iter().map(|l| l.len()).max().unwrap_or(0);
    const STATUS_COL: usize = "not installed".len();
    let emphasize = |s: &str| {
        if bold_errors {
            s.red().bold().to_string()
        } else {
            s.red().to_string()
        }
    };
    let mut issues = 0usize;

    for (row, label) in rows.iter().zip(&labels) {
        let (icon, status) = if row.installed {
            (
                "✓".green().bold().to_string(),
                "installed".green().to_string(),
            )
        } else {
            issues += 1;
            ("✗".red().bold().to_string(), emphasize("not installed"))
        };

        let service = match row.running {
            Some(true) => format!("{} {}", "✓".green().bold(), "running".green()),
            Some(false) => {
                issues += 1;
                format!("{} {}", "✗".red().bold(), emphasize("stopped"))
            }
            None => "–".dimmed().to_string(),
        };

        println!(
            "  {}  {:<name_col$}  {:<STATUS_COL$}  {}",
            icon, label, status, service
        );
    }

    issues
}

/// Renders the environment variable status table and returns the number of issues found.
///
/// `written_vars` is the result of reading the env manager's written config (e.g. the
/// shadowenv lisp file). Pass `None` if the file has not been written yet.
///
/// When `bold_errors = true` (check command): shows ✓/✗ icons with "configured"/"missing" labels.
/// When `bold_errors = false` (status command): shows each key with its current value.
pub fn print_env_table(
    config_env: &HashMap<String, String>,
    written_vars: Option<HashMap<String, String>>,
    bold_errors: bool,
) -> Result<usize> {
    let mut issues = 0usize;
    let mut sorted_keys: Vec<&String> = config_env.keys().collect();
    sorted_keys.sort();
    // (key, the key as printed), in sorted order.
    let rows: Vec<(&String, String)> = sorted_keys
        .iter()
        .map(|k| (*k, clean_line(k).into_owned()))
        .collect();
    let key_col = rows.iter().map(|(_, k)| k.len()).max().unwrap_or(0);

    match written_vars {
        None if bold_errors => {
            for (_, shown) in &rows {
                issues += 1;
                println!(
                    "  {}  {:<key_col$}  {}",
                    "✗".red().bold(),
                    shown,
                    "missing".red().bold()
                );
            }
        }
        None => {
            for (_, shown) in &rows {
                println!("  {:<key_col$}  {}", shown, "(not configured)".dimmed());
            }
        }
        Some(ref vars) if bold_errors => {
            for (key, shown) in &rows {
                let (icon, value): (String, String) = if vars.contains_key(*key) {
                    (
                        "✓".green().bold().to_string(),
                        "configured".green().to_string(),
                    )
                } else {
                    issues += 1;
                    (
                        "✗".red().bold().to_string(),
                        "missing".red().bold().to_string(),
                    )
                };
                println!("  {}  {:<key_col$}  {}", icon, shown, value);
            }
        }
        Some(vars) => {
            for (key, shown) in &rows {
                let value = status_value(key, vars.get(*key).map(String::as_str));
                println!("  {:<key_col$}  {}", shown, value.as_str().dimmed());
            }
        }
    }

    Ok(issues)
}

/// The value `devy status` shows for `key`: `(not set)` when missing, otherwise the value
/// redacted as `devy status --json` and AI requests redact it (`<redacted>` when the key
/// names a secret, credential-looking parts otherwise) and cleaned to one line.
fn status_value(key: &str, value: Option<&str>) -> String {
    match value {
        None => "(not set)".to_string(),
        Some(v) => redact::value(key, &clean_line(v)),
    }
}

/// Renders the PATH prepend status table. Shows ✓/✗ in check mode, plain path list in status mode.
/// Returns the number of missing entries (issues) when `bold_errors = true`.
pub fn print_path_table(
    configured: &[String],
    written: Option<Vec<String>>,
    bold_errors: bool,
) -> usize {
    let mut issues = 0usize;
    let shown = |entry: &String| clean_line(entry).into_owned();
    match written {
        None if bold_errors => {
            for entry in configured {
                issues += 1;
                println!(
                    "  {}  {}  {}",
                    "✗".red().bold(),
                    shown(entry),
                    "missing".red().bold()
                );
            }
        }
        None => {
            for entry in configured {
                println!("  {}  {}", shown(entry), "(not configured)".dimmed());
            }
        }
        Some(ref written_entries) if bold_errors => {
            let written_set: std::collections::HashSet<&str> =
                written_entries.iter().map(String::as_str).collect();
            for entry in configured {
                if written_set.contains(entry.as_str()) {
                    println!(
                        "  {}  {}  {}",
                        "✓".green().bold(),
                        shown(entry),
                        "configured".green()
                    );
                } else {
                    issues += 1;
                    println!(
                        "  {}  {}  {}",
                        "✗".red().bold(),
                        shown(entry),
                        "missing".red().bold()
                    );
                }
            }
        }
        Some(ref written_entries) => {
            let written_set: std::collections::HashSet<&str> =
                written_entries.iter().map(String::as_str).collect();
            for entry in configured {
                if written_set.contains(entry.as_str()) {
                    println!("  {}  {}", shown(entry), "✓".green());
                } else {
                    println!("  {}  {}", shown(entry), "(not set)".dimmed());
                }
            }
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;
    use crate::service_runner::package_runners;
    use serde_norway as yaml;
    use std::path::Path;

    /// Renders the rows for `deps` and returns the issue count, or the query error.
    fn rendered_issues(deps: &[Dependency], runners: &Runners) -> Result<usize> {
        let (rows, err) = dep_rows(deps, &[], runners);
        let issues = render_dep_rows(&rows, false);
        err.map_or(Ok(issues), Err)
    }

    #[test]
    fn render_dep_rows_returns_zero_when_all_installed() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let deps = vec![Dependency::simple("node"), Dependency::simple("python")];
        let issues = rendered_issues(&deps, &package_runners(&pm, Path::new("/tmp"))).unwrap();
        assert_eq!(issues, 0, "no issues when all deps are installed");
    }

    #[test]
    fn render_dep_rows_returns_nonzero_when_dep_missing() {
        // Kills `replace render_dep_rows -> Ok(0)` and `replace -> Ok(1)`.
        let pm = MockPackageManager::default(); // installed=false
        let deps = vec![Dependency::simple("node"), Dependency::simple("python")];
        let issues = rendered_issues(&deps, &package_runners(&pm, Path::new("/tmp"))).unwrap();
        assert!(issues > 0, "should count missing deps as issues");
    }

    #[test]
    fn render_dep_rows_counts_each_missing_dep() {
        // Kills `replace += with -=` — with subtraction, issues would be negative (wraps to usize::MAX).
        let pm = MockPackageManager::default();
        let deps = vec![
            Dependency::simple("node"),
            Dependency::simple("python"),
            Dependency::simple("ruby"),
        ];
        let issues = rendered_issues(&deps, &package_runners(&pm, Path::new("/tmp"))).unwrap();
        assert_eq!(issues, 3, "each missing dep must add 1 to issues");
    }

    #[test]
    fn render_dep_rows_empty_deps_returns_zero() {
        // Kills `delete ! in render_dep_rows at line 48` — with mutation, empty list would
        // enter the service-running check and panic (service running check on non-service).
        // Actually, empty deps → no iterations → issues = 0.
        let pm = MockPackageManager::default();
        let issues = rendered_issues(&[], &package_runners(&pm, Path::new("/tmp"))).unwrap();
        assert_eq!(issues, 0);
    }

    #[test]
    fn render_dep_rows_counts_stopped_service_as_issue() {
        // Kills `replace += with -=` at line 53 (service stopped counter).
        let pm = MockPackageManager {
            installed: true,
            service_running: false,
            ..Default::default()
        };
        let deps = vec![Dependency::simple("mysql")]; // mysql is a service
        let issues = rendered_issues(&deps, &package_runners(&pm, Path::new("/tmp"))).unwrap();
        assert_eq!(issues, 1, "stopped service must count as one issue");
    }

    #[test]
    fn render_dep_rows_running_service_does_not_add_issues() {
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let deps = vec![Dependency::simple("mysql")];
        let issues = rendered_issues(&deps, &package_runners(&pm, Path::new("/tmp"))).unwrap();
        assert_eq!(issues, 0, "running service must not add to issues");
    }

    #[test]
    fn render_dep_rows_uninstalled_service_counts_only_once() {
        // Kills `delete ! in render_dep_rows` at the `!installed` check for service rendering.
        // When not installed, service status shows "–" (not checked), so only 1 issue.
        let pm = MockPackageManager::default(); // installed=false, service_running=false
        let deps = vec![Dependency::simple("mysql")];
        let issues = rendered_issues(&deps, &package_runners(&pm, Path::new("/tmp"))).unwrap();
        assert_eq!(
            issues, 1,
            "uninstalled service should count as exactly 1 issue"
        );
    }

    #[test]
    fn dep_rows_report_docker_backend() {
        use crate::service_runner::docker::{ContainerRuntime, FakeRunner, ok};
        let config: crate::config::DevyConfig =
            yaml::from_str("service_manager: docker\ndependencies:\n  - redis\n  - jq\n").unwrap();
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let cli =
            FakeRunner::new(|_| ok(r#"[{"State":{"Running":false},"Config":{"Labels":{}}}]"#));
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &cli),
            &config,
            Path::new("/src/app"),
            None,
            false,
        );
        let deps = config.normalized_dependencies().unwrap();
        let (rows, err) = dep_rows(&deps, &[], &runners);
        assert!(err.is_none(), "{err:?}");
        assert_eq!(rows[0].backend, Backend::Docker);
        assert_eq!(rows[0].label, "redis (docker)");
        assert_eq!(rows[1].backend, Backend::Package);
    }

    #[test]
    fn dep_rows_report_locked_port_source() {
        use crate::commands::ports::{self, PortMode};
        use crate::lock::{LockFile, LockedDep};
        let mut locked = std::collections::BTreeMap::new();
        locked.insert(
            "redis".to_string(),
            LockedDep {
                resolved_version: None,
                source: "nix".into(),
                assigned_port: Some(52113),
                image_digest: None,
            },
        );
        let lock = LockFile {
            dependencies: locked,
            ..Default::default()
        };
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let mut deps = vec![Dependency::simple("redis"), Dependency::simple("jq")];
        let resolved = ports::resolve_ports(
            &mut deps,
            ports::PortSource::Lock(Some(&lock)),
            &pm,
            PortMode::ReadOnly,
        )
        .unwrap();
        let (rows, _) = dep_rows(&deps, &resolved, &package_runners(&pm, Path::new("/tmp")));
        assert_eq!(rows[0].port, Some(ResolvedPort::Locked(52113)));
        assert_eq!(rows[0].port.unwrap().source(), "lock");
        assert_eq!(rows[0].backend, Backend::Package);
        assert_eq!(rows[1].port, None, "jq has no port");
    }

    #[test]
    fn status_value_masks_secret_keys() {
        assert_eq!(status_value("API_TOKEN", Some("abc")), "<redacted>");
        assert_eq!(status_value("db_password", Some("hunter2")), "<redacted>");
        assert_eq!(status_value("STRIPE_SECRET_KEY", Some("sk")), "<redacted>");
        assert_eq!(status_value("LOG_LEVEL", Some("debug")), "debug");
        assert_eq!(status_value("API_TOKEN", None), "(not set)");
        assert_eq!(status_value("LOG_LEVEL", None), "(not set)");
        // Credential-looking parts are redacted under any key, as in `--json`.
        let url = status_value("DATABASE_URL", Some("postgres://app:hunter2@db/app"));
        assert!(!url.contains("hunter2"), "{url}");
        // An invisible character cannot split a credential past the patterns.
        let url = status_value(
            "DATABASE_URL",
            Some("postgres://app:\u{feff}hunter2@db/app"),
        );
        assert!(!url.contains("hunter2"), "{url}");
    }

    #[test]
    fn status_value_is_one_line() {
        assert_eq!(
            status_value("LOG_LEVEL", Some("debug\n  FAKE  row")),
            "debug   FAKE  row"
        );
    }

    #[test]
    fn status_value_cleans_control_sequences() {
        assert_eq!(
            status_value("GREETING", Some("hi\x1b]52;c;eA==\x07\x1b[2J!")),
            "hi!"
        );
    }

    #[test]
    fn print_env_table_returns_zero_for_empty_env() {
        let issues = print_env_table(&HashMap::new(), None, false).unwrap();
        assert_eq!(issues, 0);
    }

    #[test]
    fn print_env_table_check_mode_returns_count_when_env_not_configured() {
        // Kills `replace print_env_table -> Ok(0)` and `replace -> Ok(1)`.
        // With bold_errors=true (check mode) and no written vars, issues = config_env.len().
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        env.insert("BAZ".to_string(), "qux".to_string());
        let issues = print_env_table(&env, None, true).unwrap();
        assert_eq!(
            issues, 2,
            "issues must equal env count when env vars not written yet"
        );
    }

    #[test]
    fn print_env_table_status_mode_returns_zero_when_not_configured() {
        // In status mode (bold_errors=false), missing config prints a message but counts 0 issues.
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let issues = print_env_table(&env, None, false).unwrap();
        assert_eq!(issues, 0);
    }

    #[test]
    fn print_env_table_bold_errors_guard_distinguishes_modes() {
        // Kills `replace match guard bold_errors with true/false` mutations.
        let mut env = HashMap::new();
        env.insert("KEY".to_string(), "val".to_string());
        // true mode reports issues; false mode reports 0 (status mode).
        let check_issues = print_env_table(&env, None, true).unwrap();
        let status_issues = print_env_table(&env, None, false).unwrap();
        assert!(
            check_issues >= status_issues,
            "check mode issues ({check_issues}) must be >= status mode issues ({status_issues})"
        );
    }

    #[test]
    fn print_env_table_check_mode_reports_configured_when_var_present() {
        let mut env = HashMap::new();
        env.insert("MY_VAR".to_string(), "value".to_string());
        let mut written = HashMap::new();
        written.insert("MY_VAR".to_string(), "value".to_string());
        let issues = print_env_table(&env, Some(written), true).unwrap();
        assert_eq!(issues, 0, "configured var must not count as an issue");
    }

    #[test]
    fn print_env_table_check_mode_reports_missing_when_var_absent_from_written() {
        let mut env = HashMap::new();
        env.insert("MY_VAR".to_string(), "value".to_string());
        let written = HashMap::new(); // empty — var not written
        let issues = print_env_table(&env, Some(written), true).unwrap();
        assert_eq!(issues, 1, "missing var must count as an issue");
    }
}
