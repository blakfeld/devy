use anyhow::{Context, Result};
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use crate::output;

use super::{Module, write_mysql_config};

pub struct MysqlModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "apt" => "mysql-server",
        "winget" => "Oracle.MySQL",
        "nix" => "mysql84",
        _ => "mysql",
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

impl Module for MysqlModule {
    fn is_service(&self) -> bool {
        true
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let init = vec![
            "mysqld".to_string(),
            "--no-defaults".to_string(),
            "--initialize-insecure".to_string(),
            format!("--datadir={}", super::path_arg(data_dir)),
        ];
        // The X Protocol plugin would otherwise bind *:33060 and /tmp/mysqlx.sock,
        // colliding across projects and with a system mysqld.
        let spec = super::helpers::mysql_family_launch(
            "mysqld",
            init,
            &["--mysqlx=OFF"],
            port(dep)?,
            cli_args(dep).as_deref(),
            data_dir,
        )?;
        // mariadb wins profile name conflicts (it also ships `bin/mysqld`), so always run
        // MySQL's own server from its package.
        Ok(Some(super::LaunchSpec {
            exec_package: Some(super::nix_install_attr(self, dep, "mysql84")),
            ..spec
        }))
    }

    fn nix_attr(&self, dep: &crate::config::Dependency) -> Option<String> {
        Some(super::nix_install_attr(self, dep, "mysql84"))
    }

