use anyhow::{Context, Result};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Dependency;
#[cfg(target_os = "linux")]
use crate::output;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep};

pub struct NginxModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "Nginx.Nginx",
        "nix" => "nginx",
        _ => "nginx",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 80)
}

/// A foreground nginx config that keeps every runtime file under `data_dir`.
fn nginx_conf(port: u16, data_dir: &std::path::Path) -> String {
    let path = |name: &str| super::quoted_conf_path(&data_dir.join(name));
    format!(
        "# devy-managed — rewritten on every start\n\
         daemon off;\n\
         worker_processes 1;\n\
         pid {pid};\n\
         error_log {error_log};\n\
         \n\
         events {{\n    worker_connections 64;\n}}\n\
         \n\
         http {{\n\
         \x20   access_log {access_log};\n\
         \x20   client_body_temp_path {client_body};\n\
         \x20   proxy_temp_path {proxy};\n\
         \x20   fastcgi_temp_path {fastcgi};\n\
         \x20   uwsgi_temp_path {uwsgi};\n\
         \x20   scgi_temp_path {scgi};\n\
         \n\
         \x20   server {{\n\
         \x20       listen 127.0.0.1:{port};\n\
         \x20   }}\n\
         }}\n",
        pid = path("nginx.pid"),
        error_log = path("error.log"),
        access_log = path("access.log"),
        client_body = path("client_body_temp"),
        proxy = path("proxy_temp"),
        fastcgi = path("fastcgi_temp"),
        uwsgi = path("uwsgi_temp"),
        scgi = path("scgi_temp"),
    )
}

impl Module for NginxModule {
    fn is_service(&self) -> bool {
        true
    }

    fn extra_log_paths(&self, data_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        vec![data_dir.join("error.log"), data_dir.join("access.log")]
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let conf = data_dir.join("nginx.conf");
        std::fs::write(&conf, nginx_conf(port(dep)?, data_dir))
            .with_context(|| format!("Failed to write {}", conf.display()))?;
        Ok(Some(super::LaunchSpec::new(
            "nginx",
            [
                "-p".into(),
                super::path_arg(data_dir),
                "-c".into(),
                super::path_arg(&conf),
            ],
        )))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("nginx".to_string())
    }

    fn docker_spec(&self, _dep: &Dependency) -> Result<Option<super::DockerSpec>> {
        Ok(Some(super::DockerSpec::new("nginx", "stable", 80)))
    }

    fn default_port(&self) -> Option<u16> {
        Some(80)
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
        #[cfg(target_os = "linux")]
        {
            let p = port(dep)?;
            if p < 1024 {
                output::warn(&format!(
                    "nginx: port {p} requires root or CAP_NET_BIND_SERVICE on Linux. \
                     Set a port >= 1024 in devy.yml if this fails."
                ));
            }
        }
        super::start_via_pm(self, pm, dep, project_root)
    }

    fn stop(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.stop_service(&self.service_name(dep))
    }

    fn health_check(&self, dep: &Dependency) -> Result<()> {
        let p = port(dep)?;
        let addr: SocketAddr = format!("127.0.0.1:{p}").parse()?;
        TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .with_context(|| format!("nginx not accepting connections on port {p}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_log_paths_are_error_and_access_logs() {
        let d = std::path::Path::new("/p/.devy/data/nginx");
        assert_eq!(
            NginxModule.extra_log_paths(d),
            [d.join("error.log"), d.join("access.log")]
        );
    }

    #[test]
    fn nginx_module_is_service() {
        assert!(NginxModule.is_service());
    }

    #[test]
    fn port_defaults_to_80() {
        let dep = Dependency::simple("nginx");
        assert_eq!(port(&dep).unwrap(), 80);
    }

    #[test]
    fn port_bails_on_out_of_range() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(99999u64.into()),
        );
        let dep = Dependency::with_extra("nginx", extra);
        assert!(port(&dep).is_err());
    }

    #[test]
    fn nginx_health_check_fails_on_unused_port() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(19995u64.into()),
        );
        let dep = Dependency::with_extra("nginx", extra);
        assert!(NginxModule.health_check(&dep).is_err());
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Nginx.Nginx");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "nginx");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            NginxModule
                .is_installed(&pm, &Dependency::simple("nginx"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !NginxModule
                .is_installed(&pm, &Dependency::simple("nginx"))
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
            NginxModule
                .install(&pm, &Dependency::simple("nginx"))
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
            NginxModule
                .is_running(&pm, &Dependency::simple("nginx"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !NginxModule
                .is_running(&pm, &Dependency::simple("nginx"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            NginxModule
                .start(
                    &pm,
                    &Dependency::simple("nginx"),
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
            NginxModule
                .start(
                    &pm,
                    &Dependency::simple("nginx"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(NginxModule.stop(&pm, &Dependency::simple("nginx")).is_ok());
    }

    #[test]
    fn stop_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            stop_service_fails: true,
            ..Default::default()
        };
        assert!(NginxModule.stop(&pm, &Dependency::simple("nginx")).is_err());
    }
}
