use anyhow::{Context, Result, bail};

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep};

pub struct ElasticsearchModule;

// brew requires the elastic/tap tap: add `tap: elastic/tap` to the dependency in devy.yml.
fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "apt" => "elasticsearch",
        "winget" => "Elastic.Elasticsearch",
        "nix" => "elasticsearch",
        _ => "elasticsearch-full",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 9200)
}

impl Module for ElasticsearchModule {
    fn is_service(&self) -> bool {
        true
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        Ok(Some(super::search_server_launch(
            "elasticsearch",
            port(dep)?,
            data_dir,
        )))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("elasticsearch".to_string())
    }

    fn nix_unfree(&self) -> bool {
        true
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
        pm.install_package(&super::pkg_dep(self, pm, dep, package_name(pm)))
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
        let p = port(dep)?;
        let url = format!("http://127.0.0.1:{p}/");
        let response = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(2))
            .call()
            .with_context(|| format!("Elasticsearch not reachable on port {p}"))?;
        let body: ureq::serde_json::Value = response
            .into_json()
            .with_context(|| "Elasticsearch returned non-JSON response")?;
        let status = body
            .pointer("/status")
            .and_then(|v: &ureq::serde_json::Value| v.as_str())
            .unwrap_or("");
        classify_status(status)
    }
}

/// Classifies the Elasticsearch cluster status from the root endpoint.
/// Returns Ok for valid known statuses (green/yellow/red) and for empty status
/// (ES 8.x which omits the field), and Err for unexpected non-empty values.
pub(crate) fn classify_status(status: &str) -> Result<()> {
    if status == "green" || status == "yellow" || status == "red" {
        return Ok(());
    }
    // Elasticsearch 8.x root endpoint doesn't include cluster health status —
    // a reachable 200 response is sufficient to declare the node ready.
    if !status.is_empty() {
        bail!("Elasticsearch cluster status is '{status}' (expected 'green', 'yellow', or 'red')");
    }
    Ok(())
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
            name: "elasticsearch".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
        }
    }

    #[test]
    fn elasticsearch_module_is_service() {
        assert!(ElasticsearchModule.is_service());
    }

    #[test]
    fn port_defaults_to_9200() {
        let dep = Dependency::simple("elasticsearch");
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
    fn elasticsearch_health_check_fails_on_unused_port() {
        let dep = dep_with_port(19996);
        let err = ElasticsearchModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19996"));
    }

    #[test]
    fn package_name_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "elasticsearch");
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Elastic.Elasticsearch");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "elasticsearch-full");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            ElasticsearchModule
                .is_installed(&pm, &Dependency::simple("elasticsearch"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !ElasticsearchModule
                .is_installed(&pm, &Dependency::simple("elasticsearch"))
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
            ElasticsearchModule
                .install(&pm, &Dependency::simple("elasticsearch"))
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
            ElasticsearchModule
                .is_running(&pm, &Dependency::simple("elasticsearch"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !ElasticsearchModule
                .is_running(&pm, &Dependency::simple("elasticsearch"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            ElasticsearchModule
                .start(
                    &pm,
                    &Dependency::simple("elasticsearch"),
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
            ElasticsearchModule
                .start(
                    &pm,
                    &Dependency::simple("elasticsearch"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            ElasticsearchModule
                .stop(&pm, &Dependency::simple("elasticsearch"))
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
            ElasticsearchModule
                .stop(&pm, &Dependency::simple("elasticsearch"))
                .is_err()
        );
    }

    // ── health_check logic ────────────────────────────────────────────────────

    #[test]
    fn health_check_unknown_status_returns_error() {
        let dep = dep_with_port(19981);
        assert!(ElasticsearchModule.health_check(&dep).is_err());
    }

    // ── classify_status ───────────────────────────────────────────────────────

    #[test]
    fn classify_status_green_ok() {
        assert!(classify_status("green").is_ok());
    }

    #[test]
    fn classify_status_yellow_ok() {
        assert!(classify_status("yellow").is_ok());
    }

    #[test]
    fn classify_status_red_ok() {
        assert!(classify_status("red").is_ok());
    }

    #[test]
    fn classify_status_empty_ok() {
        // ES 8.x omits status — empty string means "reachable but no status field"
        assert!(classify_status("").is_ok());
    }

    #[test]
    fn classify_status_unknown_returns_err() {
        assert!(classify_status("unknown").is_err());
        let err = classify_status("bad-status").unwrap_err();
        assert!(err.to_string().contains("bad-status"));
    }

    #[test]
    fn classify_status_not_green_is_err() {
        // Kills `replace == with != at 65:19` — "not-green" must NOT be Ok
        assert!(classify_status("not-green").is_err());
    }

    #[test]
    fn classify_status_not_yellow_is_err() {
        // Kills `replace == with != at 65:40` — "not-yellow" must NOT be Ok
        assert!(classify_status("not-yellow").is_err());
    }

    #[test]
    fn classify_status_not_red_is_err() {
        // Kills `replace == with != at 65:62` — "not-red" must NOT be Ok
        assert!(classify_status("not-red").is_err());
    }

    #[test]
    fn classify_status_all_three_required() {
        // Kills `replace || with && at 65:30` (green || yellow):
        // If only green were accepted, yellow and red would be Err.
        assert!(classify_status("yellow").is_ok(), "yellow must be Ok");
        // Kills `replace || with && at 65:52` (|| red):
        // If only green || yellow were accepted, red would be Err.
        assert!(classify_status("red").is_ok(), "red must be Ok");
    }
}
