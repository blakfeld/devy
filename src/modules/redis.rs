use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep};

pub struct RedisModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "apt" => "redis-server",
        "winget" => "Redis.Redis",
        "nix" => "redis",
        _ => "redis",
    }
}

impl Module for RedisModule {
    fn is_service(&self) -> bool {
        true
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let port = super::extra_port(dep, "port", 6379)?;
        Ok(Some(super::LaunchSpec::new(
            "redis-server",
            [
                "--port".into(),
                port.to_string(),
                "--bind".into(),
                "127.0.0.1".into(),
                "--dir".into(),
                super::path_arg(data_dir),
            ],
        )))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("redis".to_string())
    }

    fn default_port(&self) -> Option<u16> {
        Some(6379)
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

    fn env_vars(
        &self,
        dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> std::collections::HashMap<String, String> {
        let port = super::extra_port(dep, "port", 6379).unwrap_or(6379);
        let mut map = std::collections::HashMap::new();
        map.insert("REDIS_URL".into(), format!("redis://127.0.0.1:{port}"));
        map
    }

    fn health_check(&self, dep: &Dependency) -> Result<()> {
        let port = super::extra_port(dep, "port", 6379)?;
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .with_context(|| format!("Redis not accepting connections on port {port}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(b"PING\r\n")?;
        let mut buf = [0u8; 7];
        stream.read_exact(&mut buf)?;
        anyhow::ensure!(
            &buf == b"+PONG\r\n",
            "Redis on port {port} returned unexpected PING response"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    #[test]
    fn redis_module_is_service() {
        assert!(RedisModule.is_service());
    }

    #[test]
    fn env_vars_default_port() {
        let dep = Dependency::simple("redis");
        let vars = RedisModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("REDIS_URL").map(|s| s.as_str()),
            Some("redis://127.0.0.1:6379")
        );
    }

    #[test]
    fn env_vars_custom_port() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(6380u64.into()),
        );
        let dep = Dependency::with_extra("redis", extra);
        let vars = RedisModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("REDIS_URL").map(|s| s.as_str()),
            Some("redis://127.0.0.1:6380")
        );
    }

    #[test]
    fn redis_health_check_fails_on_unused_port() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(19998u64.into()),
        );
        let dep = Dependency::with_extra("redis", extra);
        assert!(RedisModule.health_check(&dep).is_err());
    }

    #[test]
    fn package_name_apt() {
        let pm = MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "redis-server");
    }

    #[test]
    fn package_name_winget() {
        let pm = MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Redis.Redis");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "redis");
    }

    #[test]
    fn is_installed_true() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            RedisModule
                .is_installed(&pm, &Dependency::simple("redis"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = MockPackageManager::default();
        assert!(
            !RedisModule
                .is_installed(&pm, &Dependency::simple("redis"))
                .unwrap()
        );
    }

    #[test]
    fn install_propagates_pm_error() {
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        assert!(
            RedisModule
                .install(&pm, &Dependency::simple("redis"))
                .is_err()
        );
    }

    #[test]
    fn is_running_true() {
        let pm = MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        assert!(
            RedisModule
                .is_running(&pm, &Dependency::simple("redis"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = MockPackageManager::default();
        assert!(
            !RedisModule
                .is_running(&pm, &Dependency::simple("redis"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = MockPackageManager::default();
        assert!(
            RedisModule
                .start(
                    &pm,
                    &Dependency::simple("redis"),
                    std::path::Path::new("/tmp")
                )
                .is_ok()
        );
    }

    #[test]
    fn start_propagates_pm_error() {
        let pm = MockPackageManager {
            start_service_fails: true,
            ..Default::default()
        };
        assert!(
            RedisModule
                .start(
                    &pm,
                    &Dependency::simple("redis"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = MockPackageManager::default();
        assert!(RedisModule.stop(&pm, &Dependency::simple("redis")).is_ok());
    }

    #[test]
    fn stop_propagates_pm_error() {
        let pm = MockPackageManager {
            stop_service_fails: true,
            ..Default::default()
        };
        assert!(RedisModule.stop(&pm, &Dependency::simple("redis")).is_err());
    }
}
