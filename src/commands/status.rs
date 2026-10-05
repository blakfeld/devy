use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

use crate::ai::redact;
use crate::config::{Dependency, DevyCommand, DevyConfig};
use crate::env_manager::{EnvManager, Shadowenv};
use crate::modules;
use crate::output;
use crate::package_manager;
use crate::package_manager::PackageManager;
use crate::project_env;
use crate::service_runner::Runners;
use crate::service_runner::docker::ContainerRuntime;

use super::ports::{self, PortMode};
use super::shared::{self, DepRow, ServiceState};

/// Everything `devy status` reports, gathered without printing or writing anything.
pub(crate) struct StatusReport {
    pub project: Option<String>,
    pub package_manager: String,
    /// Dependencies as declared, with ports resolved read-only.
    pub deps: Vec<Dependency>,
    /// One row per dependency, up to `error`.
    pub rows: Vec<DepRow>,
    /// devy.yml `environment`.
    pub environment: HashMap<String, String>,
    /// The variables in the environment file; `None` when it does not exist.
    pub written_vars: Option<HashMap<String, String>>,
    /// Module PATH entries, in order, as the text table shows them.
    pub path_prepends: Vec<String>,
    /// Every PATH entry `devy up` writes, the package manager's first, as `--json` and
    /// `devy exec` see them.
    pub env_path: Vec<String>,
    pub written_paths: Option<Vec<String>>,
    /// Project commands sorted by name.
    pub commands: Vec<CommandInfo>,
    /// A failed state query. The rows gathered before it are kept and nothing after it
    /// is read.
    pub error: Option<anyhow::Error>,
    /// Set when the project is in a linked git worktree.
    pub worktree: Option<crate::worktree::LinkedWorktree>,
}

/// A project command from devy.yml.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CommandInfo {
    pub name: String,
    pub cmd: String,
    pub shell: String,
}

/// Gathers the status report. Ports are resolved read-only; nothing is written.
pub(crate) fn status_report(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    project_root: &std::path::Path,
) -> Result<StatusReport> {
    let mut deps = config.normalized_dependencies()?;
    // Resolve ports as `up` would so service probes see the locked port; never writes.
    let recorded = ports::RecordedPorts::load(project_root)?;
    let resolved = ports::resolve_ports(&mut deps, recorded.source(), pm, PortMode::ReadOnly)?;

    let mut commands: Vec<CommandInfo> = config
        .commands
        .iter()
        .map(|(name, raw)| {
            let cmd = DevyCommand::from(raw.clone());
            CommandInfo {
                name: name.clone(),
                cmd: cmd.cmd,
                shell: cmd.shell,
            }
        })
        .collect();
    commands.sort_by(|a, b| a.name.cmp(&b.name));

    let runners = Runners::new(pm, runtime, config, project_root, recorded.lock(), false);
    let (rows, error) = shared::dep_rows(&deps, &resolved, &runners);

    let mut report = StatusReport {
        project: config.name.clone(),
        package_manager: pm.name().to_string(),
        rows,
        environment: config.environment.clone(),
        written_vars: None,
        path_prepends: vec![],
        env_path: vec![],
        written_paths: None,
        commands,
        error,
        deps,
        worktree: recorded.worktree().cloned(),
    };
    if report.error.is_none() {
        report.path_prepends = report
            .deps
            .iter()
            .flat_map(|dep| modules::get(&dep.name).path_prepends(dep, project_root))
            .collect();
        report.env_path =
            project_env::resolve(config, &report.deps, pm, project_root, PortMode::ReadOnly)
                .path_prepends;
        report.written_vars = env_mgr.read_vars(project_root);
        report.written_paths = env_mgr.read_path_prepends(project_root);
    }
    Ok(report)
}

/// The line `devy status` prints under its header in a linked worktree.
fn worktree_line(worktree: &crate::worktree::LinkedWorktree) -> String {
    match (&worktree.main_project_root, &worktree.main_checkout) {
        (Some(main), _) => format!("worktree of {}", display_path(main)),
        (None, Some(main)) => format!(
            "worktree of {} (main project directory not read)",
            display_path(main)
        ),
        (None, None) => "worktree (no main checkout)".to_string(),
    }
}

/// `path` as users write it: without the `\\?\` verbatim prefix Windows' `canonicalize`
/// adds (`\\?\UNC\host\share` becomes `\\host\share`).
fn display_path(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    if let Some(rest) = shown.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = shown.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        shown
    }
}

