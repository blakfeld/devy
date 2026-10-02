use anyhow::Result;
use std::collections::HashMap;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep, tcp_ping};

pub struct MinioModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "MinIO.MinIO",
        "nix" => "minio",
        _ => "minio",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 9000)
}

impl Module for MinioModule {
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
            "server".to_string(),
            super::path_arg(data_dir),
            "--address".to_string(),
            format!("127.0.0.1:{p}"),
        ];
        if let Some(cp) = dep.extra.get("console_port").and_then(|v| v.as_u64()) {
            args.push("--console-address".into());
            args.push(format!(":{cp}"));
        }
        let mut env = Vec::new();
        if let Some(user) = dep.extra.get("access_key").and_then(|v| v.as_str()) {
            env.push(("MINIO_ROOT_USER".to_string(), user.to_string()));
        }
        if let Some(pass) = dep.extra.get("secret_key").and_then(|v| v.as_str()) {
            env.push(("MINIO_ROOT_PASSWORD".to_string(), pass.to_string()));
        }
        Ok(Some(super::LaunchSpec {
            env,
            ..super::LaunchSpec::new("minio", args)
        }))
    }

    fn nix_attr(&self, _dep: &crate::config::Dependency) -> Option<String> {
        Some("minio".to_string())
    }

    fn docker_spec(&self, dep: &Dependency) -> Result<Option<super::DockerSpec>> {
        let mut spec = super::DockerSpec {
            args: ["server", "/data", "--console-address", ":9001"]
                .map(String::from)
                .to_vec(),
            ..super::DockerSpec::new("minio/minio", "latest", 9000).data("/data")
        };
        if dep.extra.contains_key("console_port") {
            spec.extra_ports
                .push((super::extra_port(dep, "console_port", 9001)?, 9001));
        }
        if let Some(user) = dep.extra.get("access_key").and_then(|v| v.as_str()) {
            spec = spec.env(&[("MINIO_ROOT_USER", user)]);
        }
        if let Some(pass) = dep.extra.get("secret_key").and_then(|v| v.as_str()) {
            spec = spec.env(&[("MINIO_ROOT_PASSWORD", pass)]);
        }
        Ok(Some(spec))
    }

    fn default_port(&self) -> Option<u16> {
        Some(9000)
    }
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["port", "console_port", "access_key", "secret_key"])
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

    fn health_check(&self, dep: &Dependency) -> Result<()> {
        tcp_ping(port(dep)?, "MinIO")
    }

    fn config_warnings(&self, dep: &Dependency) -> Vec<String> {
        let has_creds =
            dep.extra.contains_key("access_key") || dep.extra.contains_key("secret_key");
        if has_creds {
            vec![
                "credentials in devy.yml will be written to plaintext .shadowenv.d — \
                 do not commit production credentials; consider ejson or a secrets manager"
                    .into(),
            ]
        } else {
            vec![]
        }
    }

    fn env_vars(
        &self,
        dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        if let Some(key) = dep.extra.get("access_key").and_then(|v| v.as_str()) {
            vars.insert("MINIO_ROOT_USER".into(), key.to_string());
        }
        if let Some(secret) = dep.extra.get("secret_key").and_then(|v| v.as_str()) {
            vars.insert("MINIO_ROOT_PASSWORD".into(), secret.to_string());
        }
        if let Some(cp) = dep.extra.get("console_port").and_then(|v| v.as_u64()) {
            vars.insert("MINIO_CONSOLE_ADDRESS".into(), format!(":{cp}"));
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
        Dependency::with_extra("minio", extra)
    }

    #[test]
    fn minio_module_is_service() {
        assert!(MinioModule.is_service());
    }

    #[test]
    fn port_defaults_to_9000() {
        let dep = Dependency::simple("minio");
        assert_eq!(port(&dep).unwrap(), 9000);
    }

    #[test]
    fn port_reads_custom_value() {
        let dep = dep_with_port(9001);
        assert_eq!(port(&dep).unwrap(), 9001);
    }

    #[test]
    fn minio_health_check_fails_on_unused_port() {
        let dep = dep_with_port(19989);
        let err = MinioModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19989"));
    }

    #[test]
    fn env_vars_empty_when_no_keys_configured() {
        let dep = Dependency::simple("minio");
        assert!(
            MinioModule
                .env_vars(&dep, std::path::Path::new("/tmp"))
                .is_empty()
        );
    }

    #[test]
    fn env_vars_includes_credentials_when_configured() {
        let mut extra = HashMap::new();
        extra.insert(
            "access_key".into(),
            crate::config::ExtraValue::String("myuser".into()),
        );
        extra.insert(
            "secret_key".into(),
            crate::config::ExtraValue::String("mypassword".into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        let vars = MinioModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("MINIO_ROOT_USER").map(|s| s.as_str()),
            Some("myuser")
        );
        assert_eq!(
            vars.get("MINIO_ROOT_PASSWORD").map(|s| s.as_str()),
            Some("mypassword")
        );
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "MinIO.MinIO");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "minio");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            MinioModule
                .is_installed(&pm, &Dependency::simple("minio"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MinioModule
                .is_installed(&pm, &Dependency::simple("minio"))
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
            MinioModule
                .install(&pm, &Dependency::simple("minio"))
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
            MinioModule
                .is_running(&pm, &Dependency::simple("minio"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !MinioModule
                .is_running(&pm, &Dependency::simple("minio"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            MinioModule
                .start(
                    &pm,
                    &Dependency::simple("minio"),
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
            MinioModule
                .start(
                    &pm,
                    &Dependency::simple("minio"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(MinioModule.stop(&pm, &Dependency::simple("minio")).is_ok());
    }

    #[test]
    fn stop_propagates_pm_error() {
        let pm = crate::package_manager::MockPackageManager {
            stop_service_fails: true,
            ..Default::default()
        };
        assert!(MinioModule.stop(&pm, &Dependency::simple("minio")).is_err());
    }

    #[test]
    fn env_vars_empty_returns_empty_not_non_empty() {
        let dep = Dependency::simple("minio");
        let vars = MinioModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert!(vars.is_empty());
    }

    #[test]
    fn env_vars_non_empty_returns_non_empty() {
        let mut extra = HashMap::new();
        extra.insert(
            "access_key".into(),
            crate::config::ExtraValue::String("user".into()),
        );
        extra.insert(
            "secret_key".into(),
            crate::config::ExtraValue::String("pass".into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        let vars = MinioModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert!(
            !vars.is_empty(),
            "Expected non-empty vars when credentials are configured"
        );
    }

    #[test]
    fn env_vars_does_not_warn_at_runtime() {
        // Runtime warning moved to config_warnings (fires at devy check time).
        // env_vars must be silent even when credentials are configured.
        let mut extra = HashMap::new();
        extra.insert(
            "access_key".into(),
            crate::config::ExtraValue::String("u".into()),
        );
        extra.insert(
            "secret_key".into(),
            crate::config::ExtraValue::String("p".into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        let warn_count = crate::output::with_warn_capture(|| {
            let _ = MinioModule.env_vars(&dep, std::path::Path::new("/tmp"));
        });
        assert_eq!(warn_count, 0, "env_vars must not warn at runtime");
    }

    #[test]
    fn env_vars_includes_console_address_when_configured() {
        let mut extra = HashMap::new();
        extra.insert(
            "console_port".into(),
            crate::config::ExtraValue::Number(9001u64.into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        let vars = MinioModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("MINIO_CONSOLE_ADDRESS").map(|s| s.as_str()),
            Some(":9001")
        );
    }

    #[test]
    fn env_vars_omits_console_address_when_not_configured() {
        let dep = Dependency::simple("minio");
        let vars = MinioModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert!(!vars.contains_key("MINIO_CONSOLE_ADDRESS"));
    }

    #[test]
    fn config_warnings_empty_when_no_credentials() {
        let dep = Dependency::simple("minio");
        assert!(MinioModule.config_warnings(&dep).is_empty());
    }

    #[test]
    fn config_warnings_non_empty_when_access_key_configured() {
        let mut extra = HashMap::new();
        extra.insert(
            "access_key".into(),
            crate::config::ExtraValue::String("u".into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        assert!(
            !MinioModule.config_warnings(&dep).is_empty(),
            "must warn when access_key is set"
        );
    }

    #[test]
    fn config_warnings_non_empty_when_secret_key_configured() {
        let mut extra = HashMap::new();
        extra.insert(
            "secret_key".into(),
            crate::config::ExtraValue::String("s".into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        assert!(
            !MinioModule.config_warnings(&dep).is_empty(),
            "must warn when secret_key is set"
        );
    }

    #[test]
    fn config_warnings_empty_when_only_console_port_set() {
        let mut extra = HashMap::new();
        extra.insert(
            "console_port".into(),
            crate::config::ExtraValue::Number(9001u64.into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        assert!(
            MinioModule.config_warnings(&dep).is_empty(),
            "console_port alone must not produce config warnings"
        );
    }

    #[test]
    fn env_vars_no_warn_when_only_console_port_set() {
        // console_port alone must not trigger the credentials warning.
        let mut extra = HashMap::new();
        extra.insert(
            "console_port".into(),
            crate::config::ExtraValue::Number(9001u64.into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        let warn_count = crate::output::with_warn_capture(|| {
            let _ = MinioModule.env_vars(&dep, std::path::Path::new("/tmp"));
        });
        assert_eq!(
            warn_count, 0,
            "console_port alone must not warn about credentials"
        );
    }
}
