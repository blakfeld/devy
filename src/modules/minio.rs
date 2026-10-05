use anyhow::Result;
use std::collections::HashMap;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::{Module, pm_dep, tcp_ping};

/// Default container image: Silo, the maintained community fork of MinIO (see
/// `docker_spec`).
const DEFAULT_IMAGE: &str = "pgsty/silo";
const DEFAULT_TAG: &str = "RELEASE.2026-09-16T00-00-00Z";
/// Digest of `DEFAULT_TAG`'s multi-arch (amd64, arm64) OCI image index on Docker Hub.
/// `devy up --update` keeps this pin, so bump `DEFAULT_TAG` and this digest together
/// with each pgsty/silo security release.
const DEFAULT_DIGEST: &str =
    "sha256:635197cb9f36d01bee221d34d1c7d7960f6a95c48b0b6c01d99cd13bdae51a46";

pub struct MinioModule;

/// Warning shown by `devy check` and `devy up` when MinIO runs with its built-in login.
const DEFAULT_CREDENTIALS_WARNING: &str = "access_key and secret_key are not both set — \
     MinIO will use its default minioadmin credentials";

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

/// The configured `console_port`, validated like other port keys. `None` when unset.
fn console_port(dep: &Dependency) -> Result<Option<u16>> {
    let Some(value) = dep.extra.get("console_port") else {
        return Ok(None);
    };
    if value.as_u64().is_none() {
        anyhow::bail!("console_port must be a number between 1 and 65535");
    }
    super::extra_port(dep, "console_port", 0).map(Some)
}

/// The console port for a nix launch: the configured one, else a free port chosen once
/// and kept in `<data_dir>/console_port` so the console URL is stable across starts.
fn launch_console_port(dep: &Dependency, data_dir: &std::path::Path) -> Result<u16> {
    if let Some(p) = console_port(dep)? {
        return Ok(p);
    }
    super::helpers::persisted_port(
        &data_dir.join("console_port"),
        &[port(dep)?],
        "MinIO console",
    )
}

/// Whether both credentials will actually reach MinIO (`credentials` ignores
/// non-string values, which would leave the default login in place).
fn has_all_credentials(dep: &Dependency) -> bool {
    credentials(dep).len() == 2
}

