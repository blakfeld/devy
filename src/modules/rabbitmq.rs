use anyhow::{Context, Result};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep};

pub struct RabbitmqModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "apt" => "rabbitmq-server",
        "winget" => "VMware.RabbitMQ",
        "nix" => "rabbitmq-server",
        _ => "rabbitmq",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 5672)
}

impl Module for RabbitmqModule {
    fn is_service(&self) -> bool {
        true
    }

    fn extra_log_paths(&self, data_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        vec![data_dir.join("log")]
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let p = port(dep)?;
        // RabbitMQ derives its distribution port as node port + 20000, which overflows
        // for OS-assigned ports, so pick one and keep it across starts.
        let dist_port = super::helpers::persisted_port(
            &data_dir.join("dist_port"),
            &[p],
            "RabbitMQ distribution",
        )?;
        // A per-project node name keeps this node apart from a system RabbitMQ in epmd.
        let node_name = format!(
            "devy-{:08x}@localhost",
            super::fnv1a(&super::path_arg(data_dir)) as u32
        );
        Ok(Some(super::LaunchSpec {
            env: vec![
                ("RABBITMQ_NODE_PORT".into(), p.to_string()),
                ("RABBITMQ_DIST_PORT".into(), dist_port.to_string()),
                ("RABBITMQ_NODENAME".into(), node_name),
                ("RABBITMQ_NODE_IP_ADDRESS".into(), "127.0.0.1".into()),
                // epmd and the Erlang distribution listener otherwise bind every
                // interface, letting anyone with the cookie reach the node.
                ("ERL_EPMD_ADDRESS".into(), "127.0.0.1".into()),
                (
                    "RABBITMQ_SERVER_ADDITIONAL_ERL_ARGS".into(),
                    "-kernel inet_dist_use_interface {127,0,0,1}".into(),
                ),
                (
                    "RABBITMQ_MNESIA_BASE".into(),
                    super::path_arg(&data_dir.join("mnesia")),
                ),
                (
                    "RABBITMQ_LOG_BASE".into(),
                    super::path_arg(&data_dir.join("log")),
                ),
            ],
            ..super::LaunchSpec::new("rabbitmq-server", [])
        }))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("rabbitmq-server".to_string())
    }

    fn docker_spec(&self, _dep: &Dependency) -> Result<Option<super::DockerSpec>> {
        Ok(Some(
            super::DockerSpec::new("rabbitmq", "3", 5672).data("/var/lib/rabbitmq"),
        ))
    }

    fn default_port(&self) -> Option<u16> {
        Some(5672)
    }
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["port"])
    }

    fn backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency> {
        Some(pm_dep(dep, package_name(pm)))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        super::backend_installed(self, pm, dep)
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        super::install_backend(self, pm, dep)
    }

    fn post_setup_writes_service_config(&self) -> bool {
        true
    }

    /// Under Homebrew and apt, keeps AMQP, plugin listeners, epmd and Erlang distribution
    /// on loopback: see `loopback::secure_rabbitmq`.
    fn post_setup(
        &self,
        dep: &Dependency,
        pm: &dyn PackageManager,
        _project_root: &std::path::Path,
    ) -> Result<()> {
        let state = super::loopback::default_state_dir(pm);
        super::loopback::secure_rabbitmq(pm, port(dep)?, state.as_deref());
        Ok(())
    }

    fn is_running(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        pm.is_service_running(&self.service_name(dep))
    }

    fn start(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
        project_root: &std::path::Path,
    ) -> Result<()> {
        super::start_via_pm(self, pm, dep, project_root)
    }

    fn stop(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.stop_service(&self.service_name(dep))
    }

    fn health_check(&self, dep: &Dependency) -> Result<()> {
        let p = port(dep)?;
        let addr: SocketAddr = format!("127.0.0.1:{p}").parse()?;
        TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .with_context(|| format!("RabbitMQ not accepting connections on port {p}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn extra_log_paths_is_the_log_dir() {
        let d = std::path::Path::new("/p/.devy/data/rabbitmq");
        assert_eq!(RabbitmqModule.extra_log_paths(d), [d.join("log")]);
    }

    fn dep_with_port(port: u64) -> Dependency {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(port.into()),
        );
        Dependency {
            name: "rabbitmq".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
        }
    }

    #[test]
    fn rabbitmq_module_is_service() {
        assert!(RabbitmqModule.is_service());
    }

    #[test]
    fn port_defaults_to_5672() {
        let dep = Dependency::simple("rabbitmq");
        assert_eq!(port(&dep).unwrap(), 5672);
    }

    #[test]
    fn port_reads_custom_value() {
        let dep = dep_with_port(5673);
        assert_eq!(port(&dep).unwrap(), 5673);
    }

    #[test]
    fn port_bails_on_out_of_range() {
        let dep = dep_with_port(99999);
        assert!(port(&dep).is_err());
    }

    #[test]
    fn rabbitmq_health_check_fails_on_unused_port() {
        let dep = dep_with_port(19994);
        let err = RabbitmqModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19994"));
    }

    #[test]
    fn package_name_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "rabbitmq-server");
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "VMware.RabbitMQ");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "rabbitmq");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            RabbitmqModule
                .is_installed(&pm, &Dependency::simple("rabbitmq"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !RabbitmqModule
                .is_installed(&pm, &Dependency::simple("rabbitmq"))
                .unwrap()
        );
    }

    #[test]
    fn install_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        assert!(
            RabbitmqModule
                .install(&pm, &Dependency::simple("rabbitmq"))
                .is_err()
        );
    }

    #[test]
    fn is_running_true() {
        let pm = crate::package_manager::MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        assert!(
            RabbitmqModule
                .is_running(&pm, &Dependency::simple("rabbitmq"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !RabbitmqModule
                .is_running(&pm, &Dependency::simple("rabbitmq"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            RabbitmqModule
                .start(
                    &pm,
                    &Dependency::simple("rabbitmq"),
                    std::path::Path::new("/tmp")
                )
                .is_ok()
        );
    }

    #[test]
    fn start_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            start_service_fails: true,
            ..Default::default()
        };
        assert!(
            RabbitmqModule
                .start(
                    &pm,
                    &Dependency::simple("rabbitmq"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            RabbitmqModule
                .stop(&pm, &Dependency::simple("rabbitmq"))
                .is_ok()
        );
    }

    #[test]
    fn stop_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            stop_service_fails: true,
            ..Default::default()
        };
        assert!(
            RabbitmqModule
                .stop(&pm, &Dependency::simple("rabbitmq"))
                .is_err()
        );
    }
}
