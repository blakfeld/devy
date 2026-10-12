use anyhow::{Context, Result};
use std::borrow::Cow;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::Module;

pub struct PostgresModule;

fn package_name(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "winget" => "PostgreSQL.PostgreSQL",
        "nix" => "postgresql",
        _ => "postgresql",
    }
}

fn port(dep: &Dependency) -> anyhow::Result<u16> {
    super::extra_port(dep, "port", 5432)
}

/// The OS user name, which libpq uses when a connection string names no user.
fn local_user() -> String {
    ["USER", "USERNAME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|u| !u.is_empty())
        .unwrap_or_else(|| "postgres".to_string())
}

/// devy's `conf.d/devy.conf` for apt's server, which includes that directory.
fn config_text(port: u16) -> String {
    format!("{}\nport = {port}\n", super::loopback::DEVY_MARKER)
}

impl Module for PostgresModule {
    fn is_service(&self) -> bool {
        true
    }
    fn default_port(&self) -> Option<u16> {
        Some(5432)
    }

    fn explicit_port_via_config(&self, pm: &dyn PackageManager) -> bool {
        pm.name() == "apt"
    }
    fn unapplied_port_hint(&self, pm: &dyn PackageManager) -> Option<&'static str> {
        (pm.name() == "brew").then_some(
            "set `port` in postgresql.conf in the server's data directory (e.g. `$(brew --prefix)/var/postgresql@<version>`)",
        )
    }

    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["port"])
    }

    fn backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency> {
        Some(super::pkg_dep(self, pm, dep, package_name(pm)))
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        super::pkg_installed(self, pm, dep, package_name(pm))
    }

    fn resolved_version(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
    ) -> Result<Option<String>> {
        super::pkg_resolved_version(self, pm, dep, package_name(pm))
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        super::install_backend(self, pm, dep)
    }

    fn nix_versioned_attr(&self, version: &str) -> Option<String> {
        // `16` or `16.4` → `postgresql_16`.
        super::helpers::allowlisted_attr(version, 1, &["14", "15", "16", "17", "18"], |v| {
            format!("postgresql_{v}")
        })
    }

    fn post_setup(
        &self,
        dep: &Dependency,
        pm: &dyn PackageManager,
        _project_root: &std::path::Path,
    ) -> Result<()> {
        let p = port(dep)?;
        // Without a config dir (nix passes the port on the command line), there is
        // nothing to write. An unapplied explicit port is reported by the shared port
        // resolver, except under apt, which takes it from devy's config file: there no
        // `/etc/postgresql/<version>` means the port can't be applied, so say so here.
        let Some(config_dir) = pm.service_config_dir("postgresql") else {
            if pm.name() == "apt" && p != 5432 {
                crate::output::warn(&format!(
                    "postgresql: port {p} is not applied — devy found no /etc/postgresql/<version> directory; create /etc/postgresql/<version>/main/conf.d/devy.conf with the line `port = {p}`"
                ));
            }
            return Ok(());
        };
        let file = config_dir.join("devy.conf");
        // brew's dir is the keg's `etc`, which the formula's server (reading only its data
        // directory's postgresql.conf) never reads: remove what earlier versions wrote.
        // Nothing to restart, since the server never read it.
        if pm.name() == "brew" {
            super::helpers::remove_owned_config(
                &file,
                "postgresql",
                "an earlier devy wrote it there, but the server never read it",
            );
            return Ok(());
        }
        if p == 5432 {
            super::helpers::remove_owned_config(&file, "postgresql", super::helpers::RESTART_NOTE);
            return Ok(());
        }
        let conf = config_text(p);
        let consequence = format!("port {p} is not applied");
        if let super::helpers::OwnedWrite::Failed(msg) = super::helpers::write_owned_config(
            &file,
            &conf,
            &super::helpers::exact_config_fix(&conf),
            "postgresql",
            &consequence,
        ) {
            crate::output::warn(&msg);
        }
        Ok(())
    }

    fn env_vars(
        &self,
        dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> std::collections::HashMap<String, String> {
        let p = port(dep).unwrap_or(5432);
        let mut map = std::collections::HashMap::new();
        map.insert(
            "DATABASE_URL".into(),
            format!("postgres://localhost:{p}/postgres"),
        );
        map
    }

    fn service_name<'a>(&self, _dep: &'a Dependency) -> Cow<'a, str> {
        Cow::Borrowed("postgresql")
    }

    fn nix_launch(
        &self,
        dep: &Dependency,
        data_dir: &std::path::Path,
    ) -> Result<Option<super::LaunchSpec>> {
        let p = port(dep)?;
        let data = super::path_arg(data_dir);
        let sockets = super::path_arg(&super::socket_dir(data_dir, &format!(".s.PGSQL.{p}"))?);
        Ok(Some(super::LaunchSpec {
            init: Some(super::InitStep {
                marker: data_dir.join("PG_VERSION"),
                cmd: vec!["initdb".into(), "-D".into(), data.clone()],
            }),
            ..super::LaunchSpec::new(
                "postgres",
                [
                    "-D".into(),
                    data.clone(),
                    "-p".into(),
                    p.to_string(),
                    "-k".into(),
                    sockets,
                    "-c".into(),
                    "listen_addresses=127.0.0.1".into(),
                ],
            )
        }))
    }

    fn nix_attr(&self, dep: &crate::config::Dependency) -> Option<String> {
        Some(super::nix_install_attr(self, dep, "postgresql"))
    }

    fn docker_spec(&self, _dep: &Dependency) -> Result<Option<super::DockerSpec>> {
        // DATABASE_URL names no user, so clients connect as the OS user, as under nix
        // where initdb makes the OS user the superuser.
        Ok(Some(
            super::DockerSpec::new("postgres", "16", 5432)
                .data("/var/lib/postgresql/data")
                .env(&[
                    ("POSTGRES_HOST_AUTH_METHOD", "trust"),
                    ("POSTGRES_USER", &local_user()),
                ]),
        ))
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
            .with_context(|| format!("PostgreSQL not accepting connections on port {p}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        // Send a minimal StartupMessage to trigger an auth response ('R') or error ('E').
        // Message format: length (4 bytes BE) + protocol version 3.0 (4 bytes BE).
        let msg: &[u8] = &[0x00, 0x00, 0x00, 0x08, 0x00, 0x03, 0x00, 0x00];
        stream.write_all(msg)?;
        let mut first = [0u8; 1];
        stream.read_exact(&mut first)?;
        anyhow::ensure!(
            first[0] == b'R' || first[0] == b'E',
            "PostgreSQL on port {p} returned unexpected startup response byte: 0x{:02x}",
            first[0]
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn dep_with_port(port: u64) -> Dependency {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(port.into()),
        );
        Dependency::with_extra("postgresql", extra)
    }

    #[test]
    fn env_vars_default_port() {
        let dep = Dependency::simple("postgresql");
        let vars = PostgresModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("DATABASE_URL").map(|s| s.as_str()),
            Some("postgres://localhost:5432/postgres")
        );
    }

    #[test]
    fn env_vars_custom_port() {
        let dep = dep_with_port(5433);
        let vars = PostgresModule.env_vars(&dep, std::path::Path::new("/tmp"));
        assert_eq!(
            vars.get("DATABASE_URL").map(|s| s.as_str()),
            Some("postgres://localhost:5433/postgres")
        );
    }

    #[test]
    fn postgres_module_is_service() {
        assert!(PostgresModule.is_service());
    }

    #[test]
    fn service_name_is_always_postgresql() {
        let dep = Dependency::simple("postgresql");
        assert_eq!(PostgresModule.service_name(&dep).as_ref(), "postgresql");
        let dep2 = Dependency::simple("postgres");
        assert_eq!(PostgresModule.service_name(&dep2).as_ref(), "postgresql");
    }

    #[test]
    fn port_defaults_to_5432() {
        let dep = Dependency::simple("postgresql");
        assert_eq!(port(&dep).unwrap(), 5432);
    }

    #[test]
    fn port_reads_custom_value() {
        let dep = dep_with_port(5433);
        assert_eq!(port(&dep).unwrap(), 5433);
    }

    #[test]
    fn port_bails_on_out_of_range() {
        let dep = dep_with_port(99999);
        assert!(port(&dep).is_err());
    }

    #[test]
    fn postgres_health_check_fails_on_unused_port() {
        let dep = dep_with_port(19984);
        let err = PostgresModule.health_check(&dep).unwrap_err();
        assert!(err.to_string().contains("19984"));
    }

    #[test]
    fn config_text_is_devy_managed() {
        assert_eq!(config_text(5433), "# devy-managed\nport = 5433\n");
    }

    #[test]
    fn package_name_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "postgresql");
    }

    #[test]
    fn package_name_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "PostgreSQL.PostgreSQL");
    }

    #[test]
    fn package_name_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(package_name(&pm), "postgresql");
    }

    #[test]
    fn is_installed_true() {
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(
            PostgresModule
                .is_installed(&pm, &Dependency::simple("postgresql"))
                .unwrap()
        );
    }

    #[test]
    fn is_installed_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !PostgresModule
                .is_installed(&pm, &Dependency::simple("postgresql"))
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
            PostgresModule
                .install(&pm, &Dependency::simple("postgresql"))
                .is_err()
        );
    }

    #[test]
    fn post_setup_writes_config_for_non_default_port() {
        let dir = crate::test_support::tmp_dir();
        let pm = crate::package_manager::MockPackageManager {
            config_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        let dep = dep_with_port(5433);
        PostgresModule
            .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
            .unwrap();
        let content = std::fs::read_to_string(dir.join("devy.conf")).unwrap();
        assert!(content.contains("port = 5433"));
    }

    #[test]
    fn post_setup_skips_config_for_default_port() {
        let dir = crate::test_support::tmp_dir();
        let pm = crate::package_manager::MockPackageManager {
            config_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        let dep = Dependency::simple("postgresql");
        PostgresModule
            .post_setup(&dep, &pm, std::path::Path::new("/tmp"))
            .unwrap();
        assert!(!dir.join("devy.conf").exists());
    }

    /// Scenario "apt config directory not writable".
    #[cfg(unix)]
    #[test]
    fn apt_postgres_unwritable_conf_dir_warns_instead_of_failing() {
        let Some(dir) = crate::test_support::read_only_dir(|_| {}) else {
            return; // root can write anywhere
        };
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            config_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        let mut result = None;
        let msgs = crate::output::with_warn_messages(|| {
            result = Some(PostgresModule.post_setup(&dep_with_port(5433), &pm, Path::new("/tmp")));
        });
        result.unwrap().unwrap();
        let file = dir.join("devy.conf").display().to_string();
        assert!(
            msgs.iter().any(|m| m.contains(&file)
                && m.contains("port = 5433")
                && m.contains("exactly these lines")
                && m.contains("`# devy-managed`")),
            "{msgs:?}"
        );
    }

    /// An explicit apt port with no `/etc/postgresql/<version>` can't be applied (the
    /// shared resolver doesn't warn under apt), so post_setup says so; the default port
    /// and other backends stay quiet.
    #[test]
    fn apt_postgres_without_version_dir_warns_that_the_port_is_not_applied() {
        let no_dir = |name| crate::package_manager::MockPackageManager {
            name,
            config_dir: None,
            ..Default::default()
        };
        let mut result = None;
        let msgs = crate::output::with_warn_messages(|| {
            result = Some(PostgresModule.post_setup(
                &dep_with_port(5433),
                &no_dir("apt"),
                Path::new("/tmp"),
            ));
        });
        result.unwrap().unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("port 5433 is not applied")
                && m.contains("/etc/postgresql/<version>/main/conf.d/devy.conf")
                && m.contains("port = 5433")),
            "{msgs:?}"
        );
        for (backend, dep) in [
            ("apt", Dependency::simple("postgresql")),
            ("nix", dep_with_port(5433)),
        ] {
            let msgs = crate::output::with_warn_messages(|| {
                PostgresModule
                    .post_setup(&dep, &no_dir(backend), Path::new("/tmp"))
                    .unwrap();
            });
            assert!(msgs.is_empty(), "{backend}: {msgs:?}");
        }
    }

    /// Scenario "Postgres on brew".
    #[test]
    fn brew_postgres_writes_no_keg_config() {
        let keg_etc = crate::test_support::tmp_dir();
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            config_dir: Some(keg_etc.to_path_buf()),
            ..Default::default()
        };
        let dep = dep_with_port(5433);
        PostgresModule
            .post_setup(&dep, &pm, Path::new("/tmp"))
            .unwrap();
        assert!(!keg_etc.join("devy.conf").exists());
        let msg = crate::commands::ports::unapplied_port_warning(&dep, &pm).expect("must warn");
        assert!(
            msg.contains("cannot make postgresql listen on port 5433 with brew")
                && msg.contains("postgresql.conf in the server's data directory (e.g. `"),
            "{msg}"
        );
    }

    #[test]
    fn brew_postgres_removes_stale_devy_managed_keg_config() {
        let keg_etc = crate::test_support::tmp_dir();
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            config_dir: Some(keg_etc.to_path_buf()),
            ..Default::default()
        };
        let stale = keg_etc.join("devy.conf");
        for dep in [dep_with_port(5433), Dependency::simple("postgresql")] {
            std::fs::write(&stale, config_text(51000)).unwrap();
            PostgresModule
                .post_setup(&dep, &pm, Path::new("/tmp"))
                .unwrap();
            assert!(!stale.exists());
        }
        // A file devy didn't write is left alone.
        std::fs::write(&stale, "port = 5434\n").unwrap();
        PostgresModule
            .post_setup(&Dependency::simple("postgresql"), &pm, Path::new("/tmp"))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&stale).unwrap(), "port = 5434\n");
    }

    #[cfg(unix)]
    #[test]
    fn apt_postgres_removal_failure_warns_instead_of_failing() {
        let Some(dir) = crate::test_support::read_only_dir(|d| {
            std::fs::write(d.join("devy.conf"), config_text(5433)).unwrap();
        }) else {
            return; // root can write anywhere
        };
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            config_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        let mut result = None;
        let msgs = crate::output::with_warn_messages(|| {
            result = Some(PostgresModule.post_setup(
                &Dependency::simple("postgresql"),
                &pm,
                Path::new("/tmp"),
            ));
        });
        result.unwrap().unwrap();
        assert!(dir.join("devy.conf").exists());
        let file = dir.join("devy.conf").display().to_string();
        assert!(
            msgs.iter()
                .any(|m| m.contains("could not remove") && m.contains(&file)),
            "{msgs:?}"
        );
    }

    #[test]
    fn is_running_true() {
        let pm = crate::package_manager::MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        assert!(
            PostgresModule
                .is_running(&pm, &Dependency::simple("postgresql"))
                .unwrap()
        );
    }

    #[test]
    fn is_running_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            !PostgresModule
                .is_running(&pm, &Dependency::simple("postgresql"))
                .unwrap()
        );
    }

    #[test]
    fn start_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            PostgresModule
                .start(
                    &pm,
                    &Dependency::simple("postgresql"),
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
            PostgresModule
                .start(
                    &pm,
                    &Dependency::simple("postgresql"),
                    std::path::Path::new("/tmp")
                )
                .is_err()
        );
    }

    #[test]
    fn stop_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            PostgresModule
                .stop(&pm, &Dependency::simple("postgresql"))
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
            PostgresModule
                .stop(&pm, &Dependency::simple("postgresql"))
                .is_err()
        );
    }
}
