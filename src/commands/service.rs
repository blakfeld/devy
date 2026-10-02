use anyhow::{Result, bail};
use colored::Colorize;
use std::path::Path;

use crate::commands::ports::{self, PortMode};
use crate::config::{Dependency, DevyConfig};
use crate::modules;
use crate::output;
use crate::package_manager::{self, PackageManager};
use crate::service_runner::docker::ContainerRuntime;
use crate::service_runner::{self, Runners, ServiceRunner};

/// Print all services from devy.yml with their current running status.
#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — requires a real devy.yml and package manager
pub fn list() -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;
    let pm = package_manager::detect(&config, &project_root)?;
    let runners = Runners::new(
        pm.as_ref(),
        ContainerRuntime::system(config.container_cli),
        &config,
        &project_root,
        None,
        false,
    );
    list_impl(&config, &runners)
}

/// One line per service: `● redis` when running, `○ redis` when stopped, with
/// docker-managed services suffixed `(docker)`.
pub(crate) fn list_impl(config: &DevyConfig, runners: &Runners) -> Result<()> {
    let services: Vec<_> = config
        .normalized_dependencies()?
        .into_iter()
        .filter(|dep| modules::get(&dep.name).is_service())
        .collect();

    if services.is_empty() {
        println!("No services defined.");
        return Ok(());
    }

    output::header("Services");

    for dep in &services {
        let runner = runners.runner_for(dep);
        let name = service_runner::display_name(&dep.name, runner);
        if runner.is_running(dep)? {
            println!("  {}  {}", "●".green().bold(), name);
        } else {
            println!("  {}  {}", "○".dimmed(), name.dimmed());
        }
    }

    println!();
    Ok(())
}

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — requires a real devy.yml and package manager
pub fn start(name: &str) -> Result<()> {
    with_service(name, true, start_impl)
}

pub(crate) fn start_impl(dep: &Dependency, runner: &dyn ServiceRunner) -> Result<()> {
    let module = modules::get(&dep.name);

    if runner.is_running(dep)? {
        output::skip(&format!("{} is already running", dep.name));
        return Ok(());
    }

    output::step(&format!("Starting {}…", dep.name));
    runner.start(dep)?;
    if let Err(e) = module.wait_for_ready(dep) {
        output::warn(&format!(
            "{} started but health check timed out — verify manually: {}",
            dep.name, e
        ));
    }
    output::success(&format!("{} started", dep.name));
    Ok(())
}

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — requires a real devy.yml and package manager
pub fn stop(name: &str) -> Result<()> {
    with_service(name, false, stop_impl)
}

pub(crate) fn stop_impl(dep: &Dependency, runner: &dyn ServiceRunner) -> Result<()> {
    if !runner.is_running(dep)? {
        output::skip(&format!("{} is already stopped", dep.name));
        return Ok(());
    }

    output::step(&format!("Stopping {}…", dep.name));
    runner.stop(dep)?;
    runner.wait_for_stopped(dep)?;
    output::success(&format!("{} stopped", dep.name));
    Ok(())
}

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — requires a real devy.yml and package manager
pub fn restart(name: &str) -> Result<()> {
    with_service(name, true, restart_impl)
}

pub(crate) fn restart_impl(dep: &Dependency, runner: &dyn ServiceRunner) -> Result<()> {
    let module = modules::get(&dep.name);

    if runner.is_running(dep)? {
        output::step(&format!("Stopping {}…", dep.name));
        runner.stop(dep)?;
        runner.wait_for_stopped(dep)?;
        output::success(&format!("{} stopped", dep.name));
    } else {
        output::skip(&format!("{} was already stopped", dep.name));
    }

    output::step(&format!("Starting {}…", dep.name));
    runner.start(dep)?;
    if let Err(e) = module.wait_for_ready(dep) {
        output::warn(&format!(
            "{} started but health check timed out — verify manually: {}",
            dep.name, e
        ));
    }
    output::success(&format!("{} started", dep.name));
    Ok(())
}

/// Finds the named dependency in config and verifies it is a service.
/// Accepts both the exact name as written in devy.yml and any registered alias
/// (e.g. "postgres" matches a dep named "postgresql" and vice-versa).
pub(crate) fn resolve_dep(config: &DevyConfig, name: &str) -> Result<Dependency> {
    let canonical_target = modules::canonical_name(name);
    let dep = config
        .normalized_dependencies()?
        .into_iter()
        .find(|d| d.name == name || modules::canonical_name(&d.name) == canonical_target)
        .ok_or_else(|| anyhow::anyhow!("'{}' not found in devy.yml dependencies", name))?;

    if !modules::get(&dep.name).is_service() {
        bail!("'{}' is not a service", name);
    }

    Ok(dep)
}

