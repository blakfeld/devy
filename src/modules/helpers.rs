use anyhow::{Context, Result};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::Duration;

use crate::config::Dependency;
use crate::package_manager::PackageManager;

use super::Module;

/// A simple package install module with per-PM package names.
/// Use for languages and tools that have no special install logic beyond `install_package`.
pub(super) struct PackageModule {
    pub(super) default: &'static str,
    pub(super) apt: &'static str,
    pub(super) winget: &'static str,
    pub(super) nix: &'static str,
    /// Maps a version to a versioned nixpkgs attribute (see `Module::nix_versioned_attr`).
    pub(super) nix_versioned: fn(&str) -> Option<String>,
    /// Whether the nixpkgs package is unfree (see `Module::nix_unfree`).
    pub(super) nix_unfree: bool,
}

/// `PackageModule::nix_versioned` for packages without versioned nixpkgs attributes.
pub(super) fn no_nix_versions(_version: &str) -> Option<String> {
    None
}

impl PackageModule {
    pub(super) fn name_for(&self, pm: &dyn PackageManager) -> &'static str {
        match pm.name() {
            "apt" => self.apt,
            "winget" => self.winget,
            "nix" => self.nix,
            _ => self.default,
        }
    }
}

impl Module for PackageModule {
    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool> {
        super::pkg_installed(self, pm, dep, self.name_for(pm))
    }

    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        pm.install_package(&super::pkg_dep(self, pm, dep, self.name_for(pm)))
    }

    fn nix_versioned_attr(&self, version: &str) -> Option<String> {
        (self.nix_versioned)(version)
    }

    fn nix_attr(&self, dep: &Dependency) -> Option<String> {
        Some(super::nix_install_attr(self, dep, self.nix))
    }

    fn nix_unfree(&self) -> bool {
        self.nix_unfree
    }
}

/// Writes a `my.cnf` snippet for MySQL-compatible services (MySQL and MariaDB share the same
/// config format). Creates the directory if absent.
pub(super) fn write_mysql_config(
    config_dir: &std::path::Path,
    port: u16,
    cli_args: Option<&str>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("Failed to create config dir {}", config_dir.display()))?;

    let mut ini = format!("[mysqld]\nport = {}\n", port);
    for (key, val) in sanitized_mysql_args(cli_args) {
        ini.push_str(&format!("{} = {}\n", key, val));
    }

    std::fs::write(config_dir.join("my.cnf"), ini).context("Failed to write my.cnf")?;
    Ok(())
}

/// Parses MySQL-compatible `cli_args` into `(key, value)` pairs, warning about and
/// skipping any token that isn't a safe `--key=value`.
pub(super) fn sanitized_mysql_args(cli_args: Option<&str>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(args) = cli_args {
        for arg in args.split_whitespace() {
            // Require the -- prefix so bare key=value tokens can't inject directives.
            let Some(rest) = arg.strip_prefix("--") else {
                crate::output::warn(&format!(
                    "mysql cli_args: skipping {:?} — must start with --",
                    arg
                ));
                continue;
            };
            let Some((key, val)) = rest.split_once('=') else {
                crate::output::warn(&format!(
                    "mysql cli_args: skipping {:?} — no = found (expected --key=value)",
                    arg
                ));
                continue;
            };
            // Keys must be safe ini identifiers: alphanumeric, hyphens, underscores.
            if !key
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                crate::output::warn(&format!(
                    "mysql cli_args: skipping {:?} — key contains unsafe characters",
                    arg
                ));
                continue;
            }
            // Values must not contain newlines, carriage returns, or null bytes (would break ini structure).
            if val.contains('\n') || val.contains('\r') || val.contains('\0') {
                crate::output::warn(&format!(
                    "mysql cli_args: skipping {:?} — value contains unsafe characters",
                    arg
                ));
                continue;
            }
            out.push((key.to_string(), val.to_string()));
        }
    }
    out
}

/// Launch spec shared by MySQL and MariaDB under nix: listens on 127.0.0.1:`port` with
/// its datadir (and, when the path is short enough, socket) under `data_dir`, then
/// `extra_args`, then each sanitized `cli_args` token. `--no-defaults` keeps system
/// option files (`/etc/my.cnf`, `~/.my.cnf`) from leaking in; it must come first.
pub(super) fn mysql_family_launch(
    server: &str,
    init_cmd: Vec<String>,
    extra_args: &[&str],
    port: u16,
    cli_args: Option<&str>,
    data_dir: &std::path::Path,
) -> Result<super::LaunchSpec> {
    let data = super::path_arg(data_dir);
    let socket = super::socket_dir(data_dir, "mysql.sock")?.join("mysql.sock");
    let mut args = vec![
        "--no-defaults".to_string(),
        format!("--datadir={data}"),
        format!("--port={port}"),
        "--bind-address=127.0.0.1".to_string(),
        format!("--socket={}", super::path_arg(&socket)),
    ];
    args.extend(extra_args.iter().map(|a| a.to_string()));
    args.extend(
        sanitized_mysql_args(cli_args)
            .into_iter()
            .map(|(k, v)| format!("--{k}={v}")),
    );
    Ok(super::LaunchSpec {
        init: Some(super::InitStep {
            // The `mysql` system schema directory exists once the datadir is initialized.
            marker: data_dir.join("mysql"),
            cmd: init_cmd,
        }),
        ..super::LaunchSpec::new(server, args)
    })
}

