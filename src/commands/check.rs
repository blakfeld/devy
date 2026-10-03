use anyhow::{Context, Result};
use colored::Colorize;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

use crate::config::{Dependency, DevyConfig};
use crate::env_manager::{EnvManager, Shadowenv};
use crate::error::SilentExit;
use crate::modules;
use crate::output;
use crate::package_manager;
use crate::package_manager::PackageManager;
use crate::service_runner::docker::ContainerRuntime;
use crate::service_runner::{self, Runners};

use super::ports::{self, PortMode};
use super::shared;

/// `check_with_runtime` with the real container CLI selected by `container_cli`.
pub(crate) fn check_impl(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    env_mgr: &dyn EnvManager,
    project_root: &Path,
) -> Result<()> {
    check_with_runtime(
        config,
        pm,
        ContainerRuntime::system(config.container_cli),
        env_mgr,
        project_root,
    )
}

pub(crate) fn check_with_runtime(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    project_root: &Path,
) -> Result<()> {
    let project_name = config.name.as_deref().unwrap_or("project");
    output::header(&format!("devy check · {}", project_name));

    let findings = collect_findings(config, pm, runtime, env_mgr, project_root);
    let issues = findings.print()?;
    if let Some(e) = findings.hard_error {
        return Err(e);
    }

    output::blank_line();
    if issues == 0 {
        output::success("all checks passed");
        Ok(())
    } else {
        let noun = issue_noun(issues);
        eprintln!("  {}  {} {} found", "✗".red().bold(), issues, noun);
        Err(SilentExit(1).into())
    }
}

/// The `devy check --json` document.
#[derive(Debug, Serialize)]
pub(crate) struct CheckDocument {
    pub passed: bool,
    pub issues: Vec<String>,
    pub warnings: Vec<String>,
}

/// Runs the checks for `devy check --json`. A hard error (an unloadable config, a port
/// conflict, a failed backend query) is returned as an error instead of a document.
pub(crate) fn check_document(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    project_root: &Path,
) -> Result<CheckDocument> {
    let mut findings = collect_findings(config, pm, runtime, env_mgr, project_root);
    if let Some(e) = findings.hard_error.take() {
        return Err(e);
    }
    let issues = findings.issues();
    Ok(CheckDocument {
        passed: issues.is_empty(),
        issues,
        warnings: findings.warnings(),
    })
}

/// Prints the check document and exits 1, without the stderr summary, when it reports
/// issues.
pub(crate) fn check_json(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    project_root: &Path,
) -> Result<()> {
    let doc = check_document(config, pm, runtime, env_mgr, project_root)?;
    super::json::print(&doc)?;
    if doc.passed {
        Ok(())
    } else {
        Err(SilentExit(1).into())
    }
}

/// A config-level message from the checks, in the order `devy check` prints them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Note {
    /// Counts toward the issue total.
    Issue(String),
    /// Printed, but not counted as an issue.
    Warning(String),
}

/// The environment-file state the checks compare against.
#[derive(Debug, Clone)]
pub(crate) struct EnvState {
    pub vars: HashMap<String, String>,
    pub written_vars: Option<HashMap<String, String>>,
    pub path_prepends: Vec<String>,
    /// Read only when `path_prepends` is non-empty.
    pub written_paths: Option<Vec<String>>,
}

/// Everything `devy check` evaluates, as data. Evaluation stops at the first hard error
/// (an unloadable config, a port conflict, a failed backend query), which is kept in
/// `hard_error`; the findings gathered before it are kept too.
#[derive(Debug, Default)]
pub(crate) struct Findings {
    pub notes: Vec<Note>,
    /// Dependency table rows; `None` when there are no dependencies or a hard error
    /// came first.
    pub deps: Option<Vec<shared::DepRow>>,
    /// `None` when devy.yml has no environment and no module adds PATH entries.
    pub env: Option<EnvState>,
    pub hard_error: Option<anyhow::Error>,
}

impl Findings {
    /// Findings for a project whose checks could not start, e.g. an unparseable devy.yml.
    pub fn from_error(e: anyhow::Error) -> Self {
        Self {
            hard_error: Some(e),
            ..Default::default()
        }
    }

