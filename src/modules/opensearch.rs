use anyhow::Result;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep, tcp_ping};

pub struct OpenSearchModule;

// apt requires the OpenSearch apt repository; see https://opensearch.org/docs/latest/install-and-configure/install-opensearch/debian/
fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "OpenSearch.OpenSearch",
        "nix" => "opensearch",
        _ => "opensearch",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 9200)
}

impl Module for OpenSearchModule {
    fn is_service(&self) -> bool {
        true
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let mut spec = super::search_server_launch(
            "opensearch",
            super::nix_install_attr(self, dep, "opensearch"),
            "OPENSEARCH_PATH_CONF",
            port(dep)?,
            data_dir,
        )?;
        // The security plugin turns on TLS; the health check speaks plain HTTP. The
        // setting aborts startup when the plugin isn't bundled, so it's conditional.
        spec.conditional_args.push(super::ConditionalArgs {
            package_path: "plugins/opensearch-security".into(),
            args: vec!["-E".into(), "plugins.security.disabled=true".into()],
        });
        Ok(Some(spec))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("opensearch".to_string())
    }

    fn docker_spec(&self, _dep: &Dependency) -> Result<Option<super::DockerSpec>> {
        Ok(Some(
            super::DockerSpec::new("opensearchproject/opensearch", "2", 9200)
                .data("/usr/share/opensearch/data")
                .env(&[
                    ("discovery.type", "single-node"),
                    ("DISABLE_SECURITY_PLUGIN", "true"),
                    // 2.12+ otherwise demands an initial admin password.
                    ("DISABLE_INSTALL_DEMO_CONFIG", "true"),
                ]),
        ))
    }

    fn default_port(&self) -> Option<u16> {
        Some(9200)
    }
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["port"])
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        pm.is_package_installed(&pm_dep(dep, package_name(pm)))
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.install_package(&pm_dep(dep, package_name(pm)))
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

    fn service_config(&self) -> super::ServiceConfig {
        super::ServiceConfig {
            health_check_max_attempts: 120,
            ..Default::default()
        }
    }

    fn health_check(&self, dep: &Dependency) -> Result<()> {
        tcp_ping(port(dep)?, "OpenSearch")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn dep_with_port(port: u64) -> Dependency {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(port.into()),
        );
        Dependency {
            name: "opensearch".into(),
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
    fn opensearch_module_is_service() {
        assert!(OpenSearchModule.is_service());
    }

    #[test]
    fn port_defaults_to_9200() {
        let dep = Dependency::simple("opensearch");
        assert_eq!(port(&dep).unwrap(), 9200);
    }

    #[test]
    fn port_reads_custom_value() {
        let dep = dep_with_port(9201);
        assert_eq!(port(&dep).unwrap(), 9201);
    }

    #[test]
    fn port_bails_on_out_of_range() {
        let dep = dep_with_port(99999);
        assert!(port(&dep).is_err());
    }

    #[test]
    fn opensearch_health_check_fails_on_unused_port() {
        let dep = dep_with_port(19997);
        let err = OpenSearchModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19997"));
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "OpenSearch.OpenSearch");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "opensearch");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            OpenSearchModule
                .is_installed(&pm, &Dependency::simple("opensearch"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !OpenSearchModule
                .is_installed(&pm, &Dependency::simple("opensearch"))
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
            OpenSearchModule
                .install(&pm, &Dependency::simple("opensearch"))
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
            OpenSearchModule
                .is_running(&pm, &Dependency::simple("opensearch"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !OpenSearchModule
                .is_running(&pm, &Dependency::simple("opensearch"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            OpenSearchModule
                .start(
                    &pm,
                    &Dependency::simple("opensearch"),
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
            OpenSearchModule
                .start(
                    &pm,
                    &Dependency::simple("opensearch"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            OpenSearchModule
                .stop(&pm, &Dependency::simple("opensearch"))
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
            OpenSearchModule
                .stop(&pm, &Dependency::simple("opensearch"))
                .is_err()
        );
    }
}
