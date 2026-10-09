use anyhow::{Context, Result};
use std::borrow::Cow;
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep};

pub struct MariadbModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "apt" => "mariadb-server",
        "winget" => "MariaDB.Server",
        "nix" => "mariadb",
        _ => "mariadb",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 3306)
}

fn cli_args(dep: &Dependency) -> Option<String> {
    dep.extra
        .get("cli_args")
        .and_then(|v| v.as_str())
        .map(String::from)
}

impl Module for MariadbModule {
    fn is_service(&self) -> bool {
        true
    }
    fn default_port(&self) -> Option<u16> {
        Some(3306)
    }

    fn explicit_port_via_config(&self, pm: &dyn PackageManager) -> bool {
        matches!(pm.name(), "brew" | "apt")
    }

    fn backend_config_warnings(&self, dep: &Dependency, pm: &dyn PackageManager) -> Vec<String> {
        super::helpers::mysql_family_backend_config_warnings("mariadb", dep, pm)
    }

    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["port", "cli_args"])
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        pm.is_package_installed(&pm_dep(dep, package_name(pm)))
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.install_package(&pm_dep(dep, package_name(pm)))
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        pm: &dyn PackageManager,
        _project_root: &std::path::Path,
    ) -> Result<()> {
        let p = port(dep)?;
        super::helpers::mysql_family_post_setup(pm, "mariadb", p, cli_args(dep).as_deref())
    }

    fn env_vars(
        &self,
        dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> std::collections::HashMap<String, String> {
        let p = port(dep).unwrap_or(3306);
        let mut map = std::collections::HashMap::new();
        map.insert(
            "DATABASE_URL".into(),
            format!("mysql://root@127.0.0.1:{p}/"),
        );
        map
    }

    /// MariaDB's PM service is always "mariadb" — older brew formulas may have registered
    /// it as "mysql", but current formulae use the correct name.
    fn service_name<'a>(&self, _dep: &'a Dependency) -> Cow<'a, str> {
        Cow::Borrowed("mariadb")
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        // Password (not unix_socket) auth for root, so DATABASE_URL works over TCP.
        let init = vec![
            "mariadb-install-db".to_string(),
            "--no-defaults".to_string(),
            format!("--datadir={}", super::path_arg(data_dir)),
            "--auth-root-authentication-method=normal".to_string(),
        ];
        Ok(Some(super::helpers::mysql_family_launch(
            "mariadbd",
            init,
            &[],
            port(dep)?,
            cli_args(dep).as_deref(),
            data_dir,
        )?))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("mariadb".to_string())
    }

    fn docker_spec(&self, dep: &Dependency) -> Result<Option<super::DockerSpec>> {
        Ok(Some(super::DockerSpec {
            args: super::helpers::mysql_server_args(cli_args(dep).as_deref()),
            ..super::DockerSpec::new("mariadb", "11", 3306)
                .data("/var/lib/mysql")
                .env(&[("MARIADB_ALLOW_EMPTY_ROOT_PASSWORD", "1")])
        }))
    }

    fn post_setup_writes_service_config(&self) -> bool {
        true
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
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .with_context(|| format!("MariaDB not accepting connections on port {p}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        // MariaDB/MySQL protocol: 4-byte packet header, then payload begins with 0x0a (protocol v10).
        let mut header = [0u8; 5];
        stream.read_exact(&mut header)?;
        anyhow::ensure!(
            header[4] == 0x0a,
            "MariaDB on port {p} returned unexpected protocol byte"
        );
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
        Dependency::with_extra("mariadb", extra)
    }

    #[test]
    fn env_vars_default_port() {
        let dep = Dependency::simple("mariadb");
        let vars = MariadbModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("DATABASE_URL").map(|s| s.as_str()),
            Some("mysql://root@127.0.0.1:3306/")
        );
    }

    #[test]
    fn env_vars_custom_port() {
        let dep = dep_with_port(3307);
        let vars = MariadbModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("DATABASE_URL").map(|s| s.as_str()),
            Some("mysql://root@127.0.0.1:3307/")
        );
    }

    #[test]
    fn mariadb_module_is_service() {
        assert!(MariadbModule.is_service());
    }

    #[test]
    fn port_defaults_to_3306() {
        let dep = Dependency::simple("mariadb");
        assert_eq!(port(&dep).unwrap(), 3306);
    }

    #[test]
    fn port_reads_custom_value() {
        let dep = dep_with_port(3307);
        assert_eq!(port(&dep).unwrap(), 3307);
    }

    #[test]
    fn port_bails_on_out_of_range() {
        let dep = dep_with_port(99999);
        assert!(port(&dep).is_err());
    }

    #[test]
    fn mariadb_health_check_fails_on_unused_port() {
        let dep = dep_with_port(19987);
        let err = MariadbModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19987"));
    }

    #[test]
    fn cli_args_absent_returns_none() {
        let dep = Dependency::simple("mariadb");
        assert!(cli_args(&dep).is_none());
    }

    #[test]
    fn cli_args_present_returns_string() {
        let mut extra = HashMap::new();
        extra.insert(
            "cli_args".into(),
            crate::config::ExtraValue::String("--innodb-buffer-pool-size=256M".into()),
        );
        let dep = Dependency::with_extra("mariadb", extra);
        assert_eq!(
            cli_args(&dep).as_deref(),
            Some("--innodb-buffer-pool-size=256M")
        );
    }

    #[test]
    fn package_name_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "mariadb-server");
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "MariaDB.Server");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "mariadb");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            MariadbModule
                .is_installed(&pm, &Dependency::simple("mariadb"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MariadbModule
                .is_installed(&pm, &Dependency::simple("mariadb"))
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
            MariadbModule
                .install(&pm, &Dependency::simple("mariadb"))
                .is_err()
        );
    }

    #[test]
    fn post_setup_writes_config_when_default_port_but_cli_args_set() {
        let dir = std::env::temp_dir().join(format!("devy_mariadb_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pm = crate::package_manager::MockPackageManager {
            config_dir: Some(dir.clone()),
            ..Default::default()
        };
        let mut extra = HashMap::new();
        extra.insert(
            "cli_args".into(),
            crate::config::ExtraValue::String("--innodb-buffer-pool-size=256M".into()),
        );
        let dep = Dependency::with_extra("mariadb", extra);
        MariadbModule
            .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
            .unwrap();
        let content = std::fs::read_to_string(dir.join("devy.cnf")).unwrap();
        assert!(content.contains("innodb-buffer-pool-size"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn post_setup_skips_config_when_default_port_no_args() {
        let dir = std::env::temp_dir().join(format!("devy_mariadb_skip_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pm = crate::package_manager::MockPackageManager {
            config_dir: Some(dir.clone()),
            ..Default::default()
        };
        let dep = Dependency::simple("mariadb");
        MariadbModule
            .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
            .unwrap();
        assert!(!dir.join("devy.cnf").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn service_name_is_always_mariadb() {
        let dep = Dependency::simple("mariadb");
        assert_eq!(MariadbModule.service_name(&dep).as_ref(), "mariadb");
        // Ensure it ignores dep.name
        let dep2 = Dependency::simple("something-else");
        assert_eq!(MariadbModule.service_name(&dep2).as_ref(), "mariadb");
    }

    #[test]
    fn is_running_true() {
        let pm = crate::package_manager::MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        assert!(
            MariadbModule
                .is_running(&pm, &Dependency::simple("mariadb"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MariadbModule
                .is_running(&pm, &Dependency::simple("mariadb"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MariadbModule
                .start(
                    &pm,
                    &Dependency::simple("mariadb"),
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
            MariadbModule
                .start(
                    &pm,
                    &Dependency::simple("mariadb"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MariadbModule
                .stop(&pm, &Dependency::simple("mariadb"))
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
            MariadbModule
                .stop(&pm, &Dependency::simple("mariadb"))
                .is_err()
        );
    }
}