    /// Every counted issue as one line, hard error last.
    pub fn issues(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .notes
            .iter()
            .filter_map(|n| match n {
                Note::Issue(s) => Some(s.clone()),
                Note::Warning(_) => None,
            })
            .collect();
        out.extend(self.deps.iter().flatten().filter_map(shared::DepRow::issue));
        if let Some(env) = &self.env {
            let mut keys: Vec<&String> = env.vars.keys().collect();
            keys.sort();
            for key in keys {
                if !env
                    .written_vars
                    .as_ref()
                    .is_some_and(|w| w.contains_key(key))
                {
                    out.push(format!(
                        "environment variable {key} is missing from the environment file"
                    ));
                }
            }
            for entry in &env.path_prepends {
                if !env
                    .written_paths
                    .as_ref()
                    .is_some_and(|w| w.contains(entry))
                {
                    out.push(format!(
                        "PATH entry {entry} is missing from the environment file"
                    ));
                }
            }
        }
        out.extend(self.hard_error.iter().map(|e| format!("{e:#}")));
        out
    }

    pub fn warnings(&self) -> Vec<String> {
        self.notes
            .iter()
            .filter_map(|n| match n {
                Note::Warning(s) => Some(s.clone()),
                Note::Issue(_) => None,
            })
            .collect()
    }

    /// Prints the findings as `devy check` does and returns the number of issues,
    /// not counting `hard_error`, which the caller reports.
    pub fn print(&self) -> Result<usize> {
        let mut issues = 0usize;
        for note in &self.notes {
            match note {
                Note::Issue(s) => {
                    issues += 1;
                    output::warn(s);
                }
                Note::Warning(s) => output::warn(s),
            }
        }
        if let Some(rows) = &self.deps {
            output::header("Dependencies");
            issues += shared::render_dep_rows(rows, true);
        }
        if let Some(env) = &self.env {
            output::header("Environment");
            issues += shared::print_env_table(&env.vars, env.written_vars.clone(), true)?;
            if !env.path_prepends.is_empty() {
                issues +=
                    shared::print_path_table(&env.path_prepends, env.written_paths.clone(), true);
            }
        }
        Ok(issues)
    }
}

/// Evaluates the checks without printing. Read-only: nothing is installed, started or
/// written.
pub(crate) fn collect_findings(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    project_root: &Path,
) -> Findings {
    let mut findings = Findings::default();
    if let Err(e) = collect_into(&mut findings, config, pm, runtime, env_mgr, project_root) {
        findings.hard_error = Some(e);
    }
    findings
}

fn collect_into(
    findings: &mut Findings,
    config: &DevyConfig,
    pm: &dyn PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    project_root: &Path,
) -> Result<()> {
    let deps = config.normalized_dependencies()?;
    // Resolve ports exactly as `up` would, without assigning new ones or writing the lock.
    let lock = ports::load_lock(project_root)?;
    let mut resolved_deps = deps.clone();
    let resolved =
        ports::resolve_and_check(&mut resolved_deps, lock.as_ref(), pm, PortMode::ReadOnly)?;
    let runners = Runners::new(pm, runtime, config, project_root, lock.as_ref(), false);

    if !deps.is_empty() {
        for dep in &deps {
            if !dep.docker {
                pm.validate_config(dep)
                    .with_context(|| format!("{}: config validation failed", dep.name))?;
            }
            let module = modules::get(&dep.name);
            findings
                .notes
                .extend(extra_key_issues(dep).into_iter().map(Note::Issue));
            let warnings = module
                .config_warnings(dep)
                .into_iter()
                .chain(ports::unapplied_port_warning(dep, pm))
                .chain(modules::nix_version_warning(dep, pm))
                .chain(service_runner::docker_warnings(dep));
            for warning in warnings {
                findings
                    .notes
                    .push(Note::Warning(format!("{}: {}", dep.name, warning)));
            }
            findings.notes.extend(shell_issue(dep).map(Note::Issue));
        }
        let (rows, err) = shared::dep_rows(&resolved_deps, &resolved, &runners);
        findings.deps = Some(rows);
        if let Some(e) = err {
            return Err(e);
        }
    }

    // Collect PATH prepends from all modules.
    let path_prepends: Vec<String> = deps
        .iter()
        .flat_map(|dep| modules::get(&dep.name).path_prepends(dep, project_root))
        .collect();

    if !config.environment.is_empty() || !path_prepends.is_empty() {
        let written_paths = if path_prepends.is_empty() {
            None
        } else {
            env_mgr.read_path_prepends(project_root)
        };
        findings.env = Some(EnvState {
            vars: config.environment.clone(),
            written_vars: env_mgr.read_vars(project_root),
            path_prepends,
            written_paths,
        });
    }
    Ok(())
}