fn credentials(dep: &Dependency) -> Vec<(&'static str, &str)> {
    let mut out = Vec::new();
    if let Some(user) = dep.extra.get("access_key").and_then(|v| v.as_str()) {
        out.push(("MINIO_ROOT_USER", user));
    }
    if let Some(pass) = dep.extra.get("secret_key").and_then(|v| v.as_str()) {
        out.push(("MINIO_ROOT_PASSWORD", pass));
    }
    out
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
        // Always pass the console address: MinIO otherwise binds the console to a random
        // port on every interface.
        let console = launch_console_port(dep, data_dir)?;
        let args = vec![
            "server".to_string(),
            super::path_arg(data_dir),
            "--address".to_string(),
            format!("127.0.0.1:{p}"),
            "--console-address".to_string(),
            format!("127.0.0.1:{console}"),
        ];
        let env = credentials(dep)
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
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
            // MinIO no longer publishes `minio/minio`. pgsty/silo is the maintained
            // continuation of the pgsty/minio community fork (whose final release,
            // RELEASE.2026-08-04T00-00-00Z, gets no further fixes). Silo keeps the MinIO
            // server flags, `MINIO_*` variables and on-disk format, and its entrypoint
            // maps `server ...` to `silo server ...`, so existing volumes keep working.
            // The tag is pinned by its index digest so a re-pushed tag can't change what
            // runs.
            ..super::DockerSpec::new(DEFAULT_IMAGE, DEFAULT_TAG, 9000)
                .pinned(DEFAULT_DIGEST)
                .data("/data")
                .secret_env(&credentials(dep))
        };
        if let Some(cp) = console_port(dep)? {
            spec.extra_ports.push((cp, 9001));
        }
        Ok(Some(spec))
    }

    fn docker_warnings(&self, dep: &Dependency) -> Vec<String> {
        // MinIO stopped publishing community images: `minio/minio` on Docker Hub gets no
        // new tags, and `quay.io/minio/minio` now requires authentication.
        let (repository, _) = super::split_image_tag(dep.image.as_deref().unwrap_or(""));
        let repository = ["docker.io/", "index.docker.io/", "registry-1.docker.io/"]
            .iter()
            .find_map(|prefix| repository.strip_prefix(prefix))
            .unwrap_or(repository);
        match repository {
            "minio/minio" | "quay.io/minio/minio" => vec![format!(
                "{repository} is no longer published and gets no security fixes; consider pgsty/silo"
            )],
            _ => vec![],
        }
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

    fn post_setup(
        &self,
        dep: &Dependency,
        _pm: &dyn PackageManager,
        _project_root: &std::path::Path,
    ) -> Result<()> {
        if !has_all_credentials(dep) {
            crate::output::warn(&format!("{}: {DEFAULT_CREDENTIALS_WARNING}", dep.name));
        }
        Ok(())
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
        let mut warnings = Vec::new();
        let has_creds =
            dep.extra.contains_key("access_key") || dep.extra.contains_key("secret_key");
        if has_creds {
            warnings.push(
                "credentials in devy.yml will be written to plaintext .shadowenv.d — \
                 do not commit production credentials; consider ejson or a secrets manager"
                    .into(),
            );
        }
        if !has_all_credentials(dep) {
            warnings.push(DEFAULT_CREDENTIALS_WARNING.into());
        }
        warnings
    }

    fn config_issues(&self, dep: &Dependency) -> Vec<String> {
        let mut issues: Vec<String> = ["access_key", "secret_key"]
            .into_iter()
            .filter_map(|key| super::helpers::control_char_issue(dep, key))
            .collect();
        if let Err(e) = console_port(dep) {
            issues.push(e.to_string());
        }
        issues
    }

    fn env_vars(
        &self,
        dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> HashMap<String, String> {
        let mut vars: HashMap<String, String> = credentials(dep)
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Ok(Some(cp)) = console_port(dep) {
            vars.insert("MINIO_CONSOLE_ADDRESS".into(), format!("127.0.0.1:{cp}"));
        }
        vars
    }

    /// Under nix without `console_port`, devy picks the console port at launch and keeps
    /// it in `<data_dir>/console_port`. `devy up` (`PortMode::Assign`) resolves it the
    /// same way here, choosing and recording it before the service first starts, so the
    /// exported address is the one MinIO listens on. Read-only commands only read an
    /// already recorded port and write nothing. Other backends do not use a devy-chosen
    /// console port.
    fn backend_env_vars(
        &self,
        dep: &Dependency,
        pm: &dyn PackageManager,
        project_root: &std::path::Path,
        mode: crate::commands::ports::PortMode,
    ) -> HashMap<String, String> {
        use crate::commands::ports::PortMode;
        let mut vars = HashMap::new();
        if pm.name() != "nix" || dep.docker || !matches!(console_port(dep), Ok(None)) {
            return vars;
        }
        let Ok(main) = port(dep) else {
            return vars;
        };
        let data_dir = super::nix_data_dir(project_root, super::canonical_name(&dep.name));
        let console = match mode {
            PortMode::ReadOnly => recorded_console_port(&data_dir, main),
            // Refuses a symlinked component, like `nix_launch_for`, so a repository can't
            // redirect the port file out of the project.
            PortMode::Assign => crate::fs_safe::ensure_devy_subdir(project_root, &data_dir)
                .and_then(|()| launch_console_port(dep, &data_dir))
                .inspect_err(|e| {
                    crate::output::warn(&format!(
                        "{}: could not record the MinIO console port, so MINIO_CONSOLE_ADDRESS is not set: {e:#}",
                        dep.name
                    ));
                })
                .ok(),
        };
        if let Some(cp) = console {
            vars.insert("MINIO_CONSOLE_ADDRESS".into(), format!("127.0.0.1:{cp}"));
        }
        vars
    }
}

/// The console port recorded in `<data_dir>/console_port` by an earlier launch or
/// `devy up`, if it is a regular file holding a usable port (the same rule
/// `persisted_port` applies). Never creates, deletes or writes anything.
fn recorded_console_port(data_dir: &std::path::Path, main: u16) -> Option<u16> {
    let file = data_dir.join("console_port");
    if !std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) {
        return None;
    }
    super::helpers::read_port_file(&file).filter(|p| *p != 0 && *p != main)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_credentials_still_get_the_default_credentials_warning() {
        let mut dep = Dependency::simple("minio");
        for key in ["access_key", "secret_key"] {
            dep.extra.insert(
                key.into(),
                crate::config::ExtraValue::Number(12345u64.into()),
            );
        }
        assert!(
            MinioModule
                .config_warnings(&dep)
                .iter()
                .any(|w| w == DEFAULT_CREDENTIALS_WARNING)
        );
    }

    #[test]
    fn invalid_console_port_and_control_chars_are_issues() {
        let mut dep = Dependency::simple("minio");
        assert!(MinioModule.config_issues(&dep).is_empty());
        dep.extra.insert(
            "console_port".into(),
            crate::config::ExtraValue::Number(70000u64.into()),
        );
        dep.extra.insert(
            "secret_key".into(),
            crate::config::ExtraValue::String("x\rExecStartPre=/bin/sh".into()),
        );
        let issues = MinioModule.config_issues(&dep);
        assert_eq!(issues.len(), 2, "{issues:?}");
        assert!(issues.iter().any(|i| i.contains("secret_key")));
        assert!(issues.iter().all(|i| !i.contains("ExecStartPre")));
        assert!(
            MinioModule
                .config_warnings(&dep)
                .iter()
                .all(|w| !w.contains("console_port"))
        );
    }

    #[test]
    fn launch_console_port_avoids_main_port() {
        let dir = crate::test_support::tmp_dir();
        let mut dep = Dependency::simple("minio");
        dep.extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(9000u64.into()),
        );
        std::fs::write(dir.join("console_port"), "9000").unwrap();
        assert_ne!(launch_console_port(&dep, &dir).unwrap(), 9000);
    }
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
            Some("127.0.0.1:9001")
        );
    }

    #[test]
    fn env_vars_omits_console_address_when_console_port_invalid() {
        let mut extra = HashMap::new();
        extra.insert(
            "console_port".into(),
            crate::config::ExtraValue::Number(70000u64.into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        let vars = MinioModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert!(!vars.contains_key("MINIO_CONSOLE_ADDRESS"));
    }

    #[test]
    fn console_port_validation() {
        let with = |v: crate::config::ExtraValue| {
            let mut extra = HashMap::new();
            extra.insert("console_port".into(), v);
            Dependency::with_extra("minio", extra)
        };
        assert_eq!(console_port(&Dependency::simple("minio")).unwrap(), None);
        assert_eq!(
            console_port(&with(crate::config::ExtraValue::Number(9001u64.into()))).unwrap(),
            Some(9001)
        );
        for bad in [
            crate::config::ExtraValue::Number(0u64.into()),
            crate::config::ExtraValue::Number(65536u64.into()),
            crate::config::ExtraValue::String("9001".into()),
        ] {
            let dep = with(bad);
            assert!(console_port(&dep).is_err());
            assert!(
                MinioModule
                    .nix_launch(&dep, std::path::Path::new("/tmp"))
                    .is_err()
            );
            assert!(MinioModule.docker_spec(&dep).is_err());
            assert!(
                MinioModule
                    .config_issues(&dep)
                    .iter()
                    .any(|w| w.contains("console_port"))
            );
        }
    }

    #[test]
    fn launch_picks_and_keeps_a_console_port_when_unset() {
        let dir = crate::test_support::tmp_dir();
        let dep = Dependency::simple("minio");
        let first = MinioModule.nix_launch(&dep, &dir).unwrap().unwrap();
        let addr = first.args[5].clone();
        assert_eq!(first.args[4], "--console-address");
        let port: u16 = addr
            .strip_prefix("127.0.0.1:")
            .expect("console bound to loopback")
            .parse()
            .unwrap();
        assert_ne!(port, 0);
        let again = MinioModule.nix_launch(&dep, &dir).unwrap().unwrap();
        assert_eq!(again.args[5], addr, "the chosen console port is reused");
    }

    #[test]
    fn auto_picked_console_port_is_exported_and_matches_launch() {
        use crate::commands::ports::PortMode::{Assign, ReadOnly};
        let root = crate::test_support::tmp_dir();
        let dep = Dependency::simple("minio");
        let nix = crate::package_manager::MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        // Read-only resolution before anything is recorded exports nothing and writes nothing.
        assert!(
            MinioModule
                .backend_env_vars(&dep, &nix, &root, ReadOnly)
                .is_empty()
        );
        assert!(!root.join(".devy").exists(), "read-only must not write");
        // `devy up` resolves before the first start: the port is chosen and recorded now...
        let vars = MinioModule.backend_env_vars(&dep, &nix, &root, Assign);
        let addr = vars
            .get("MINIO_CONSOLE_ADDRESS")
            .cloned()
            .expect("exported");
        // ...the launch binds the console to the same one...
        let spec = crate::modules::nix_launch_for(&MinioModule, &nix, &dep, &root)
            .unwrap()
            .unwrap();
        assert_eq!(spec.args[5], addr);
        // ...and read-only commands now report it too.
        for mode in [ReadOnly, Assign] {
            assert_eq!(
                MinioModule.backend_env_vars(&dep, &nix, &root, mode)["MINIO_CONSOLE_ADDRESS"],
                addr
            );
        }

        // A configured port comes from env_vars; docker and other backends add nothing.
        let mut extra = HashMap::new();
        extra.insert(
            "console_port".into(),
            crate::config::ExtraValue::Number(9001u64.into()),
        );
        let configured = Dependency::with_extra("minio", extra);
        let mut docker = Dependency::simple("minio");
        docker.docker = true;
        let brew = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        let other = crate::test_support::tmp_dir();
        for (dep, pm) in [(&configured, &nix), (&docker, &nix), (&dep, &brew)] {
            assert!(
                MinioModule
                    .backend_env_vars(dep, pm, &other, Assign)
                    .is_empty()
            );
        }
        assert!(!other.join(".devy").exists());
    }

    /// A repository that commits `.devy` as a symlink can't make `devy up` create the
    /// data dir (and the console port file) outside the project.
    #[cfg(unix)]
    #[test]
    fn assign_refuses_a_symlinked_data_dir_component() {
        let root = crate::test_support::tmp_dir();
        let outside = crate::test_support::tmp_dir();
        std::os::unix::fs::symlink(&*outside, root.join(".devy")).unwrap();
        let nix = crate::package_manager::MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let vars = MinioModule.backend_env_vars(
            &Dependency::simple("minio"),
            &nix,
            &root,
            crate::commands::ports::PortMode::Assign,
        );
        assert!(vars.is_empty(), "{vars:?}");
        assert_eq!(std::fs::read_dir(&*outside).unwrap().count(), 0);
    }

    #[test]
    fn read_only_ignores_unusable_recorded_console_port() {
        let dir = crate::test_support::tmp_dir();
        assert_eq!(recorded_console_port(&dir, 9000), None);
        for bad in ["garbage", "0", "9000", "70000"] {
            std::fs::write(dir.join("console_port"), bad).unwrap();
            assert_eq!(recorded_console_port(&dir, 9000), None, "{bad}");
            assert_eq!(
                std::fs::read_to_string(dir.join("console_port")).unwrap(),
                bad
            );
        }
        std::fs::write(dir.join("console_port"), "9101\n").unwrap();
        assert_eq!(recorded_console_port(&dir, 9000), Some(9101));
    }

    #[test]
    fn post_setup_warns_about_default_credentials() {
        let pm = crate::package_manager::MockPackageManager::default();
        let msgs = crate::output::with_warn_messages(|| {
            MinioModule
                .post_setup(
                    &Dependency::simple("minio"),
                    &pm,
                    std::path::Path::new("/tmp"),
                )
                .unwrap();
        });
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("minioadmin"), "{msgs:?}");

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
        let warns = crate::output::with_warn_capture(|| {
            MinioModule
                .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
                .unwrap();
        });
        assert_eq!(warns, 0);
    }

    #[test]
    fn env_vars_omits_console_address_when_not_configured() {
        let dep = Dependency::simple("minio");
        let vars = MinioModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert!(!vars.contains_key("MINIO_CONSOLE_ADDRESS"));
    }

    #[test]
    fn config_warnings_default_credentials_when_no_credentials() {
        let dep = Dependency::simple("minio");
        let warnings = MinioModule.config_warnings(&dep);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("default minioadmin credentials"));
    }

    #[test]
    fn config_warnings_no_default_warning_when_both_credentials_set() {
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
        let warnings = MinioModule.config_warnings(&dep);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("plaintext"));
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
    fn config_warnings_no_plaintext_warning_when_only_console_port_set() {
        let mut extra = HashMap::new();
        extra.insert(
            "console_port".into(),
            crate::config::ExtraValue::Number(9001u64.into()),
        );
        let dep = Dependency::with_extra("minio", extra);
        let warnings = MinioModule.config_warnings(&dep);
        assert!(
            !warnings.iter().any(|w| w.contains("plaintext")),
            "console_port alone must not produce the plaintext-credentials warning"
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

    #[test]
    fn docker_warnings_only_for_upstream_minio_image() {
        let with_image = |image: &str| {
            let mut dep = Dependency::simple("minio");
            dep.image = Some(image.into());
            MinioModule.docker_warnings(&dep)
        };
        let warning = |repository: &str| {
            vec![format!(
                "{repository} is no longer published and gets no security fixes; consider pgsty/silo"
            )]
        };
        assert!(
            MinioModule
                .docker_warnings(&Dependency::simple("minio"))
                .is_empty()
        );
        for image in [
            "minio/minio",
            "minio/minio:RELEASE.2025-04-22T22-12-26Z",
            "docker.io/minio/minio:latest",
            "index.docker.io/minio/minio",
            "registry-1.docker.io/minio/minio:latest",
        ] {
            assert_eq!(with_image(image), warning("minio/minio"), "{image}");
        }
        for image in [
            "quay.io/minio/minio",
            "quay.io/minio/minio:RELEASE.2025-04-22T22-12-26Z",
        ] {
            assert_eq!(with_image(image), warning("quay.io/minio/minio"), "{image}");
        }
        for image in [
            "pgsty/minio",
            "pgsty/silo:latest",
            "mirror.example/minio/minio",
            "quay.io/pgsty/minio",
        ] {
            assert!(with_image(image).is_empty(), "{image}");
        }
    }
}
