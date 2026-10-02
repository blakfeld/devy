use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::config::Dependency;
use crate::output;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep, tcp_ping};

pub struct VaultModule;

// apt requires the HashiCorp apt repository; see https://developer.hashicorp.com/vault/install
fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "Hashicorp.Vault",
        "nix" => "vault",
        _ => "vault",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 8200)
}

fn dev_mode(dep: &Dependency) -> bool {
    dep.extra
        .get("dev_mode")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// A non-dev Vault server config: file storage under `data_dir` and a plain-HTTP
/// listener on 127.0.0.1. The server starts sealed; initialize and unseal it yourself.
fn vault_hcl(port: u16, data_dir: &std::path::Path) -> String {
    format!(
        "# devy-managed — rewritten on every start\n\
         storage \"file\" {{\n  path = {storage}\n}}\n\
         \n\
         listener \"tcp\" {{\n  address     = \"127.0.0.1:{port}\"\n  tls_disable = true\n}}\n\
         \n\
         api_addr      = \"http://127.0.0.1:{port}\"\n\
         disable_mlock = true\n",
        storage = super::quoted_conf_path(&data_dir.join("storage")),
    )
}

impl Module for VaultModule {
    fn is_service(&self) -> bool {
        true
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let p = port(dep)?;
        if dev_mode(dep) {
            return Ok(Some(super::LaunchSpec::new(
                "vault",
                [
                    "server".into(),
                    "-dev".into(),
                    format!("-dev-listen-address=127.0.0.1:{p}"),
                    "-dev-root-token-id=root".into(),
                ],
            )));
        }
        let conf = data_dir.join("vault.hcl");
        std::fs::write(&conf, vault_hcl(p, data_dir))
            .with_context(|| format!("Failed to write {}", conf.display()))?;
        Ok(Some(super::LaunchSpec::new(
            "vault",
            [
                "server".into(),
                format!("-config={}", super::path_arg(&conf)),
            ],
        )))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("vault".to_string())
    }

    fn nix_unfree(&self) -> bool {
        true
    }

    fn default_port(&self) -> Option<u16> {
        Some(8200)
    }
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["port", "dev_mode"])
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

    fn health_check(&self, dep: &Dependency) -> Result<()> {
        tcp_ping(port(dep)?, "Vault")
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        _pm: &dyn PackageManager,
        _project_root: &std::path::Path,
    ) -> Result<()> {
        // Validate port eagerly so an invalid value is caught before env_vars is called.
        // env_vars can't return Result, so it falls back to 8200 on error.
        let _ = port(dep)?;
        if dev_mode(dep) {
            output::warn(
                "Vault: VAULT_TOKEN=\"root\" (dev mode). Override via environment in devy.yml before deploying.",
            );
        }
        Ok(())
    }

    fn env_vars(
        &self,
        dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> HashMap<String, String> {
        let p = port(dep).unwrap_or(8200);
        let mut vars = HashMap::new();
        vars.insert("VAULT_ADDR".into(), format!("http://127.0.0.1:{p}"));
        if dev_mode(dep) {
            vars.insert("VAULT_TOKEN".into(), "root".into());
        }
        vars
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
        Dependency::with_extra("vault", extra)
    }

    #[test]
    fn vault_module_is_service() {
        assert!(VaultModule.is_service());
    }

    #[test]
    fn port_defaults_to_8200() {
        let dep = Dependency::simple("vault");
        assert_eq!(port(&dep).unwrap(), 8200);
    }

    #[test]
    fn port_reads_custom_value() {
        let dep = dep_with_port(8201);
        assert_eq!(port(&dep).unwrap(), 8201);
    }

    #[test]
    fn port_bails_on_out_of_range() {
        let dep = dep_with_port(99999);
        assert!(port(&dep).is_err());
    }

    #[test]
    fn vault_health_check_fails_on_unused_port() {
        let dep = dep_with_port(19986);
        let err = VaultModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19986"));
    }

    #[test]
    fn dev_mode_defaults_to_false() {
        let dep = Dependency::simple("vault");
        assert!(!dev_mode(&dep));
    }

    #[test]
    fn dev_mode_reads_true() {
        let mut extra = HashMap::new();
        extra.insert("dev_mode".into(), crate::config::ExtraValue::Bool(true));
        let dep = Dependency::with_extra("vault", extra);
        assert!(dev_mode(&dep));
    }

    #[test]
    fn env_vars_always_includes_vault_addr() {
        let dep = Dependency::simple("vault");
        let vars = VaultModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("VAULT_ADDR").map(|s| s.as_str()),
            Some("http://127.0.0.1:8200")
        );
        assert!(!vars.contains_key("VAULT_TOKEN"));
    }

    #[test]
    fn env_vars_includes_vault_token_in_dev_mode() {
        let mut extra = HashMap::new();
        extra.insert("dev_mode".into(), crate::config::ExtraValue::Bool(true));
        let dep = Dependency::with_extra("vault", extra);
        let vars = VaultModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(vars.get("VAULT_TOKEN").map(|s| s.as_str()), Some("root"));
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "Hashicorp.Vault");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "vault");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            VaultModule
                .is_installed(&pm, &Dependency::simple("vault"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !VaultModule
                .is_installed(&pm, &Dependency::simple("vault"))
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
            VaultModule
                .install(&pm, &Dependency::simple("vault"))
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
            VaultModule
                .is_running(&pm, &Dependency::simple("vault"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !VaultModule
                .is_running(&pm, &Dependency::simple("vault"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            VaultModule
                .start(
                    &pm,
                    &Dependency::simple("vault"),
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
            VaultModule
                .start(
                    &pm,
                    &Dependency::simple("vault"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(VaultModule.stop(&pm, &Dependency::simple("vault")).is_ok());
    }

    #[test]
    fn stop_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            stop_service_fails: true,
            ..Default::default()
        };
        assert!(VaultModule.stop(&pm, &Dependency::simple("vault")).is_err());
    }

    #[test]
    fn env_vars_addr_uses_custom_port() {
        let dep = dep_with_port(8201);
        let vars = VaultModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("VAULT_ADDR").map(|s| s.as_str()),
            Some("http://127.0.0.1:8201")
        );
    }

    #[test]
    fn post_setup_warns_in_dev_mode() {
        let mut extra = HashMap::new();
        extra.insert("dev_mode".into(), crate::config::ExtraValue::Bool(true));
        let dep = Dependency::with_extra("vault", extra);
        let pm = crate::package_manager::MockPackageManager::default();
        let count = crate::output::with_warn_capture(|| {
            VaultModule
                .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
                .unwrap();
        });
        assert_eq!(count, 1, "post_setup must warn when dev_mode is enabled");
    }

    #[test]
    fn post_setup_returns_err_on_invalid_port() {
        let dep = dep_with_port(99999);
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            VaultModule
                .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
                .is_err(),
            "post_setup must fail when port is out of range"
        );
    }

    #[test]
    fn post_setup_no_warn_without_dev_mode() {
        let dep = Dependency::simple("vault");
        let pm = crate::package_manager::MockPackageManager::default();
        let count = crate::output::with_warn_capture(|| {
            VaultModule
                .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
                .unwrap();
        });
        assert_eq!(count, 0, "post_setup must not warn when dev_mode is off");
    }
}
