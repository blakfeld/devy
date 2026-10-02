use anyhow::Result;

use crate::config::DevyConfig;
use crate::env_manager::{EnvManager, Shadowenv};
use crate::output;
use crate::package_manager;
use crate::package_manager::PackageManager;
use crate::service_runner::Runners;
use crate::service_runner::docker::ContainerRuntime;

use super::ports::{self, PortMode};
use super::shared;

pub(crate) fn status_impl(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    project_root: &std::path::Path,
) -> Result<()> {
    let project_name = config.name.as_deref().unwrap_or("project");
    output::header(&format!("devy status · {}", project_name));

    let mut deps = config.normalized_dependencies()?;
    // Resolve ports as `up` would so service probes see the locked port; never writes.
    let lock = ports::load_lock(project_root)?;
    ports::resolve_ports(&mut deps, lock.as_ref(), pm, PortMode::ReadOnly)?;

    if !deps.is_empty() {
        output::header("Dependencies");
        let runners = Runners::new(pm, runtime, config, project_root, lock.as_ref(), false);
        shared::print_dep_table(&deps, &runners, false)?;
    }

    let path_prepends: Vec<String> = deps
        .iter()
        .flat_map(|dep| crate::modules::get(&dep.name).path_prepends(dep, project_root))
        .collect();

    if !config.environment.is_empty() || !path_prepends.is_empty() {
        output::header("Environment");
        let written_vars = env_mgr.read_vars(project_root);
        shared::print_env_table(&config.environment, written_vars, false)?;

        if !path_prepends.is_empty() {
            let written_paths = env_mgr.read_path_prepends(project_root);
            shared::print_path_table(&path_prepends, written_paths, false);
        }
    }

    output::blank_line();
    Ok(())
}

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — requires a real devy.yml and package manager
pub fn run() -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;
    let pm = package_manager::detect(&config, &project_root)?;
    let env_mgr = Shadowenv;
    status_impl(
        &config,
        pm.as_ref(),
        ContainerRuntime::system(config.container_cli),
        &env_mgr,
        &project_root,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_manager::MockEnvManager;
    use crate::package_manager::MockPackageManager;
    use crate::service_runner::docker::{FakeRunner, fail, ok};
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
        )
        .unwrap();
        assert!(!dir.join(crate::lock::PATH).exists());
        assert!(fake.lines().is_empty(), "no docker-managed services");
    }

    #[test]
    fn status_impl_probes_docker_services_through_the_container_cli() {
        let dir = crate::test_support::tmp_dir();
        let config: DevyConfig =
            serde_yml::from_str("service_manager: docker\ndependencies:\n  - redis\n  - jq\n")
                .unwrap();
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
}