    fn default_port(&self) -> Option<u16> {
        Some(3306)
    }
    fn port_applicable(&self, pm: &dyn PackageManager) -> bool {
        pm.name() == "nix" || pm.service_config_dir("mysql").is_some()
    }

    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["port", "cli_args"])
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        super::pkg_installed(self, pm, dep, package_name(pm))
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.install_package(&super::pkg_dep(self, pm, dep, package_name(pm)))
    }

    fn nix_versioned_attr(&self, version: &str) -> Option<String> {
        // `8.4` or `8.4.3` → `mysql84`. nixpkgs dropped mysql80.
        super::helpers::allowlisted_attr(version, 2, &["8.4"], |v| {
            format!("mysql{}", v.replace('.', ""))
        })
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        pm: &dyn PackageManager,
        _project_root: &std::path::Path,
    ) -> Result<()> {
        let p = port(dep)?;
        let args = cli_args(dep);
        if p != 3306 || args.is_some() {
            match pm.service_config_dir("mysql") {
                Some(config_dir) => write_mysql_config(&config_dir, p, args.as_deref())?,
                // Under nix the port and cli_args go on the command line instead. An
                // unapplied explicit port is reported by the shared port resolver.
                None if pm.name() != "nix" && args.is_some() => {
                    output::warn(&format!(
                        "cli_args ignored: {} does not support service config dirs",
                        pm.name()
                    ));
                }
                None => {}
            }
        }
        Ok(())
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
            .with_context(|| format!("MySQL not accepting connections on port {p}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        // MySQL sends a 4-byte packet header on connect; first payload byte is 0x0a (protocol v10).
        let mut header = [0u8; 5];
        stream.read_exact(&mut header)?;
        anyhow::ensure!(
            header[4] == 0x0a,
            "MySQL on port {p} returned unexpected protocol byte"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn dep_with_extra(extra: HashMap<String, crate::config::ExtraValue>) -> Dependency {
        Dependency {
            name: "mysql".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
        }
    }

    // ── env_vars ──────────────────────────────────────────────────────────────

    #[test]
    fn env_vars_default_port() {
        let dep = Dependency::simple("mysql");
        let vars = MysqlModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("DATABASE_URL").map(|s| s.as_str()),
            Some("mysql://root@127.0.0.1:3306/")
        );
    }

    #[test]
    fn env_vars_custom_port() {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(3307u64.into()),
        );
        let dep = dep_with_extra(extra);
        let vars = MysqlModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("DATABASE_URL").map(|s| s.as_str()),
            Some("mysql://root@127.0.0.1:3307/")
        );
    }

    // ── port ──────────────────────────────────────────────────────────────────

    #[test]
    fn port_defaults_to_3306() {
        let dep = Dependency::simple("mysql");
        assert_eq!(port(&dep).unwrap(), 3306);
    }

    #[test]
    fn port_reads_custom_value() {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(3307.into()),
        );
        let dep = dep_with_extra(extra);
        assert_eq!(port(&dep).unwrap(), 3307);
    }

    #[test]
    fn port_ignores_non_numeric_value() {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::String("bogus".into()),
        );
        let dep = dep_with_extra(extra);
        assert_eq!(port(&dep).unwrap(), 3306);
    }

    #[test]
    fn port_bails_on_out_of_range() {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(99999.into()),
        );
        let dep = dep_with_extra(extra);
        assert!(port(&dep).is_err());
    }

    // ── cli_args ──────────────────────────────────────────────────────────────

    #[test]
    fn cli_args_absent_returns_none() {
        let dep = Dependency::simple("mysql");
        assert!(cli_args(&dep).is_none());
    }

    #[test]
    fn cli_args_present_returns_string() {
        let mut extra = HashMap::new();
        extra.insert(
            "cli_args".into(),
            crate::config::ExtraValue::String("--innodb-buffer-pool-size=256M".into()),
        );
        let dep = dep_with_extra(extra);
        assert_eq!(
            cli_args(&dep).as_deref(),
            Some("--innodb-buffer-pool-size=256M")
        );
    }

    // ── MysqlModule trait methods ─────────────────────────────────────────────

    #[test]
    fn mysql_module_is_service() {
        assert!(MysqlModule.is_service());
    }

    #[test]
    fn mysql_health_check_fails_on_unused_port() {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(19999u64.into()),
        );
        let dep = dep_with_extra(extra);
        let err = MysqlModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19999"));
    }

    #[test]
    fn package_name_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "mysql-server");
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Oracle.MySQL");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "mysql");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            MysqlModule
                .is_installed(&pm, &Dependency::simple("mysql"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MysqlModule
                .is_installed(&pm, &Dependency::simple("mysql"))
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
            MysqlModule
                .install(&pm, &Dependency::simple("mysql"))
                .is_err()
        );
    }

    #[test]
    fn post_setup_writes_config_when_custom_port_and_config_dir_available() {
        let dir = std::env::temp_dir().join(format!("devy_mysql_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pm = crate::package_manager::MockPackageManager {
            config_dir: Some(dir.clone()),
            ..Default::default()
        };
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(3307u64.into()),
        );
        let dep = dep_with_extra(extra);
        MysqlModule
            .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
            .unwrap();
        let content = std::fs::read_to_string(dir.join("my.cnf")).unwrap();
        assert!(content.contains("port = 3307"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn post_setup_writes_config_when_default_port_but_cli_args_set() {
        let dir = std::env::temp_dir().join(format!("devy_mysql_test_args_{}", std::process::id()));
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
        let dep = dep_with_extra(extra);
        MysqlModule
            .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
            .unwrap();
        let content = std::fs::read_to_string(dir.join("my.cnf")).unwrap();
        assert!(content.contains("innodb-buffer-pool-size"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn post_setup_skips_config_when_default_port_no_args() {
        let dir = std::env::temp_dir().join(format!("devy_mysql_test_skip_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pm = crate::package_manager::MockPackageManager {
            config_dir: Some(dir.clone()),
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        MysqlModule
            .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
            .unwrap();
        assert!(!dir.join("my.cnf").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_running_true() {
        let pm = crate::package_manager::MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        assert!(
            MysqlModule
                .is_running(&pm, &Dependency::simple("mysql"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MysqlModule
                .is_running(&pm, &Dependency::simple("mysql"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MysqlModule
                .start(
                    &pm,
                    &Dependency::simple("mysql"),
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
            MysqlModule
                .start(
                    &pm,
                    &Dependency::simple("mysql"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(MysqlModule.stop(&pm, &Dependency::simple("mysql")).is_ok());
    }

    #[test]
    fn stop_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            stop_service_fails: true,
            ..Default::default()
        };
        assert!(MysqlModule.stop(&pm, &Dependency::simple("mysql")).is_err());
    }
}
