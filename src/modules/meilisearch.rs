use anyhow::Result;
use std::collections::HashMap;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use crate::output;

use super::{Module, pm_dep, tcp_ping};

pub struct MeilisearchModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "Meilisearch.Meilisearch",
        "nix" => "meilisearch",
        _ => "meilisearch",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 7700)
}

impl Module for MeilisearchModule {
    fn is_service(&self) -> bool {
        true
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let p = port(dep)?;
        let mut args = vec![
            "--http-addr".to_string(),
            format!("127.0.0.1:{p}"),
            "--db-path".to_string(),
            super::path_arg(data_dir),
        ];
        if let Some(key) = dep.extra.get("master_key").and_then(|v| v.as_str()) {
            args.push("--master-key".into());
            args.push(key.to_string());
        }
        Ok(Some(super::LaunchSpec::new("meilisearch", args)))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("meilisearch".to_string())
    }

    fn default_port(&self) -> Option<u16> {
        Some(7700)
    }
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["port", "master_key"])
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
            health_check_max_attempts: 60,
            ..Default::default()
        }
    }

    fn health_check(&self, dep: &Dependency) -> Result<()> {
        tcp_ping(port(dep)?, "Meilisearch")
    }

    fn env_vars(
        &self,
        dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        if let Some(key) = dep.extra.get("master_key").and_then(|v| v.as_str()) {
            vars.insert("MEILI_MASTER_KEY".into(), key.to_string());
        }
        vars
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        _pm: &dyn PackageManager,
        _project_root: &std::path::Path,
    ) -> Result<()> {
        if dep
            .extra
            .get("master_key")
            .and_then(|v| v.as_str())
            .is_some()
        {
            output::warn(
                "Meilisearch: master_key is set in plaintext in devy.yml. \
                 Consider using environment variable substitution or a secrets manager \
                 rather than committing this value.",
            );
        }
        Ok(())
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
        Dependency::with_extra("meilisearch", extra)
    }

    #[test]
    fn meilisearch_module_is_service() {
        assert!(MeilisearchModule.is_service());
    }

    #[test]
    fn port_defaults_to_7700() {
        let dep = Dependency::simple("meilisearch");
        assert_eq!(port(&dep).unwrap(), 7700);
    }

    #[test]
    fn port_reads_custom_value() {
        let dep = dep_with_port(7701);
        assert_eq!(port(&dep).unwrap(), 7701);
    }

    #[test]
    fn port_bails_on_out_of_range() {
        let dep = dep_with_port(99999);
        assert!(port(&dep).is_err());
    }

    #[test]
    fn meilisearch_health_check_fails_on_unused_port() {
        let dep = dep_with_port(19990);
        let err = MeilisearchModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19990"));
    }

    #[test]
    fn env_vars_empty_when_no_master_key() {
        let dep = Dependency::simple("meilisearch");
        assert!(
            MeilisearchModule
                .env_vars(&dep, std::path::Path::new("/tmp"))
                .is_empty()
        );
    }

    #[test]
    fn env_vars_includes_master_key_when_configured() {
        let mut extra = HashMap::new();
        extra.insert(
            "master_key".into(),
            crate::config::ExtraValue::String("supersecret".into()),
        );
        let dep = Dependency::with_extra("meilisearch", extra);
        let vars = MeilisearchModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("MEILI_MASTER_KEY").map(|s| s.as_str()),
            Some("supersecret")
        );
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Meilisearch.Meilisearch");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "meilisearch");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            MeilisearchModule
                .is_installed(&pm, &Dependency::simple("meilisearch"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MeilisearchModule
                .is_installed(&pm, &Dependency::simple("meilisearch"))
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
            MeilisearchModule
                .install(&pm, &Dependency::simple("meilisearch"))
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
            MeilisearchModule
                .is_running(&pm, &Dependency::simple("meilisearch"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MeilisearchModule
                .is_running(&pm, &Dependency::simple("meilisearch"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MeilisearchModule
                .start(
                    &pm,
                    &Dependency::simple("meilisearch"),
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
            MeilisearchModule
                .start(
                    &pm,
                    &Dependency::simple("meilisearch"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MeilisearchModule
                .stop(&pm, &Dependency::simple("meilisearch"))
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
            MeilisearchModule
                .stop(&pm, &Dependency::simple("meilisearch"))
                .is_err()
        );
    }
}
