use anyhow::{Context, Result};
use std::borrow::Cow;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep};

pub struct MongodbModule;

// brew requires the mongodb/brew tap; users should add `tap: mongodb/brew` in devy.yml.
// nix: mongodb-ce is the prebuilt binary package (mongodb builds from source). Both are
// unfree, so installing needs NIXPKGS_ALLOW_UNFREE=1.
fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "apt" => "mongodb-org",
        "winget" => "MongoDB.Server",
        "nix" => "mongodb-ce",
        _ => "mongodb-community",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 27017)
}

impl Module for MongodbModule {
    fn is_service(&self) -> bool {
        true
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("mongodb-ce".to_string())
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let p = port(dep)?;
        Ok(Some(super::LaunchSpec::new(
            "mongod",
            [
                "--port".into(),
                p.to_string(),
                "--bind_ip".into(),
                "127.0.0.1".into(),
                "--dbpath".into(),
                super::path_arg(data_dir),
            ],
        )))
    }

    fn default_port(&self) -> Option<u16> {
        Some(27017)
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

    fn service_name<'a>(&self, _dep: &'a Dependency) -> Cow<'a, str> {
        if cfg!(target_os = "linux") {
            Cow::Borrowed("mongod")
        } else {
            Cow::Borrowed("mongodb-community")
        }
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
        let p = port(dep)?;
        let addr: SocketAddr = format!("127.0.0.1:{p}").parse()?;
        TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .with_context(|| format!("MongoDB not accepting connections on port {p}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mongodb_module_is_service() {
        assert!(MongodbModule.is_service());
    }

    #[test]
    fn service_name_ignores_dep_name() {
        let expected = if cfg!(target_os = "linux") {
            "mongod"
        } else {
            "mongodb-community"
        };
        let dep = Dependency::simple("mongodb");
        assert_eq!(MongodbModule.service_name(&dep).as_ref(), expected);
        let dep2 = Dependency::simple("mongo");
        assert_eq!(MongodbModule.service_name(&dep2).as_ref(), expected);
    }

    #[test]
    fn port_defaults_to_27017() {
        let dep = Dependency::simple("mongodb");
        assert_eq!(port(&dep).unwrap(), 27017);
    }

    #[test]
    fn mongodb_health_check_fails_on_unused_port() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(19983u64.into()),
        );
        let dep = Dependency::with_extra("mongodb", extra);
        assert!(MongodbModule.health_check(&dep).is_err());
    }

    #[test]
    fn package_name_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "mongodb-org");
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "MongoDB.Server");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "mongodb-community");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            MongodbModule
                .is_installed(&pm, &Dependency::simple("mongodb"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MongodbModule
                .is_installed(&pm, &Dependency::simple("mongodb"))
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
            MongodbModule
                .install(&pm, &Dependency::simple("mongodb"))
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
            MongodbModule
                .is_running(&pm, &Dependency::simple("mongodb"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MongodbModule
                .is_running(&pm, &Dependency::simple("mongodb"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MongodbModule
                .start(
                    &pm,
                    &Dependency::simple("mongodb"),
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
            MongodbModule
                .start(
                    &pm,
                    &Dependency::simple("mongodb"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MongodbModule
                .stop(&pm, &Dependency::simple("mongodb"))
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
            MongodbModule
                .stop(&pm, &Dependency::simple("mongodb"))
                .is_err()
        );
    }
}