/// Like `resolve_dep`, then resolves the service's port from devy.lock exactly as
/// `devy up` would, so start/restart launch and health-check the same port.
/// Never assigns a new port or writes devy.lock.
///
/// With `require_port`, fails when `devy up` hasn't assigned the port yet: starting on
/// the default port would leave the service where `devy up` later won't look.
pub(crate) fn resolve_service(
    config: &DevyConfig,
    name: &str,
    pm: &dyn PackageManager,
    project_root: &Path,
    require_port: bool,
) -> Result<Dependency> {
    let mut dep = resolve_dep(config, name)?;
    let lock = ports::load_lock(project_root)?;
    let resolved = ports::resolve_ports(
        std::slice::from_mut(&mut dep),
        lock.as_ref(),
        pm,
        PortMode::ReadOnly,
    )?;
    if require_port && resolved[0] == Some(ports::ResolvedPort::Unassigned) {
        bail!(
            "'{}' has no port in devy.lock yet — run `devy up` first",
            dep.name
        );
    }
    Ok(dep)
}

/// Resolves service `name` and runs `f` with it and its runner, after checking the
/// container runtime when the service is docker-managed.
#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — requires a real devy.yml and package manager
fn with_service(
    name: &str,
    require_port: bool,
    f: impl FnOnce(&Dependency, &dyn ServiceRunner) -> Result<()>,
) -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;
    let pm = package_manager::detect(&config, &project_root)?;
    let dep = resolve_service(&config, name, pm.as_ref(), &project_root, require_port)?;
    let lock = ports::load_lock(&project_root)?;
    let runners = Runners::new(
        pm.as_ref(),
        ContainerRuntime::system(config.container_cli),
        &config,
        &project_root,
        lock.as_ref(),
        false,
    );
    runners.ensure_docker_available([&dep])?;
    f(&dep, runners.runner_for(&dep))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevyConfig;
    use crate::package_manager::MockPackageManager;
    use crate::service_runner::{PackageRunner, package_runners};
    use std::collections::HashMap;

    fn make_config(dep_names: &[&str]) -> DevyConfig {
        crate::test_support::make_config(dep_names, HashMap::new())
    }

    // ── resolve_dep ───────────────────────────────────────────────────────────

    #[test]
    fn resolve_dep_finds_correct_dep_by_name() {
        let config = make_config(&["mysql", "redis"]);
        let dep = resolve_dep(&config, "mysql").unwrap();
        assert_eq!(
            dep.name, "mysql",
            "must return the dep matching the given name"
        );
    }

    #[test]
    fn resolve_dep_returns_err_for_unknown_name() {
        let config = make_config(&["mysql"]);
        assert!(resolve_dep(&config, "nonexistent").is_err());
    }

    #[test]
    fn resolve_dep_returns_err_for_non_service() {
        // node is NOT a service — resolve_dep must reject it.
        let config = make_config(&["node"]);
        assert!(
            resolve_dep(&config, "node").is_err(),
            "non-service dep must be rejected"
        );
    }

    #[test]
    fn resolve_dep_returns_ok_for_valid_service() {
        // mysql IS a service — resolve_dep must accept it.
        let config = make_config(&["mysql"]);
        assert!(
            resolve_dep(&config, "mysql").is_ok(),
            "service dep must be accepted"
        );
    }

    #[test]
    fn resolve_dep_accepts_alias_for_service() {
        // Config has "postgresql" (canonical), queried with alias "postgres".
        let config = make_config(&["postgresql"]);
        let dep = resolve_dep(&config, "postgres").unwrap();
        assert_eq!(
            dep.name, "postgresql",
            "must return the dep as written in devy.yml"
        );
    }

    #[test]
    fn resolve_dep_accepts_canonical_name_for_aliased_dep() {
        // Config has "postgres" (alias), queried with canonical "postgresql".
        let config = make_config(&["postgres"]);
        let dep = resolve_dep(&config, "postgresql").unwrap();
        assert_eq!(
            dep.name, "postgres",
            "must return the dep as written in devy.yml"
        );
    }

    // ── stop_impl ─────────────────────────────────────────────────────────────

    #[test]
    fn stop_impl_stops_running_service() {
        let pm = MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        stop_impl(&dep, &PackageRunner::new(&pm, Path::new("/tmp"))).unwrap();
        assert!(
            !pm.stopped_services.borrow().is_empty(),
            "stop must be called when service is running"
        );
    }

    #[test]
    fn stop_impl_propagates_stop_error() {
        let pm = MockPackageManager {
            service_running: true,
            stop_service_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        assert!(
            stop_impl(&dep, &PackageRunner::new(&pm, Path::new("/tmp"))).is_err(),
            "stop error must be propagated"
        );
    }

    #[test]
    fn stop_impl_skips_when_service_already_stopped() {
        let pm = MockPackageManager {
            service_running: false,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        stop_impl(&dep, &PackageRunner::new(&pm, Path::new("/tmp"))).unwrap();
        assert!(
            pm.stopped_services.borrow().is_empty(),
            "stop must not be called when service is already stopped"
        );
    }

    // ── start_impl ────────────────────────────────────────────────────────────

    #[test]
    fn start_impl_propagates_start_error() {
        let pm = MockPackageManager {
            service_running: false,
            start_service_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        assert!(
            start_impl(&dep, &PackageRunner::new(&pm, Path::new("/tmp"))).is_err(),
            "start error must be propagated"
        );
    }

    #[test]
    fn start_impl_skips_when_service_already_running() {
        let pm = MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        // Returns Ok without calling start_service.
        start_impl(&dep, &PackageRunner::new(&pm, Path::new("/tmp"))).unwrap();
        assert!(pm.started_services.borrow().is_empty());
    }

    #[test]
    fn start_impl_returns_ok_when_start_succeeds() {
        // Even though wait_for_ready will time out (no real service in tests),
        // start_impl must return Ok — timeout is demoted to a warning.
        let pm = MockPackageManager {
            service_running: false,
            start_service_fails: false,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        assert!(
            start_impl(&dep, &PackageRunner::new(&pm, Path::new("/tmp"))).is_ok(),
            "start_impl must return Ok when start succeeds, even if health check times out"
        );
    }

    // ── restart_impl ──────────────────────────────────────────────────────────

    #[test]
    fn restart_impl_propagates_start_error() {
        let pm = MockPackageManager {
            service_running: false,
            start_service_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        assert!(
            restart_impl(&dep, &PackageRunner::new(&pm, Path::new("/tmp"))).is_err(),
            "restart must propagate start error"
        );
    }

    // ── list_impl ─────────────────────────────────────────────────────────────

    #[test]
    fn list_impl_returns_ok_with_no_services() {
        let config = make_config(&["node"]); // node is not a service
        let pm = MockPackageManager::default();
        assert!(list_impl(&config, &package_runners(&pm, Path::new("/tmp"))).is_ok());
    }

    #[test]
    fn list_impl_returns_ok_with_services() {
        let config = make_config(&["mysql"]);
        let pm = MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        assert!(list_impl(&config, &package_runners(&pm, Path::new("/tmp"))).is_ok());
    }

    #[test]
    fn list_impl_returns_err_when_is_running_fails() {
        let config = make_config(&["mysql"]);
        let pm = MockPackageManager {
            installed: true,
            is_running_fails: true,
            ..Default::default()
        };
        assert!(
            list_impl(&config, &package_runners(&pm, Path::new("/tmp"))).is_err(),
            "list_impl must propagate is_running errors"
        );
    }

    // ── resolve_service ───────────────────────────────────────────────────────

    fn write_lock_with_port(dir: &std::path::Path, name: &str, port: u16) {
        let mut deps = std::collections::BTreeMap::new();
        deps.insert(
            name.to_string(),
            crate::lock::LockedDep {
                resolved_version: None,
                source: "nix".into(),
                assigned_port: Some(port),
                image_digest: None,
            },
        );
        crate::lock::LockFile {
            dependencies: deps,
            ..Default::default()
        }
        .write(&dir.join(crate::lock::PATH))
        .unwrap();
    }

    #[test]
    fn start_health_checks_the_locked_port() {
        let dir = crate::test_support::tmp_dir();
        write_lock_with_port(&dir, "redis", 51000);
        let config = make_config(&["redis"]);
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let dep = resolve_service(&config, "redis", &pm, &dir, true).unwrap();
        assert_eq!(
            crate::modules::helpers::extra_port(&dep, "port", 6379).unwrap(),
            51000,
            "start must probe the port recorded in devy.lock"
        );
        // The health check reports the port it probed.
        let err = modules::get("redis").health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("51000"), "{err}");
    }

    #[test]
    fn resolve_service_does_not_write_lock() {
        let dir = crate::test_support::tmp_dir();
        let config = make_config(&["redis"]);
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        resolve_service(&config, "redis", &pm, &dir, false).unwrap();
        assert!(!dir.join(crate::lock::PATH).exists());
    }

    #[test]
    fn start_requires_a_port_from_up() {
        let dir = crate::test_support::tmp_dir();
        let config = make_config(&["redis"]);
        let nix = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let err = resolve_service(&config, "redis", &nix, &dir, true).unwrap_err();
        assert!(err.to_string().contains("run `devy up` first"), "{err}");
        // Backends that can't apply ports always use the default, so nothing is missing.
        let brew = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert!(resolve_service(&config, "redis", &brew, &dir, true).is_ok());
    }

    // ── docker-managed services ──────────────────────────────────────────────

    use crate::service_runner::docker::{FakeRunner, ok};
    use std::cell::Cell;
    use std::rc::Rc;

    fn docker_config() -> DevyConfig {
        serde_yml::from_str(
            "name: app\nservice_manager: docker\ndependencies:\n  - redis\n  - jq\n",
        )
        .unwrap()
    }

    #[test]
    fn docker_services_are_labeled() {
        let config = docker_config();
        let pm = MockPackageManager::default();
        let cli = FakeRunner::ok();
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &cli),
            &config,
            Path::new("/src/app"),
            None,
            false,
        );
        let deps = config.normalized_dependencies().unwrap();
        assert_eq!(
            service_runner::display_name(&deps[0].name, runners.runner_for(&deps[0])),
            "redis (docker)"
        );
        assert_eq!(
            service_runner::display_name("redis", &PackageRunner::new(&pm, Path::new("/tmp"))),
            "redis"
        );
    }

    #[test]
    fn list_impl_reads_docker_container_state() {
        let config = docker_config();
        let pm = MockPackageManager::default();
        let cli = FakeRunner::new(|_| ok(r#"{"State":{"Running":true},"Config":{"Labels":{}}}"#));
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &cli),
            &config,
            Path::new("/src/app"),
            None,
            false,
        );
        list_impl(&config, &runners).unwrap();
        let lines = cli.lines();
        assert_eq!(lines.len(), 1, "only redis is a service: {lines:?}");
        assert!(lines[0].starts_with("docker container inspect"));
    }

    #[test]
    fn restart_keeps_the_port_and_reuses_the_container() {
        let config = docker_config();
        let pm = MockPackageManager::default();
        let root = Path::new("/src/app");
        let mut dep = config.normalized_dependencies().unwrap().remove(0);
        dep.extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(51000u64.into()),
        );
        // Labels of a container created for this exact configuration.
        let spec_cli = FakeRunner::ok();
        let labels: serde_json::Map<String, serde_json::Value> = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &spec_cli),
            &config,
            root,
            None,
            false,
        )
        .docker
        .run_spec(&dep)
        .unwrap()
        .labels
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
        let running = Rc::new(Cell::new(true));
        let state = Rc::clone(&running);
        let cli = FakeRunner::new(move |call| match call[1].as_str() {
            "stop" => {
                state.set(false);
                ok("")
            }
            "start" => {
                state.set(true);
                ok("")
            }
            _ => ok(&serde_json::json!({
                "State": {"Running": state.get()},
                "Config": {"Labels": labels.clone()},
            })
            .to_string()),
        });
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &cli),
            &config,
            root,
            None,
            false,
        );
        restart_impl(&dep, runners.runner_for(&dep)).unwrap();
        let name = runners.docker.container_name(&dep);
        let lines = cli.lines();
        assert!(lines.contains(&format!("docker stop {name}")), "{lines:?}");
        assert!(lines.contains(&format!("docker start {name}")), "{lines:?}");
        assert!(
            !lines
                .iter()
                .any(|l| l.contains(" run ") || l.contains(" rm ")),
            "same config must reuse the container: {lines:?}"
        );
        assert!(pm.stopped_services.borrow().is_empty());
    }

    #[test]
    fn docker_start_requires_a_port_from_up() {
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        let err = resolve_service(&docker_config(), "redis", &pm, &dir, true).unwrap_err();
        assert!(err.to_string().contains("run `devy up` first"), "{err}");
    }
}
