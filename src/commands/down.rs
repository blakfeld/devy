use anyhow::{Context, Result};

use crate::commands::exec::run_hook;
use crate::config::DevyConfig;
use crate::modules;
use crate::output;
use crate::package_manager;
use crate::service_runner::Runners;
use crate::service_runner::docker::ContainerRuntime;

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — requires a real devy.yml and package manager
pub fn run(volumes: bool) -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;

    let project_name = config.name.as_deref().unwrap_or("project");
    output::header(&format!("devy down · {}", project_name));

    if let Some(ref hook) = config.hooks.before_down {
        output::header("Hooks");
        run_hook("before_down", hook, &project_root)?;
    }

    let pm = package_manager::detect(&config, &project_root)?;
    let runners = Runners::new(
        pm.as_ref(),
        ContainerRuntime::system(config.container_cli),
        &config,
        &project_root,
        None,
        false,
    );
    down_impl(&config, &runners, volumes)?;

    if let Some(ref hook) = config.hooks.after_down {
        output::header("Hooks");
        run_hook("after_down", hook, &project_root)?;
    }

    output::blank_line();
    Ok(())
}

/// Stops every running service. With `volumes`, also removes each docker-managed
/// service's container and data volume; package-managed services are unaffected.
pub(crate) fn down_impl(config: &DevyConfig, runners: &Runners, volumes: bool) -> Result<()> {
    let deps = config.normalized_dependencies()?;
    let services: Vec<_> = deps
        .iter()
        .filter(|dep| modules::get(&dep.name).is_service())
        .collect();

    if services.is_empty() {
        output::skip("no services defined");
        return Ok(());
    }

    runners.ensure_docker_available(services.iter().copied())?;

    // Services are stopped in declaration order (the order they appear in devy.yml).
    // Modules that need to stop before a peer (e.g. Kafka before ZooKeeper) must
    // handle that ordering themselves inside their stop() implementation.
    let mut stopped_any = false;
    for dep in services {
        let runner = runners.runner_for(dep);
        // Read before migrating: a running legacy unit counts, and migration stops it.
        let running = runner.is_running(dep)?;
        runner.migrate(dep)?;
        if running {
            output::step(&format!("Stopping {}", dep.name));
            runner
                .stop(dep)
                .with_context(|| format!("Failed to stop {}", dep.name))?;
            runner.wait_for_stopped(dep)?;
            output::success(&format!("{} stopped", dep.name));
            stopped_any = true;
        } else {
            output::skip(&format!("{} already stopped", dep.name));
        }

        if volumes
            && runner
                .remove(dep, true)
                .with_context(|| format!("Failed to remove {}", dep.name))?
        {
            output::success(&format!("{} container and volume removed", dep.name));
        }
    }

    if stopped_any {
        output::success("all services stopped");
    } else {
        output::skip("nothing to stop");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevyConfig;
    use crate::package_manager::MockPackageManager;
    use crate::service_runner::docker::{FakeRunner, ok};
    use crate::service_runner::package_runners;
    use serde_norway as yaml;
    use std::collections::HashMap;
    use std::path::Path;

    fn make_config(dep_names: &[&str]) -> DevyConfig {
        crate::test_support::make_config(dep_names, HashMap::new())
    }

    fn down(config: &DevyConfig, pm: &MockPackageManager) -> Result<()> {
        down_impl(config, &package_runners(pm, Path::new("/tmp")), false)
    }

    #[test]
    fn down_impl_stops_running_service() {
        let config = make_config(&["mysql"]);
        let pm = MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        down(&config, &pm).unwrap();
        assert!(
            !pm.stopped_services.borrow().is_empty(),
            "stop must be called for a running service"
        );
    }

    #[test]
    fn down_impl_migrates_every_service_even_when_stopped() {
        let config = make_config(&["mysql", "redis"]);
        let pm = MockPackageManager::default();
        down(&config, &pm).unwrap();
        assert_eq!(*pm.migrated_services.borrow(), ["mysql", "redis"]);
    }

    #[test]
    fn down_impl_propagates_stop_error() {
        let config = make_config(&["mysql"]);
        let pm = MockPackageManager {
            service_running: true,
            stop_service_fails: true,
            ..Default::default()
        };
        assert!(
            down(&config, &pm).is_err(),
            "stop failure must be propagated as Err"
        );
    }

    #[test]
    fn down_impl_skips_service_already_stopped() {
        let config = make_config(&["mysql"]);
        let pm = MockPackageManager {
            service_running: false,
            ..Default::default()
        };
        down(&config, &pm).unwrap();
        assert!(pm.stopped_services.borrow().is_empty());
    }

    #[test]
    fn down_impl_returns_ok_with_no_services() {
        let config = make_config(&["node"]); // node is not a service
        let pm = MockPackageManager::default();
        assert!(down(&config, &pm).is_ok());
    }

    #[test]
    fn down_volumes_leaves_package_managed_services_alone() {
        let config = make_config(&["mysql"]);
        let pm = MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        // package_runners panics on any container CLI call.
        down_impl(&config, &package_runners(&pm, Path::new("/tmp")), true).unwrap();
        assert_eq!(*pm.stopped_services.borrow(), vec!["mysql".to_string()]);
    }

    /// Runs `down` on a docker-managed postgres whose container is running, created by
    /// an older devy (no `sh.devy.host` label).
    fn docker_down(volumes: bool) -> (Vec<String>, String) {
        docker_down_on(volumes, None)
    }

    /// `docker_down`, with the container's `sh.devy.host` label (or none) given.
    fn docker_down_on(volumes: bool, host: Option<&str>) -> (Vec<String>, String) {
        let host_label = host
            .map(|h| format!(r#","sh.devy.host":"{h}""#))
            .unwrap_or_default();
        let config: DevyConfig =
            yaml::from_str("name: app\nservice_manager: docker\ndependencies:\n  - postgres\n")
                .unwrap();
        let pm = MockPackageManager::default();
        let stopped = std::cell::Cell::new(false);
        let stopped = std::rc::Rc::new(stopped);
        let seen = std::rc::Rc::clone(&stopped);
        let fake = FakeRunner::new(move |call| match call[1].as_str() {
            "stop" => {
                seen.set(true);
                ok("")
            }
            "container" => ok(&format!(
                r#"{{"State":{{"Running":{}}},"Config":{{"Labels":{{"sh.devy.project":"/src/app"{host_label}}}}}}}"#,
                !seen.get()
            )),
            _ => ok(""),
        });
        let root = Path::new("/src/app");
        let mut runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &fake),
            &config,
            root,
            None,
            false,
        );
        runners.docker.set_host(Some(THIS_HOST));
        down_impl(&config, &runners, volumes).unwrap();
        let name = runners
            .docker
            .container_name(&config.normalized_dependencies().unwrap()[0]);
        (fake.lines(), name)
    }

    #[test]
    fn plain_down_stops_container_and_keeps_volume() {
        let (lines, name) = docker_down(false);
        assert_eq!(lines[0], "docker info --format {{json .ServerVersion}}");
        assert!(lines.contains(&format!("docker stop {name}")), "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.contains(" rm ")),
            "plain down must keep the container and volume: {lines:?}"
        );
        assert!(name.ends_with("-postgresql"));
    }

    #[test]
    fn down_volumes_removes_container_and_volume() {
        let (lines, name) = docker_down(true);
        assert!(lines.contains(&format!("docker stop {name}")), "{lines:?}");
        let rm = lines
            .iter()
            .position(|l| *l == format!("docker rm -f {name}"))
            .expect("container removed");
        let volume_rm = lines
            .iter()
            .position(|l| *l == format!("docker volume rm {name}"))
            .expect("volume removed");
        assert!(rm < volume_rm);
    }

    /// The host id `docker_down_on` runs as.
    const THIS_HOST: &str = "0123456789abcdef";

    #[test]
    fn down_acts_on_this_hosts_container() {
        let (lines, name) = docker_down_on(true, Some(THIS_HOST));
        assert!(lines.contains(&format!("docker stop {name}")), "{lines:?}");
        assert!(lines.contains(&format!("docker rm -f {name}")), "{lines:?}");
        assert!(
            lines.contains(&format!("docker volume rm {name}")),
            "{lines:?}"
        );
    }

    #[test]
    fn down_leaves_another_hosts_container_alone() {
        let mut lines = Vec::new();
        let warnings = crate::output::with_warn_messages(|| {
            lines = docker_down_on(true, Some("ffffffffffffffff")).0;
        });
        assert!(
            !lines
                .iter()
                .any(|l| l.contains(" stop ") || l.contains(" rm ")),
            "{lines:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("belongs to another machine or user")),
            "{warnings:?}"
        );
    }

    #[test]
    fn down_fails_when_container_runtime_unavailable() {
        let config: DevyConfig =
            yaml::from_str("service_manager: docker\ndependencies:\n  - redis\n").unwrap();
        let pm = MockPackageManager::default();
        let fake = FakeRunner::new(|_| crate::service_runner::docker::fail("daemon down"));
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &fake),
            &config,
            Path::new("/tmp"),
            None,
            false,
        );
        let err = down_impl(&config, &runners, false).unwrap_err();
        assert!(err.to_string().starts_with("docker is not available"));
        assert_eq!(
            fake.lines().len(),
            1,
            "nothing after the availability check"
        );
    }
}