/// Runs a command, inheriting stdio, and bails on non-zero exit.
pub(super) fn run_cmd(prog: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(prog)
        .args(args)
        .status()
        .with_context(|| format!("Failed to start `{prog}`"))?;
    if !status.success() {
        anyhow::bail!(
            "`{prog} {}` failed — check the output above for details",
            args.join(" ")
        );
    }
    Ok(())
}

/// Binds an ephemeral socket to let the OS pick a free port and returns that port number.
pub(crate) fn find_available_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("Failed to bind socket to discover an available port")?;
    Ok(listener.local_addr()?.port())
}

/// Reads a port value from `dep.extra[key]`, returning an error on overflow or zero.
pub(crate) fn extra_port(dep: &Dependency, key: &str, default: u16) -> Result<u16> {
    let raw = dep
        .extra
        .get(key)
        .and_then(|v| v.as_u64())
        .unwrap_or(default as u64);
    if raw == 0 {
        anyhow::bail!("{} value 0 is out of range (must be 1–65535)", key);
    }
    u16::try_from(raw)
        .with_context(|| format!("{} value {} is out of range (must be 1–65535)", key, raw))
}

/// Reads a YAML sequence of strings from dep.extra.
pub(super) fn extra_strs(dep: &Dependency, key: &str) -> Vec<String> {
    dep.extra
        .get(key)
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Maps `version` to `attr(key)`, where `key` is its first `parts` numeric components
/// (e.g. `"3.12.4"` with 2 parts → `"3.12"`), when `key` is in `allowed`.
pub(super) fn allowlisted_attr(
    version: &str,
    parts: usize,
    allowed: &[&str],
    attr: impl Fn(&str) -> String,
) -> Option<String> {
    let comps: Vec<&str> = version.trim().split('.').take(parts).collect();
    if comps.len() < parts
        || comps
            .iter()
            .any(|c| c.is_empty() || !c.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let key = comps.join(".");
    allowed.contains(&key.as_str()).then(|| attr(&key))
}

/// Returns a copy of `dep` with the name replaced by the platform-appropriate package name.
pub(super) fn pm_dep(dep: &Dependency, name: &str) -> Dependency {
    Dependency {
        name: name.to_string(),
        version: dep.version.clone(),
        tap: dep.tap.clone(),
        after_install: None,
        shell: None,
        extra: HashMap::new(),
        version_from_lock: dep.version_from_lock,
        allow_unfree: false,
    }
}

/// Returns the mtime of `path` as seconds since the Unix epoch, or `None` on failure.
pub(super) fn mtime_secs(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// Returns `true` if the stamp at `stamp_path` records the same mtime as `manifest`.
pub(super) fn stamp_matches(stamp_path: &std::path::Path, manifest: &std::path::Path) -> bool {
    let Some(current) = mtime_secs(manifest) else {
        return false;
    };
    std::fs::read_to_string(stamp_path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .is_some_and(|stamped| stamped == current)
}

/// Writes the current mtime of `manifest` to `stamp_path`.
pub(super) fn write_stamp(stamp_path: &std::path::Path, manifest: &std::path::Path) {
    if let Some(secs) = mtime_secs(manifest)
        && let Err(e) = std::fs::write(stamp_path, secs.to_string())
    {
        crate::output::warn(&format!(
            "Could not write stamp file {}: {e} — dependencies will re-run on next `devy up`",
            stamp_path.display()
        ));
    }
}

/// Checks that a service is accepting TCP connections on localhost.
pub(super) fn tcp_ping(port: u16, service: &str) -> Result<()> {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .with_context(|| format!("{service} not accepting connections on port {port}"))?;
    Ok(())
}

/// Returns the platform-appropriate package name for Node.js.
/// Shared between NodeModule and TypeScriptModule.
pub(super) fn node_pkg(pm: &dyn PackageManager) -> &'static str {
    match pm.name() {
        "apt" => "nodejs",
        "winget" => "OpenJS.NodeJS",
        "nix" => "nodejs",
        _ => "node",
    }
}
