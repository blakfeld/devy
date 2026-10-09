use anyhow::{Context, Result};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep};

pub struct MailhogModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "MailHog.MailHog",
        "nix" => "mailhog",
        _ => "mailhog",
    }
}

/// MailHog's web UI and API port (they share one listener).
const UI_PORT: u16 = 8025;

fn smtp_port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "smtp_port", 1025)
}

impl Module for MailhogModule {
    fn is_service(&self) -> bool {
        true
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        _data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let p = smtp_port(dep)?;
        // MailHog's web UI and API otherwise listen on 0.0.0.0, exposing captured mail.
        let ui = format!("127.0.0.1:{UI_PORT}");
        Ok(Some(super::LaunchSpec::new(
            "MailHog",
            [
                "-smtp-bind-addr".into(),
                format!("127.0.0.1:{p}"),
                "-ui-bind-addr".into(),
                ui.clone(),
                "-api-bind-addr".into(),
                ui,
            ],
        )))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("mailhog".to_string())
    }

    fn docker_spec(&self, _dep: &Dependency) -> Result<Option<super::DockerSpec>> {
        Ok(Some(super::DockerSpec::new(
            "mailhog/mailhog",
            "v1.0.1",
            1025,
        )))
    }

    fn default_port(&self) -> Option<u16> {
        Some(1025)
    }
    fn port_key(&self) -> Option<&'static str> {
        Some("smtp_port")
    }
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["smtp_port"])
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
        let p = smtp_port(dep)?;
        let addr: SocketAddr = format!("127.0.0.1:{p}").parse()?;
        TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .with_context(|| format!("MailHog SMTP not accepting connections on port {p}"))?;
        Ok(())
    }

    fn env_vars(
        &self,
        dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> HashMap<String, String> {
        let p = smtp_port(dep).unwrap_or(1025);
        let mut vars = HashMap::new();
        vars.insert("SMTP_HOST".into(), "127.0.0.1".into());
        vars.insert("SMTP_PORT".into(), p.to_string());
        vars
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn dep_with_smtp_port(port: u64) -> Dependency {
        let mut extra = HashMap::new();
        extra.insert(
            "smtp_port".into(),
            crate::config::ExtraValue::Number(port.into()),
        );
        Dependency::with_extra("mailhog", extra)
    }

    #[test]
    fn mailhog_module_is_service() {
        assert!(MailhogModule.is_service());
    }

    #[test]
    fn smtp_port_defaults_to_1025() {
        let dep = Dependency::simple("mailhog");
        assert_eq!(smtp_port(&dep).unwrap(), 1025);
    }

    #[test]
    fn smtp_port_reads_custom_value() {
        let dep = dep_with_smtp_port(1026);
        assert_eq!(smtp_port(&dep).unwrap(), 1026);
    }

    #[test]
    fn smtp_port_bails_on_out_of_range() {
        let dep = dep_with_smtp_port(99999);
        assert!(smtp_port(&dep).is_err());
    }

    #[test]
    fn mailhog_health_check_fails_on_unused_port() {
        let dep = dep_with_smtp_port(19988);
        let err = MailhogModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19988"));
    }

    #[test]
    fn env_vars_includes_smtp_host_and_default_port() {
        let dep = Dependency::simple("mailhog");
        let vars = MailhogModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(vars.get("SMTP_HOST").map(|s| s.as_str()), Some("127.0.0.1"));
        assert_eq!(vars.get("SMTP_PORT").map(|s| s.as_str()), Some("1025"));
    }

    #[test]
    fn env_vars_reflects_custom_smtp_port() {
        let dep = dep_with_smtp_port(2525);
        let vars = MailhogModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(vars.get("SMTP_PORT").map(|s| s.as_str()), Some("2525"));
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "MailHog.MailHog");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "mailhog");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            MailhogModule
                .is_installed(&pm, &Dependency::simple("mailhog"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MailhogModule
                .is_installed(&pm, &Dependency::simple("mailhog"))
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
            MailhogModule
                .install(&pm, &Dependency::simple("mailhog"))
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
            MailhogModule
                .is_running(&pm, &Dependency::simple("mailhog"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MailhogModule
                .is_running(&pm, &Dependency::simple("mailhog"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MailhogModule
                .start(
                    &pm,
                    &Dependency::simple("mailhog"),
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
            MailhogModule
                .start(
                    &pm,
                    &Dependency::simple("mailhog"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MailhogModule
                .stop(&pm, &Dependency::simple("mailhog"))
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
            MailhogModule
                .stop(&pm, &Dependency::simple("mailhog"))
                .is_err()
        );
    }

    #[test]
    fn port_key_is_smtp_port() {
        assert_eq!(MailhogModule.port_key(), Some("smtp_port"));
    }

    #[test]
    fn resolve_ports_injects_into_smtp_port_key() {
        let mut deps = vec![Dependency::simple("mailhog")];
        let pm = crate::package_manager::MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        crate::commands::ports::resolve_ports(
            &mut deps,
            crate::commands::ports::PortSource::Lock(None),
            &pm,
            crate::commands::ports::PortMode::Assign,
        )
        .unwrap();
        assert!(
            deps[0].extra.contains_key("smtp_port"),
            "resolve_ports must inject into smtp_port, not port"
        );
        assert!(
            !deps[0].extra.contains_key("port"),
            "port key must not be set for mailhog"
        );
    }
}