/// Prints the report as the `devy status` tables. Returns the state-query error, if any,
/// after printing the rows gathered before it.
fn print_text(report: StatusReport) -> Result<()> {
    let project_name = report.project.as_deref().unwrap_or("project");
    output::header(&format!("devy status · {}", project_name));
    if let Some(line) = report.worktree.as_ref().map(worktree_line) {
        println!("{}", output::clean_line(&line));
    }

    if !report.deps.is_empty() {
        output::header("Dependencies");
        shared::render_dep_rows(&report.rows, false);
    }
    if let Some(e) = report.error {
        return Err(e);
    }

    if !report.environment.is_empty() || !report.path_prepends.is_empty() {
        output::header("Environment");
        shared::print_env_table(&report.environment, report.written_vars, false)?;
        if !report.path_prepends.is_empty() {
            shared::print_path_table(&report.path_prepends, report.written_paths, false);
        }
    }

    output::blank_line();
    Ok(())
}

#[derive(Serialize)]
struct StatusDocument<'a> {
    project: Option<&'a str>,
    package_manager: &'a str,
    dependencies: Vec<DependencyStatus<'a>>,
    environment: BTreeMap<&'a str, Option<String>>,
    environment_written: bool,
    path: Vec<PathEntry<'a>>,
    commands: Vec<CommandInfo>,
}

#[derive(Serialize)]
struct DependencyStatus<'a> {
    name: &'a str,
    installed: bool,
    version: Option<&'a str>,
    service: bool,
    #[serde(flatten)]
    state: Option<ServiceState>,
}

#[derive(Serialize)]
struct PathEntry<'a> {
    entry: &'a str,
    written: bool,
}

impl StatusReport {
    /// The `--json` document. Environment values are redacted as for AI requests.
    fn document(&self) -> StatusDocument<'_> {
        let dependencies = self
            .deps
            .iter()
            .zip(&self.rows)
            .map(|(dep, row)| {
                let service = modules::get(&dep.name).is_service();
                DependencyStatus {
                    name: &row.dep,
                    installed: row.installed,
                    version: dep.version.as_deref(),
                    service,
                    state: service.then(|| {
                        ServiceState::new(row.backend, row.running == Some(true), row.port)
                    }),
                }
            })
            .collect();
        let environment = self
            .environment
            .keys()
            .map(|key| {
                let written = self
                    .written_vars
                    .as_ref()
                    .and_then(|vars| vars.get(key))
                    // Cleaned before redaction so an invisible character cannot split a
                    // credential past the patterns; controls and invisible characters are
                    // therefore not reported in the JSON value.
                    .map(|value| redact::value(key, &crate::output::clean(value)));
                (key.as_str(), written)
            })
            .collect();
        let path = self
            .env_path
            .iter()
            .map(|entry| PathEntry {
                entry,
                written: self
                    .written_paths
                    .as_ref()
                    .is_some_and(|written| written.contains(entry)),
            })
            .collect();
        StatusDocument {
            project: self.project.as_deref(),
            package_manager: &self.package_manager,
            dependencies,
            environment,
            environment_written: self.written_vars.is_some(),
            path,
            // Redacted like environment values: a command line can carry a token
            // (`curl -H "Authorization: …"`, `mysql -pSECRET`).
            commands: self
                .commands
                .iter()
                .map(|c| CommandInfo {
                    cmd: redact::text(&crate::output::clean(&c.cmd)),
                    ..c.clone()
                })
                .collect(),
        }
    }
}