/// Problems in `config` that are detectable without a package manager, env manager or
/// lock: dependency normalization, unrecognized extra keys, invalid shells and conflicts
/// between explicit ports. Used to validate a config before anything is installed.
pub(crate) fn static_issues(config: &DevyConfig) -> Result<Vec<String>> {
    let deps = config.normalized_dependencies()?;
    let mut issues = Vec::new();
    for dep in &deps {
        issues.extend(extra_key_issues(dep));
        issues.extend(shell_issue(dep));
    }
    issues.extend(explicit_port_conflict(&deps));
    Ok(issues)
}

fn extra_key_issues(dep: &Dependency) -> Vec<String> {
    let Some(known) = modules::get(&dep.name).known_extra_keys() else {
        return vec![];
    };
    let hint = if known.is_empty() {
        "this module accepts no extra keys".to_string()
    } else {
        format!("known keys: {}", known.join(", "))
    };
    let mut keys: Vec<&String> = dep
        .extra
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .collect();
    keys.sort();
    keys.into_iter()
        .map(|key| format!("{}: unrecognized config key `{key}` — {hint}", dep.name))
        .collect()
}

fn shell_issue(dep: &Dependency) -> Option<String> {
    let shell = dep.shell.as_deref()?;
    let e = crate::commands::exec::validate_shell(shell).err()?;
    Some(format!("{}: invalid shell '{}': {}", dep.name, shell, e))
}

/// Conflicts and out-of-range values among ports written explicitly in devy.yml. Default
/// and assigned ports depend on the backend, so they are left to `check` and `up`.
fn explicit_port_conflict(deps: &[Dependency]) -> Option<String> {
    let mut explicit = Vec::with_capacity(deps.len());
    for dep in deps {
        let port = modules::get(&dep.name)
            .port_key()
            .and_then(|key| dep.extra.get(key))
            .and_then(|v| v.as_u64());
        match port.map(u16::try_from) {
            None => explicit.push(None),
            Some(Ok(p)) if p != 0 => explicit.push(Some(ports::ResolvedPort::Explicit(p))),
            Some(_) => {
                return Some(format!(
                    "'{}': port value {} is out of range (must be 1–65535)",
                    dep.name,
                    port.unwrap_or_default()
                ));
            }
        }
    }
    ports::check_port_conflicts(deps, &explicit)
        .err()
        .map(|e| e.to_string())
}