pub(crate) fn status_impl(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    project_root: &std::path::Path,
    json: bool,
) -> Result<()> {
    let mut report = status_report(config, pm, runtime, env_mgr, project_root)?;
    if !json {
        return print_text(report);
    }
    if let Some(e) = report.error.take() {
        return Err(e);
    }
    super::json::print(&report.document())
}

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — requires a real devy.yml and package manager
pub fn run(json: bool) -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;
    let pm = package_manager::detect(&config, &project_root)?;
    let env_mgr = Shadowenv;
    status_impl(
        &config,
        pm.as_ref(),
        ContainerRuntime::system(config.container_cli),
        &env_mgr,
        &project_root,
        json,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_manager::MockEnvManager;
    use crate::package_manager::MockPackageManager;
    use crate::service_runner::docker::{FakeRunner, fail, ok};
    use serde_norway as yaml;
    use std::collections::HashMap;

    #[test]
    fn status_impl_never_writes_lock() {
        let dir = crate::test_support::tmp_dir();
        let config = crate::test_support::make_config(&["redis", "mysql"], HashMap::new());
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            ..Default::default()
        };
        let fake = FakeRunner::ok();
        status_impl(
            &config,
            &pm,
            ContainerRuntime::new(config.container_cli, &fake),
            &MockEnvManager::default(),
            &dir,
            false,
        )
        .unwrap();
        assert!(!dir.join(crate::lock::PATH).exists());
        assert!(fake.lines().is_empty(), "no docker-managed services");
    }

    #[test]
    fn status_impl_probes_docker_services_through_the_container_cli() {
        let dir = crate::test_support::tmp_dir();
        let config: DevyConfig =
            yaml::from_str("service_manager: docker\ndependencies:\n  - redis\n  - jq\n").unwrap();
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let fake = FakeRunner::new(|call| match call[1].as_str() {
            "image" => ok("[]"),
            _ => fail("No such container"),
        });
        status_impl(
            &config,
            &pm,
            ContainerRuntime::new(config.container_cli, &fake),
            &MockEnvManager::default(),
            &dir,
            false,
        )
        .unwrap();
        let lines = fake.lines();
        assert_eq!(lines[0], "docker image inspect redis:7");
        assert!(
            lines[1].starts_with("docker container inspect"),
            "{lines:?}"
        );
        assert_eq!(lines.len(), 2, "jq must not touch the container CLI");
    }

    // ── status --json ────────────────────────────────────────────────────────

    fn config(yaml: &str) -> DevyConfig {
        yaml::from_str(yaml).unwrap()
    }

    fn document(report: &StatusReport) -> serde_json::Value {
        let out = crate::commands::json::render(&report.document()).unwrap();
        serde_json::from_str(&out).unwrap()
    }

    fn report(
        config: &DevyConfig,
        env_mgr: &dyn EnvManager,
        dir: &std::path::Path,
    ) -> StatusReport {
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let fake = FakeRunner::ok();
        status_report(
            config,
            &pm,
            ContainerRuntime::new(config.container_cli, &fake),
            env_mgr,
            dir,
        )
        .unwrap()
    }

    #[test]
    fn status_json_before_up() {
        let dir = crate::test_support::tmp_dir();
        let config = config("name: app\nenvironment:\n  LOG_LEVEL: debug\n");
        let doc = document(&report(&config, &MockEnvManager::default(), &dir));
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["project"], "app");
        assert_eq!(doc["package_manager"], "nix");
        assert_eq!(doc["environment_written"], false);
        assert_eq!(
            doc["environment"],
            serde_json::json!({"LOG_LEVEL": null}),
            "{doc}"
        );
        assert_eq!(doc["dependencies"], serde_json::json!([]));
        assert_eq!(doc["path"], serde_json::json!([]));
        assert_eq!(doc["commands"], serde_json::json!([]));
    }

    #[test]
    fn status_json_lists_commands_sorted_by_name() {
        let dir = crate::test_support::tmp_dir();
        let config = config("commands:\n  test: cargo test\n  lint: cargo clippy\n");
        let doc = document(&report(&config, &MockEnvManager::default(), &dir));
        let shell = crate::config::default_shell();
        assert_eq!(
            doc["commands"],
            serde_json::json!([
                {"name": "lint", "cmd": "cargo clippy", "shell": shell},
                {"name": "test", "cmd": "cargo test", "shell": shell},
            ])
        );
    }

    #[test]
    fn status_json_reports_dependencies() {
        let dir = crate::test_support::tmp_dir();
        let config = config("dependencies:\n  - node:\n      version: \"22\"\n  - redis\n");
        let doc = document(&report(&config, &MockEnvManager::default(), &dir));
        assert_eq!(
            doc["dependencies"],
            serde_json::json!([
                {"name": "node", "installed": true, "version": "22", "service": false},
                {
                    "name": "redis",
                    "installed": true,
                    "version": null,
                    "service": true,
                    "backend": "package",
                    "running": true,
                    "host": "127.0.0.1",
                    "port": null,
                    "port_source": "unassigned",
                },
            ])
        );
    }

    #[test]
    fn status_json_writes_nothing() {
        let dir = crate::test_support::tmp_dir();
        let config = config("dependencies:\n  - redis\nenvironment:\n  LOG_LEVEL: debug\n");
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let fake = FakeRunner::ok();
        status_impl(
            &config,
            &pm,
            ContainerRuntime::new(config.container_cli, &fake),
            &Shadowenv,
            &dir,
            true,
        )
        .unwrap();
        assert!(!dir.join(crate::lock::PATH).exists());
        assert!(!dir.join(".shadowenv.d").exists());
    }

    #[test]
    fn status_json_redacts_secrets_in_commands() {
        let dir = crate::test_support::tmp_dir();
        let config = config(concat!(
            "commands:\n",
            "  db: \"mysql -uroot -phunter2 app\"\n",
            "  fetch: \"curl -H 'Authorization: Bearer abc.def.ghi' https://x\"\n",
            "  push: \"GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123 git push\"\n",
        ));
        let doc = document(&report(&config, &Shadowenv, &dir));
        let text = doc["commands"].to_string();
        for secret in ["hunter2", "abc.def.ghi", "ghp_abc"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
        assert!(text.contains("mysql -uroot"), "{text}");
    }

    #[test]
    fn status_json_redacts_secret_environment_values() {
        let dir = crate::test_support::tmp_dir();
        let written: HashMap<String, String> = [
            ("STRIPE_SECRET_KEY", "sk_live_abc"),
            ("DATABASE_URL", "postgres://app:hunter2@127.0.0.1:5432/app"),
            ("LOG_LEVEL", "debug"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        Shadowenv.write_env_file(&dir, &written, &[]).unwrap();
        let config = crate::test_support::make_config(&[], written.clone());

        let report = report(&config, &Shadowenv, &dir);
        let doc = document(&report);
        assert_eq!(doc["environment_written"], true);
        let env = &doc["environment"];
        assert_eq!(env["STRIPE_SECRET_KEY"], "<redacted>");
        let url = env["DATABASE_URL"].as_str().unwrap();
        assert!(!url.contains("hunter2"), "{url}");
        assert!(url.starts_with("postgres://app:"), "{url}");
        assert!(url.ends_with("@127.0.0.1:5432/app"), "{url}");
        assert_eq!(env["LOG_LEVEL"], "debug");

        // The text tables render `written_vars`, which keeps the raw values.
        assert_eq!(report.written_vars, Some(written));
    }

    #[test]
    fn status_json_path_matches_what_up_writes() {
        let dir = crate::test_support::tmp_dir();
        let config = config("dependencies:\n  - python\n  - jq\n");
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            path_prepends_result: vec!["/p/.devy/nix-profile/bin".into()],
            ..Default::default()
        };
        let fake = FakeRunner::ok();
        let report = status_report(
            &config,
            &pm,
            ContainerRuntime::new(config.container_cli, &fake),
            &MockEnvManager::default(),
            &dir,
        )
        .unwrap();
        let deps = config.normalized_dependencies().unwrap();
        let expected = crate::project_env::resolve(&config, &deps, &pm, &dir, PortMode::ReadOnly)
            .path_prepends;
        assert_eq!(expected[0], "/p/.devy/nix-profile/bin");
        let doc = document(&report);
        let entries: Vec<&str> = doc["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["entry"].as_str().unwrap())
            .collect();
        assert_eq!(entries, expected);
        assert!(
            doc["path"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["written"] == false)
        );
        // The text table keeps showing module entries only.
        assert_eq!(report.path_prepends, expected[1..]);
    }

    #[test]
    fn worktree_line_names_the_main_project_root_or_its_absence() {
        let mut worktree = crate::worktree::LinkedWorktree {
            common_dir: "/src/app/.git".into(),
            top_level: "/src/app-feat".into(),
            main_checkout: Some("/src/app".into()),
            main_project_root: Some(std::path::Path::new("/src/app").join("api")),
        };
        assert_eq!(
            worktree_line(&worktree),
            format!(
                "worktree of {}",
                std::path::Path::new("/src/app").join("api").display()
            )
        );
        worktree.main_project_root = None;
        assert_eq!(
            worktree_line(&worktree),
            format!(
                "worktree of {} (main project directory not read)",
                std::path::Path::new("/src/app").display()
            )
        );
        worktree.main_checkout = None;
        assert_eq!(worktree_line(&worktree), "worktree (no main checkout)");
    }

    #[test]
    fn display_path_drops_the_windows_verbatim_prefix() {
        use std::path::Path;
        assert_eq!(display_path(Path::new(r"\\?\C:\src\app")), r"C:\src\app");
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\host\share\app")),
            r"\\host\share\app"
        );
        assert_eq!(display_path(Path::new("/src/app")), "/src/app");
    }

    #[test]
    fn status_report_detects_a_linked_worktree() {
        let tmp = crate::test_support::tmp_dir();
        let (main, feat) = crate::test_support::fake_linked_worktree(&tmp);
        let config = crate::test_support::make_config(&[], HashMap::new());
        let pm = MockPackageManager::default();
        let report = |root: &std::path::Path| {
            status_report(
                &config,
                &pm,
                ContainerRuntime::system(config.container_cli),
                &MockEnvManager::default(),
                root,
            )
            .unwrap()
        };
        let in_feat = report(&feat);
        assert_eq!(
            in_feat.worktree.and_then(|w| w.main_project_root),
            Some(main.clone())
        );
        assert!(report(&main).worktree.is_none());
        assert!(!feat.join(".devy").exists(), "status must not write files");
    }
}