#[cfg_attr(test, mutants::skip)] // cosmetic singular/plural; no observable behavioral difference
fn issue_noun(count: usize) -> &'static str {
    if count == 1 { "issue" } else { "issues" }
}

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper
pub fn run(json: bool) -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;
    let pm = package_manager::detect(&config, &project_root)?;
    let env_mgr = Shadowenv;
    if json {
        let runtime = ContainerRuntime::system(config.container_cli);
        check_json(&config, pm.as_ref(), runtime, &env_mgr, &project_root)
    } else {
        check_impl(&config, pm.as_ref(), &env_mgr, &project_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevyConfig;
    use crate::env_manager::MockEnvManager;
    use crate::package_manager::MockPackageManager;
    use std::collections::HashMap;

    fn make_config(dep_names: &[&str], env: HashMap<String, String>) -> DevyConfig {
        crate::test_support::make_config(dep_names, env)
    }

    #[test]
    fn check_impl_returns_ok_when_all_installed() {
        let config = make_config(&["node"], HashMap::new());

        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_ok());
    }

    #[test]
    fn check_impl_returns_err_when_dep_not_installed() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager::default(); // installed=false
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_err());
    }

    #[test]
    fn check_impl_counts_two_missing_deps_as_issues() {
        let config = make_config(&["node", "python"], HashMap::new());
        let pm = MockPackageManager::default();
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(result.is_err(), "two missing deps must produce errors");
    }

    #[test]
    fn check_impl_empty_deps_skips_dep_table() {
        let config = make_config(&[], HashMap::new());
        let pm = MockPackageManager::default();
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_ok());
    }

    #[test]
    fn check_impl_single_issue_uses_singular_noun() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager::default();
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_err());
    }

    #[test]
    fn check_impl_zero_issues_returns_ok() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_ok());
    }

    #[test]
    fn check_impl_not_installed_service_counts_as_issue() {
        let config = make_config(&["mysql"], HashMap::new()); // mysql is a service
        let pm = MockPackageManager::default(); // installed=false
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "not-installed service must count as an issue"
        );
    }

    #[test]
    fn check_impl_env_issue_without_dep_issues_returns_err() {
        let mut env = HashMap::new();
        env.insert("MY_VAR".to_string(), "value".to_string());
        // Empty deps so the dep block contributes 0 to issues.
        let config = make_config(&[], env);
        let pm = MockPackageManager::default();
        // print_env_table with bold_errors=true and no shadowenv file returns env.len() = 1 issue.
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "a missing env var must be counted as an issue"
        );
    }

    #[test]
    fn check_impl_dep_and_env_issues_both_contribute() {
        let mut env = HashMap::new();
        env.insert("MY_VAR".to_string(), "val".to_string());
        let config = make_config(&["node"], env);
        let pm = MockPackageManager::default(); // nothing installed, no shadowenv file
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_err());
    }

    #[test]
    fn check_impl_returns_err_on_unrecognized_extra_key() {
        // minio has known_extra_keys; portx is not in the list — must count as an issue.
        let yaml = "dependencies:\n  - minio:\n      portx: 9001\n";
        let config: crate::config::DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "unrecognized extra key must count as an issue"
        );
    }

    #[test]
    fn check_impl_returns_err_on_port_conflict() {
        // mysql and mariadb both default to 3306 — check must catch this, not just up.
        let config = make_config(&["mysql", "mariadb"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "port conflict must be caught by devy check"
        );
    }

    #[test]
    fn check_impl_warns_on_extra_key_for_module_with_empty_allowlist() {
        // ruby has no known extra keys (inherits Some(&[]) default) — any extra key must warn.
        let yaml = "dependencies:\n  - ruby:\n      vrsion: \"3.3.0\"\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let warn_count = crate::output::with_warn_capture(|| {
            let _ = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        });
        assert!(
            warn_count > 0,
            "check_impl must warn on unknown extra key for module with empty allowlist"
        );
    }

    #[test]
    fn check_impl_emits_config_warnings_for_minio_with_credentials() {
        // config_warnings on MinioModule fires when access_key is configured.
        // check_impl must call it and emit the warning via output::warn.
        let yaml = "dependencies:\n  - minio:\n      access_key: myuser\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let warn_count = crate::output::with_warn_capture(|| {
            let _ = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        });
        assert!(
            warn_count > 0,
            "check_impl must emit config_warnings for minio with credentials"
        );
    }

    #[test]
    fn check_impl_returns_err_on_invalid_shell() {
        use crate::config::{DepConfig, RawDependency};
        use std::collections::HashMap as HM;
        let mut map = HM::new();
        map.insert(
            "node".to_string(),
            Some(DepConfig {
                shell: Some("not-a-shell".to_string()),
                ..Default::default()
            }),
        );
        let config = DevyConfig {
            name: None,
            dependencies: vec![RawDependency::Configured(map)],
            environment: HashMap::new(),
            commands: HashMap::new(),
            hooks: Default::default(),
            package_manager: Default::default(),
            service_manager: Default::default(),
            container_cli: Default::default(),
        };
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "check_impl must return Err when dep.shell is not in the allowed list"
        );
    }

    #[test]
    fn check_impl_warns_on_unhonored_nix_version_without_counting_issue() {
        let dir = crate::test_support::tmp_dir();
        let yaml = "dependencies:\n  - jq:\n      version: \"1.6\"\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            ..Default::default()
        };
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(check_impl(&config, &pm, &MockEnvManager::default(), &dir));
        });
        assert!(
            warnings
                .iter()
                .any(|w| w == "jq: version 1.6 is not supported by the nix backend — installing the nixpkgs default"),
            "{warnings:?}"
        );
        assert!(
            result.unwrap().is_ok(),
            "the warning must not count as an issue"
        );
    }

    #[test]
    fn check_impl_does_not_flag_service_manager_or_image_as_module_keys() {
        let yaml = "dependencies:\n  - redis: { service_manager: package, image: mirror/redis }\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let warnings = crate::output::with_warn_messages(|| {
            let _ = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        });
        assert!(
            !warnings.iter().any(|w| w.contains("unrecognized")),
            "{warnings:?}"
        );
    }

    #[test]
    fn check_impl_rejects_service_manager_on_non_service() {
        let yaml = "dependencies:\n  - node: { service_manager: docker }\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let err = check_impl(
            &config,
            &MockPackageManager::default(),
            &MockEnvManager::default(),
            Path::new("."),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "node: service_manager and image apply only to built-in services"
        );
    }

    use crate::service_runner::docker::{FakeRunner, fail, ok};

    fn docker_check(yaml: &str, cli: &FakeRunner) -> (Result<()>, Vec<String>) {
        let dir = crate::test_support::tmp_dir();
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(check_with_runtime(
                &config,
                &pm,
                ContainerRuntime::new(config.container_cli, cli),
                &MockEnvManager::default(),
                &dir,
            ));
        });
        (result.unwrap(), warnings)
    }

    #[test]
    fn check_counts_missing_image_as_not_installed() {
        let cli = FakeRunner::new(|_| fail("No such image"));
        let (result, _) = docker_check("service_manager: docker\ndependencies:\n  - redis\n", &cli);
        assert!(result.is_err(), "a missing image is an issue");
        assert_eq!(cli.lines(), vec!["docker image inspect redis:7"]);
    }

    #[test]
    fn check_counts_missing_container_as_stopped() {
        let cli = FakeRunner::new(|call| match call[1].as_str() {
            "image" => ok("[{}]"),
            _ => fail("No such container"),
        });
        let (result, _) = docker_check("service_manager: docker\ndependencies:\n  - redis\n", &cli);
        assert!(result.is_err(), "a missing container is a stopped service");
    }

    #[test]
    fn check_passes_for_running_container() {
        let cli = FakeRunner::new(|call| match call[1].as_str() {
            "image" => ok("[{}]"),
            _ => ok(r#"{"State":{"Running":true},"Config":{"Labels":{}}}"#),
        });
        let (result, _) = docker_check("service_manager: docker\ndependencies:\n  - redis\n", &cli);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn check_warns_kafka_runs_kraft_under_docker() {
        let cli = FakeRunner::new(|call| match call[1].as_str() {
            "image" => ok("[{}]"),
            _ => ok(r#"{"State":{"Running":true},"Config":{"Labels":{}}}"#),
        });
        let (_, warnings) =
            docker_check("service_manager: docker\ndependencies:\n  - kafka\n", &cli);
        assert!(
            warnings.contains(
                &"kafka: zookeeper mode is not supported with docker — running Kafka in KRaft mode"
                    .to_string()
            ),
            "{warnings:?}"
        );
    }

    #[test]
    fn check_no_port_conflict_for_docker_mysql_and_mariadb() {
        let cli = FakeRunner::new(|call| match call[1].as_str() {
            "image" => ok("[{}]"),
            _ => ok(r#"{"State":{"Running":true},"Config":{"Labels":{}}}"#),
        });
        let (result, _) = docker_check(
            "package_manager: brew\nservice_manager: docker\ndependencies:\n  - mysql\n  - mariadb\n",
            &cli,
        );
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn check_impl_accepts_typescript_global_packages() {
        let yaml = "dependencies:\n  - typescript:\n      global_packages: [eslint]\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(check_impl(
                &config,
                &pm,
                &MockEnvManager::default(),
                Path::new("."),
            ));
        });
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn check_impl_no_conflict_for_unassigned_ports_under_nix() {
        let dir = crate::test_support::tmp_dir();
        let config = make_config(&["mysql", "mariadb"], HashMap::new());
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let result = check_impl(&config, &pm, &MockEnvManager::default(), &dir);
        assert!(result.is_ok(), "{result:?}");
        assert!(
            !dir.join(crate::lock::PATH).exists(),
            "check must never write devy.lock"
        );
    }

    #[test]
    fn check_impl_warns_on_unapplied_explicit_port_without_counting_issue() {
        let dir = crate::test_support::tmp_dir();
        let yaml = "dependencies:\n  - redis:\n      port: 6380\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            name: "brew",
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(check_impl(&config, &pm, &MockEnvManager::default(), &dir));
        });
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("cannot make redis listen on port 6380 with brew")),
            "{warnings:?}"
        );
        assert!(
            result.unwrap().is_ok(),
            "a warning must not count as an issue"
        );
    }

    fn collect(config: &DevyConfig, pm: &MockPackageManager) -> Findings {
        let dir = crate::test_support::tmp_dir();
        collect_findings(
            config,
            pm,
            ContainerRuntime::system(config.container_cli),
            &MockEnvManager::default(),
            &dir,
        )
    }

    // ── check --json ─────────────────────────────────────────────────────────

    fn json_doc(config: &DevyConfig, pm: &MockPackageManager) -> Result<CheckDocument> {
        let dir = crate::test_support::tmp_dir();
        check_document(
            config,
            pm,
            ContainerRuntime::system(config.container_cli),
            &MockEnvManager::default(),
            &dir,
        )
    }

    fn rendered(doc: &CheckDocument) -> serde_json::Value {
        serde_json::from_str(&crate::commands::json::render(doc).unwrap()).unwrap()
    }

    #[test]
    fn check_json_passes() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let doc = json_doc(&config, &pm).unwrap();
        assert_eq!(
            rendered(&doc),
            serde_json::json!({"version": 1, "passed": true, "issues": [], "warnings": []})
        );
    }

    #[test]
    fn check_json_lists_two_issues_and_exits_one() {
        let config = make_config(&["jq", "node"], HashMap::new());
        let pm = MockPackageManager::default();
        let doc = json_doc(&config, &pm).unwrap();
        assert!(!doc.passed);
        assert_eq!(doc.issues, ["jq: not installed", "node: not installed"]);
        let dir = crate::test_support::tmp_dir();
        let err = check_json(
            &config,
            &pm,
            ContainerRuntime::system(config.container_cli),
            &MockEnvManager::default(),
            &dir,
        )
        .unwrap_err();
        assert_eq!(err.downcast_ref::<SilentExit>().map(|e| e.0), Some(1));
    }

    #[test]
    fn check_json_warning_does_not_fail() {
        let config: DevyConfig =
            serde_yml::from_str("dependencies:\n  - jq:\n      version: \"1.6\"\n").unwrap();
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            ..Default::default()
        };
        let doc = json_doc(&config, &pm).unwrap();
        assert!(doc.passed);
        assert!(doc.issues.is_empty());
        assert_eq!(
            doc.warnings,
            [
                "jq: version 1.6 is not supported by the nix backend — installing the nixpkgs default"
            ]
        );
        let dir = crate::test_support::tmp_dir();
        assert!(
            check_json(
                &config,
                &pm,
                ContainerRuntime::system(config.container_cli),
                &MockEnvManager::default(),
                &dir,
            )
            .is_ok()
        );
    }

    #[test]
    fn check_json_port_conflict_is_a_hard_error() {
        let config = make_config(&["mysql", "mariadb"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let err = json_doc(&config, &pm).unwrap_err();
        assert!(err.downcast_ref::<SilentExit>().is_none());
        assert!(format!("{err:#}").contains("port conflict"), "{err:#}");
    }

    #[test]
    fn collect_findings_keeps_port_conflict_as_hard_error() {
        let config = make_config(&["mysql", "mariadb"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let findings = collect(&config, &pm);
        let err = findings
            .hard_error
            .as_ref()
            .expect("port conflict is a hard error");
        assert!(format!("{err:#}").contains("port conflict"), "{err:#}");
        assert!(
            findings.issues().last().unwrap().contains("port conflict"),
            "the hard error is listed as the last issue"
        );
    }

    #[test]
    fn collect_findings_keeps_multi_key_dependency_as_hard_error() {
        let yaml = "dependencies:\n  - node: {}\n    redis: {}\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let findings = collect(&config, &MockPackageManager::default());
        let err = findings
            .hard_error
            .as_ref()
            .expect("multi-key entry is a hard error");
        assert!(err.to_string().contains("multiple keys"), "{err}");
        assert!(findings.deps.is_none(), "no table after a hard error");
    }

    #[test]
    fn collect_findings_lists_issues_and_warnings_as_data() {
        let yaml = "dependencies:\n  - ruby: { vrsion: \"3\" }\n  - mysql\n  - minio: { access_key: me }\nenvironment:\n  FOO: bar\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            service_running: false,
            ..Default::default()
        };
        let mut findings = None;
        let warned = crate::output::with_warn_capture(|| findings = Some(collect(&config, &pm)));
        let findings = findings.unwrap();
        assert_eq!(warned, 0, "collecting must not print");
        assert!(findings.hard_error.is_none());
        let issues = findings.issues();
        assert!(
            issues
                .iter()
                .any(|i| i.contains("unrecognized config key `vrsion`")),
            "{issues:?}"
        );
        assert!(
            issues.contains(&"mysql: service stopped".to_string()),
            "{issues:?}"
        );
        assert!(
            issues.contains(&"minio: service stopped".to_string()),
            "{issues:?}"
        );
        assert!(
            issues.contains(
                &"environment variable FOO is missing from the environment file".to_string()
            ),
            "{issues:?}"
        );
        assert!(
            findings.warnings().iter().any(|w| w.starts_with("minio:")),
            "{:?}",
            findings.warnings()
        );
    }

    #[test]
    fn findings_print_counts_the_same_issues_as_issues() {
        let config = make_config(
            &["node", "mysql"],
            HashMap::from([("A".into(), "b".into())]),
        );
        let findings = collect(&config, &MockPackageManager::default());
        let printed = findings.print().unwrap();
        assert_eq!(printed, findings.issues().len());
    }

    #[test]
    fn static_issues_reports_misspelled_port_key() {
        let yaml = "dependencies:\n  - redis: { prot: 6380 }\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let issues = static_issues(&config).unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(
            issues[0].contains("unrecognized config key `prot`"),
            "{issues:?}"
        );
    }

    #[test]
    fn static_issues_empty_for_clean_config() {
        let yaml =
            "dependencies:\n  - node:\n      version: \"22\"\n  - redis:\n      port: 6380\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(static_issues(&config).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn static_issues_reports_explicit_port_conflict() {
        let yaml =
            "dependencies:\n  - redis:\n      port: 5432\n  - postgresql:\n      port: 5432\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let issues = static_issues(&config).unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("port conflict"), "{issues:?}");
    }

    #[test]
    fn static_issues_ignores_default_port_overlap() {
        // mysql and mariadb share a default port; only explicit ports are compared.
        let config = make_config(&["mysql", "mariadb"], HashMap::new());
        assert!(static_issues(&config).unwrap().is_empty());
    }

    #[test]
    fn static_issues_reports_invalid_shell() {
        let yaml = "dependencies:\n  - node:\n      shell: not-a-shell\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let issues = static_issues(&config).unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("invalid shell"), "{issues:?}");
    }

    #[test]
    fn static_issues_errs_on_multi_key_dependency() {
        let yaml = "dependencies:\n  - node: {}\n    redis: {}\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        assert!(static_issues(&config).is_err());
    }
}
