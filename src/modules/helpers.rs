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

    fn resolved_version(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
    ) -> Result<Option<String>> {
        super::pkg_resolved_version(self, pm, dep, self.name_for(pm))
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

/// Server tuning options devy passes from MySQL/MariaDB `cli_args` (keys with `_`
/// normalized to `-`). Anything else (network, auth, file paths, plugins, startup
/// scripts) is skipped, so an untrusted devy.yml can't widen the server's exposure.
pub(super) const MYSQL_ALLOWED_ARGS: &[&str] = &[
    // buffers and caches
    "innodb-buffer-pool-size",
    "innodb-log-file-size",
    "innodb-redo-log-capacity",
    "innodb-flush-log-at-trx-commit",
    "innodb-file-per-table",
    "key-buffer-size",
    "table-open-cache",
    "thread-cache-size",
    "tmp-table-size",
    "max-heap-table-size",
    "sort-buffer-size",
    "join-buffer-size",
    // limits
    "max-connections",
    "max-allowed-packet",
    "wait-timeout",
    "interactive-timeout",
    // character sets and SQL behavior
    "character-set-server",
    "collation-server",
    "sql-mode",
    "default-time-zone",
    "lower-case-table-names",
    "explicit-defaults-for-timestamp",
    "default-authentication-plugin",
    // logging
    "log-bin-trust-function-creators",
    "slow-query-log",
    "long-query-time",
    "general-log",
    // other
    "transaction-isolation",
    "event-scheduler",
    "performance-schema",
    "skip-name-resolve",
];

/// Name of the devy-owned option file written into Homebrew's `my.cnf.d`.
const BREW_MYSQL_INCLUDE_FILE: &str = "devy.cnf";

/// Writes MySQL-compatible settings (MySQL and MariaDB share the config format).
///
/// Under brew, `config_dir` is the global `$(brew --prefix)/etc` (which mysqld and mariadbd
/// read; the keg's own `etc` is ignored), whose `my.cnf` belongs to the user: devy writes `my.cnf.d/devy.cnf` instead, and warns (without editing) when an
/// existing `my.cnf` doesn't `!includedir` that directory. With no `my.cnf` at all, devy
/// creates one containing only the include. Elsewhere (apt's `conf.d`, which the server
/// already includes) it writes `my.cnf` into `config_dir`.
pub(super) fn write_mysql_config(
    pm_name: &str,
    config_dir: &std::path::Path,
    port: u16,
    cli_args: Option<&str>,
) -> anyhow::Result<()> {
    let mut ini = format!("[mysqld]\nport = {}\n", port);
    let args = sanitized_mysql_args(cli_args);
    let customized = port != 3306 || !args.is_empty();
    for (key, val) in args {
        ini.push_str(&format!("{} = {}\n", key, val));
    }
    // Last, so they win: without Homebrew's stock my.cnf nothing else keeps the server
    // (and MySQL's X Protocol listener) off other interfaces. `loose-` makes MariaDB,
    // which has no X Protocol, ignore the second option instead of refusing to start.
    ini.push_str("bind-address = 127.0.0.1\nloose-mysqlx-bind-address = 127.0.0.1\n");

    if pm_name != "brew" {
        std::fs::create_dir_all(config_dir)
            .with_context(|| format!("Failed to create config dir {}", config_dir.display()))?;
        crate::fs_safe::write_atomic(&config_dir.join("my.cnf"), ini.as_bytes(), 0o644)
            .context("Failed to write my.cnf")?;
        return Ok(());
    }

    let include_dir = config_dir.join("my.cnf.d");
    std::fs::create_dir_all(&include_dir)
        .with_context(|| format!("Failed to create config dir {}", include_dir.display()))?;
    let target = include_dir.join(BREW_MYSQL_INCLUDE_FILE);
    // Unchanged settings were already written (and any warning shown) by an earlier
    // `up`, so don't repeat the `!includedir` warning on every run.
    let changed = std::fs::read_to_string(&target).map_or(true, |old| old != ini);
    if changed {
        crate::fs_safe::write_atomic(&target, ini.as_bytes(), 0o644)
            .with_context(|| format!("Failed to write {}", target.display()))?;
    }

    let main_cnf = config_dir.join("my.cnf");
    match std::fs::read_to_string(&main_cnf) {
        Ok(text) => {
            // At defaults a user's my.cnf (Homebrew's stock MySQL one binds loopback)
            // already describes the server; only devy's own settings need the include.
            if changed && customized && !includes_dir(&text, &include_dir) {
                crate::output::warn(&format!(
                    "{} does not include {} — devy left it unchanged; add `!includedir {}` \
                     to it so devy's port and cli_args settings take effect",
                    main_cnf.display(),
                    include_dir.display(),
                    include_dir.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let include = format!("!includedir {}\n", include_dir.display());
            crate::fs_safe::write_atomic(&main_cnf, include.as_bytes(), 0o644)
                .with_context(|| format!("Failed to write {}", main_cnf.display()))?;
        }
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to read {}", main_cnf.display()));
        }
    }
    Ok(())
}

/// MySQL/MariaDB `post_setup` for backends with a config dir. Under brew devy always
/// writes `my.cnf.d/devy.cnf`, even at defaults: it carries the loopback bind, which
/// nothing else sets once the stock my.cnf is gone (and brew MariaDB's never did).
/// Elsewhere settings are written only when the port or `cli_args` differ from the
/// defaults. Under nix the port and cli_args go on the command line instead; an
/// unapplied explicit port is reported by the shared port resolver.
pub(super) fn mysql_family_post_setup(
    pm: &dyn PackageManager,
    service: &str,
    port: u16,
    cli_args: Option<&str>,
) -> Result<()> {
    let customized = port != 3306 || cli_args.is_some();
    match pm.service_config_dir(service) {
        Some(config_dir) if customized || pm.name() == "brew" => {
            write_mysql_config(pm.name(), &config_dir, port, cli_args)
        }
        Some(_) => Ok(()),
        None if pm.name() != "nix" && cli_args.is_some() => {
            crate::output::warn(&format!(
                "cli_args ignored: {} does not support service config dirs",
                pm.name()
            ));
            Ok(())
        }
        None => Ok(()),
    }
}

/// The port number in `file`, reading at most a few bytes so a huge committed file
/// cannot exhaust memory. `None` if unreadable or not a number.
pub(crate) fn read_port_file(file: &std::path::Path) -> Option<u16> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(file)
        .ok()?
        .take(16)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse::<u16>().ok()
}

/// A port chosen once and kept in `file`, so a service's secondary listener (MinIO
/// console, RabbitMQ distribution) is stable across starts. A recorded port in `avoid`
/// (the service's main port) is replaced. `file` lives in the project's data dir, which
/// a repo can pre-populate, so a non-regular file (symlink, FIFO, directory) at `file`
/// itself is refused rather than followed, and the port is written with
/// `fs_safe::write_atomic`. The data dir itself is created with `fs_safe::ensure_dir_in`
/// (by `nix_launch_for`, or by MinIO's `backend_env_vars` during `devy up`), which refuses
/// symlinked components.
pub(crate) fn persisted_port(file: &std::path::Path, avoid: &[u16], what: &str) -> Result<u16> {
    match std::fs::symlink_metadata(file) {
        Ok(meta) if meta.is_file() => {
            let recorded = read_port_file(file).filter(|p| *p != 0 && !avoid.contains(p));
            if let Some(p) = recorded {
                return Ok(p);
            }
        }
        Ok(_) => anyhow::bail!(
            "{} is not a regular file — remove it so devy can record the {what} port",
            file.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("Failed to inspect {}", file.display())),
    }
    let pick = || {
        find_available_port()
            .with_context(|| format!("Failed to find an available port for the {what}"))
    };
    let mut port = pick()?;
    for _ in 0..8 {
        if !avoid.contains(&port) {
            break;
        }
        port = pick()?;
    }
    if avoid.contains(&port) {
        anyhow::bail!("Failed to find an available port for the {what}");
    }
    // Refuses a symlink planted after the check above, so the write never lands outside
    // the data dir.
    crate::fs_safe::write_atomic(file, port.to_string().as_bytes(), 0o644)
        .with_context(|| format!("Failed to write {}", file.display()))?;
    Ok(port)
}

/// An issue for a credential `key` that can't be passed to a service safely: control
/// characters would break the line-oriented unit files and env files devy writes.
pub(crate) fn control_char_issue(dep: &Dependency, key: &str) -> Option<String> {
    dep.extra
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|v| v.chars().any(char::is_control))
        .map(|_| format!("{key} contains a control character (newline, CR, NUL, …)"))
}

/// Whether option-file `text` has an `!includedir` line naming `dir`.
fn includes_dir(text: &str, dir: &std::path::Path) -> bool {
    let want = dir.to_string_lossy();
    let want = want.trim_end_matches(['/', '\\']);
    text.lines().any(|line| {
        line.trim()
            .strip_prefix("!includedir")
            .is_some_and(|rest| rest.trim().trim_end_matches(['/', '\\']) == want)
    })
}

/// Parses MySQL-compatible `cli_args` into `(key, value)` pairs, keeping only
/// `--key=value` tokens whose key (with `_` read as `-`) is in `MYSQL_ALLOWED_ARGS` and
/// whose value has no newline, carriage return or NUL. Every other token is skipped with
/// a warning. Keys are returned in their normalized `-` form.
pub(super) fn sanitized_mysql_args(cli_args: Option<&str>) -> Vec<(String, String)> {
    let Some(args) = cli_args else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for token in args.split_whitespace() {
        match allowed_mysql_arg(token) {
            Some(pair) => out.push(pair),
            None => crate::output::warn(&format!(
                "skipped cli_args token {}: not an allowed server option",
                token.escape_debug()
            )),
        }
    }
    out
}

/// `token` as a normalized `(key, value)` when it is an allowed `--key=value` option.
fn allowed_mysql_arg(token: &str) -> Option<(String, String)> {
    let (key, val) = token.strip_prefix("--")?.split_once('=')?;
    let key = key.replace('_', "-");
    if !MYSQL_ALLOWED_ARGS.contains(&key.as_str()) || val.contains(['\n', '\r', '\0']) {
        return None;
    }
    Some((key, val.to_string()))
}

/// Allowed MySQL-compatible `cli_args` as `--key=value` server arguments.
pub(super) fn mysql_server_args(cli_args: Option<&str>) -> Vec<String> {
    sanitized_mysql_args(cli_args)
        .into_iter()
        .map(|(k, v)| format!("--{k}={v}"))
        .collect()
}

/// Launch spec shared by MySQL and MariaDB under nix: `--no-defaults` first (it keeps
/// system option files like `/etc/my.cnf` and `~/.my.cnf` from leaking in, and must come
/// first), then the datadir under `data_dir`, then each allowed `cli_args` token, then
/// devy's forced arguments: 127.0.0.1:`port`, the socket (under `data_dir` when the path
/// is short enough) and `extra_args`. Forced arguments come last so the server's
/// last-one-wins option parsing always favors them.
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
    let mut args = vec!["--no-defaults".to_string(), format!("--datadir={data}")];
    args.extend(mysql_server_args(cli_args));
    args.extend([
        format!("--port={port}"),
        "--bind-address=127.0.0.1".to_string(),
        format!("--socket={}", super::path_arg(&socket)),
    ]);
    args.extend(extra_args.iter().map(|a| a.to_string()));
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

/// 64-bit FNV-1a: a stable hash for short, per-project names (unlike `DefaultHasher`,
/// it doesn't change between Rust releases).
pub(crate) fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A name for the project that is unique per checkout: the project `name` lower-cased
/// with everything outside `[a-z0-9-]` replaced by `-`, then `-` and the first 8 hex
/// characters of the FNV-1a hash of the project root path. Two checkouts of the same
/// project therefore never share containers or volumes.
pub(crate) fn project_slug(name: &str, project_root: &std::path::Path) -> String {
    let sanitized: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let hash = fnv1a(&project_root.to_string_lossy());
    format!("{sanitized}-{:08x}", hash >> 32)
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

/// Reads a YAML sequence of strings from `dep.extra[key]` that devy passes to a tool as
/// arguments, failing when an entry breaks the `list_entry` rule (option-like, a URL, a
/// local path or a non-registry npm source) or is not a string. A missing key or a
/// non-sequence yields an empty list.
pub(crate) fn extra_list(dep: &Dependency, key: &str) -> Result<Vec<String>> {
    extra_list_from(&dep.name, &dep.extra, key)
}

fn extra_list_from(
    dep: &str,
    extra: &HashMap<String, crate::config::ExtraValue>,
    key: &str,
) -> Result<Vec<String>> {
    let Some(seq) = extra.get(key).and_then(|v| v.as_sequence()) else {
        return Ok(Vec::new());
    };
    let location = format!("dependencies.{dep}.{key}");
    seq.iter()
        .map(|v| {
            let entry = v.as_str().ok_or_else(|| {
                crate::validate::invalid(&location, "list entry", &format!("{v:?}"))
            })?;
            crate::validate::require(
                crate::validate::list_entry(entry),
                &location,
                "list entry",
                entry,
            )?;
            Ok(entry.to_string())
        })
        .collect()
}

/// Extra keys holding tool argument lists, by canonical module name.
const EXTRA_LISTS: &[(&str, &[&str])] = &[
    ("node", &["global_packages"]),
    ("typescript", &["global_packages"]),
    ("rust", &["targets", "components"]),
    ("gcloud", &["components"]),
];

/// Config-time validation of the module `extra` values that reach tool arguments or
/// file paths: the argument lists in `EXTRA_LISTS`, rust's `toolchain` and python's
/// `venv_path`.
pub(crate) fn validate_extra(
    dep: &str,
    extra: &HashMap<String, crate::config::ExtraValue>,
) -> Result<()> {
    let canonical = super::canonical_name(dep);
    for (module, keys) in EXTRA_LISTS {
        if *module == canonical {
            for key in *keys {
                extra_list_from(dep, extra, key)?;
            }
        }
    }
    if canonical == "python"
        && let Some(path) = extra.get("venv_path").and_then(|v| v.as_str())
    {
        // Empty, `.` or `<dir>/..` would put the venv, and its `bin` on PATH, at the project
        // root, and `<symlink>/..` could leave it; so require a name and allow no `..`.
        let components = || std::path::Path::new(path).components();
        let inside = crate::validate::rel_path_inside(path)
            && components().any(|c| matches!(c, std::path::Component::Normal(_)))
            && !components().any(|c| matches!(c, std::path::Component::ParentDir));
        crate::validate::require(
            inside,
            &format!("dependencies.{dep}.venv_path"),
            "path",
            path,
        )?;
    }
    // A non-string `install_cmd` would be ignored by the python module, which would then
    // run pip instead of what the executable-entry summary shows; refuse it.
    if canonical == "python"
        && let Some(cmd) = extra.get("install_cmd")
        && cmd.as_str().is_none()
    {
        anyhow::bail!("dependencies.{dep}.install_cmd: invalid command (expected a string)");
    }
    if canonical == "rust"
        && let Some(toolchain) = extra.get("toolchain").and_then(|v| v.as_str())
    {
        crate::validate::require(
            crate::validate::toolchain(toolchain),
            &format!("dependencies.{dep}.toolchain"),
            "toolchain",
            toolchain,
        )?;
    }
    Ok(())
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
        allow_insecure: false,
        image: None,
        docker: false,
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

/// Writes the current mtime of `manifest` to `stamp_path`. A symlink at `stamp_path` (a
/// repository can commit one pointing anywhere) is an error; any other failure only
/// warns, since a missing stamp just re-runs the install on the next `devy up`.
pub(super) fn write_stamp(stamp_path: &std::path::Path, manifest: &std::path::Path) -> Result<()> {
    if let Some(secs) = mtime_secs(manifest) {
        write_stamp_text(stamp_path, &secs.to_string())?;
    }
    Ok(())
}

/// Writes `text` to the stamp file at `stamp_path`, with `write_stamp`'s error handling.
pub(super) fn write_stamp_text(stamp_path: &std::path::Path, text: &str) -> Result<()> {
    crate::fs_safe::refuse_symlink(stamp_path)?;
    if let Err(e) = crate::fs_safe::write_atomic(stamp_path, text.as_bytes(), 0o644) {
        crate::output::warn(&format!(
            "Could not write stamp file {}: {e:#} — dependencies will re-run on next `devy up`",
            stamp_path.display()
        ));
    }
    Ok(())
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

// ── Trust-summary setup steps ────────────────────────────────────────────────

/// `[step]` when `<project_root>/<file>` exists, otherwise nothing: the shape of most
/// modules' [`super::Module::setup_steps`].
pub(crate) fn step_if_exists(
    project_root: &std::path::Path,
    file: &str,
    step: &str,
) -> Vec<String> {
    if project_root.join(file).exists() {
        vec![step.to_string()]
    } else {
        Vec::new()
    }
}

/// The Gradle step java and kotlin run when a Gradle build file exists: the repository's
/// `./gradlew` when present, otherwise `gradle` running the build script.
pub(crate) fn gradle_steps(project_root: &std::path::Path) -> Vec<String> {
    let build = ["build.gradle.kts", "build.gradle"]
        .into_iter()
        .find(|f| project_root.join(f).exists());
    match build {
        None => Vec::new(),
        Some(_) if project_root.join("gradlew").exists() => {
            vec!["./gradlew --no-daemon dependencies (repository script)".to_string()]
        }
        Some(file) => vec![format!(
            "gradle --no-daemon dependencies ({file} build script)"
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_port_records_and_reuses_a_port() {
        let dir = crate::test_support::tmp_dir();
        let file = dir.join("console_port");
        let p = persisted_port(&file, &[], "test").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), p.to_string());
        assert_eq!(persisted_port(&file, &[], "test").unwrap(), p);
    }

    #[test]
    fn persisted_port_replaces_invalid_or_main_port() {
        let dir = crate::test_support::tmp_dir();
        let file = dir.join("console_port");
        for recorded in ["garbage", "0", "9000"] {
            std::fs::write(&file, recorded).unwrap();
            let p = persisted_port(&file, &[9000], "test").unwrap();
            assert_ne!(p, 9000);
            assert_eq!(std::fs::read_to_string(&file).unwrap(), p.to_string());
        }
    }

    #[test]
    fn persisted_port_refuses_non_regular_file() {
        let dir = crate::test_support::tmp_dir();
        let file = dir.join("console_port");
        std::fs::create_dir(&file).unwrap();
        let err = persisted_port(&file, &[], "test").unwrap_err().to_string();
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn persisted_port_never_writes_through_a_symlink() {
        let dir = crate::test_support::tmp_dir();
        let victim = dir.join("victim");
        std::fs::write(&victim, "precious").unwrap();
        let file = dir.join("console_port");
        std::os::unix::fs::symlink(&victim, &file).unwrap();
        assert!(persisted_port(&file, &[], "test").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
    }

    /// Scenario "Committed symlink stamp".
    #[cfg(unix)]
    #[test]
    fn write_stamp_refuses_symlinked_stamp() {
        let dir = crate::test_support::tmp_dir();
        let outside = crate::test_support::tmp_dir();
        let victim = outside.join(".zshrc");
        std::fs::write(&victim, "precious").unwrap();
        let manifest = dir.join("package.json");
        std::fs::write(&manifest, "{}").unwrap();
        let stamp = dir.join(".devy_bun_stamp");
        std::os::unix::fs::symlink(&victim, &stamp).unwrap();
        let err = write_stamp(&stamp, &manifest).unwrap_err().to_string();
        assert!(err.contains("it is a symbolic link"), "{err}");
        let err = write_stamp_text(&stamp, "x").unwrap_err().to_string();
        assert!(err.contains("it is a symbolic link"), "{err}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
    }

    #[test]
    fn write_stamp_records_manifest_mtime() {
        let dir = crate::test_support::tmp_dir();
        let manifest = dir.join("package.json");
        std::fs::write(&manifest, "{}").unwrap();
        let stamp = dir.join(".devy_bun_stamp");
        write_stamp(&stamp, &manifest).unwrap();
        assert!(stamp_matches(&stamp, &manifest));
    }

    #[test]
    fn control_char_issue_flags_only_control_characters() {
        let mut dep = Dependency::simple("meilisearch");
        assert!(control_char_issue(&dep, "master_key").is_none());
        for (value, bad) in [("s3cr3t $%\"", false), ("a\rb", true), ("a\0b", true)] {
            dep.extra.insert(
                "master_key".into(),
                crate::config::ExtraValue::String(value.into()),
            );
            let issue = control_char_issue(&dep, "master_key");
            assert_eq!(issue.is_some(), bad, "{value:?}");
            if let Some(issue) = issue {
                assert!(!issue.contains(value), "{issue}");
            }
        }
    }
    use std::path::Path;

    #[test]
    fn fnv1a_known_values() {
        // Reference values for 64-bit FNV-1a.
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a("foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn project_slug_uses_first_8_hex_of_root_hash() {
        let root = "/src/app";
        let expected = format!("app-{}", &format!("{:016x}", fnv1a(root))[..8]);
        assert_eq!(project_slug("app", Path::new(root)), expected);
    }

    #[test]
    fn project_slug_sanitizes_name() {
        let slug = project_slug("My App_v2.0!", Path::new("/x"));
        let (name, hash) = slug.rsplit_once('-').unwrap();
        assert_eq!(name, "my-app-v2-0-");
        assert_eq!(hash.len(), 8);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(
            slug.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
    }

    #[test]
    fn project_slug_differs_per_root() {
        let a = project_slug("app", Path::new("/src/a"));
        let b = project_slug("app", Path::new("/src/b"));
        assert_ne!(a, b);
        assert!(a.starts_with("app-") && b.starts_with("app-"));
    }

    // ── extra_list / validate_extra ───────────────────────────────────────────

    fn strs(items: &[&str]) -> crate::config::ExtraValue {
        crate::config::ExtraValue::Sequence(
            items
                .iter()
                .map(|s| crate::config::ExtraValue::String((*s).into()))
                .collect(),
        )
    }

    fn extra(
        key: &str,
        value: crate::config::ExtraValue,
    ) -> HashMap<String, crate::config::ExtraValue> {
        HashMap::from([(key.to_string(), value)])
    }

    #[test]
    fn extra_list_returns_valid_entries() {
        let dep = Dependency::with_extra(
            "node",
            extra("global_packages", strs(&["@angular/cli", "eslint@8"])),
        );
        assert_eq!(
            extra_list(&dep, "global_packages").unwrap(),
            vec!["@angular/cli", "eslint@8"]
        );
    }

    #[test]
    fn extra_list_missing_or_non_sequence_is_empty() {
        assert!(
            extra_list(&Dependency::simple("node"), "global_packages")
                .unwrap()
                .is_empty()
        );
        let dep = Dependency::with_extra(
            "node",
            extra(
                "global_packages",
                crate::config::ExtraValue::String("ts".into()),
            ),
        );
        assert!(extra_list(&dep, "global_packages").unwrap().is_empty());
    }

    #[test]
    fn extra_list_rejects_hostile_entries() {
        for bad in [
            "./x",
            "-g",
            "--registry=https://evil",
            "https://e/x.tgz",
            "/tmp/shared",
        ] {
            let dep = Dependency::with_extra("node", extra("global_packages", strs(&["ok", bad])));
            let err = extra_list(&dep, "global_packages").unwrap_err().to_string();
            assert_eq!(
                err,
                format!("dependencies.node.global_packages: invalid list entry {bad:?}")
            );
        }
    }

    #[test]
    fn extra_list_rejects_non_string_entries() {
        let dep = Dependency::with_extra(
            "rust",
            extra(
                "targets",
                crate::config::ExtraValue::Sequence(vec![crate::config::ExtraValue::Number(
                    1.into(),
                )]),
            ),
        );
        assert!(extra_list(&dep, "targets").is_err());
    }

    #[test]
    fn validate_extra_checks_every_module_list() {
        for (module, key) in [
            ("node", "global_packages"),
            ("js", "global_packages"),
            ("typescript", "global_packages"),
            ("rust", "targets"),
            ("rust", "components"),
            ("gcloud", "components"),
        ] {
            assert!(
                validate_extra(module, &extra(key, strs(&["./x"]))).is_err(),
                "{module}.{key}"
            );
            validate_extra(module, &extra(key, strs(&["fine"]))).unwrap();
        }
        // Keys of other modules are not tool argument lists.
        validate_extra("redis", &extra("components", strs(&["./x"]))).unwrap();
    }

    #[test]
    fn validate_extra_checks_python_venv_path() {
        let venv = |p: &str| extra("venv_path", crate::config::ExtraValue::String(p.into()));
        validate_extra("python", &venv(".venv")).unwrap();
        validate_extra("python", &venv("envs/dev")).unwrap();
        for bad in [
            "../../",
            "/tmp/shared",
            "a/../../b",
            "",
            ".",
            "./",
            "bin/..",
            "a/../.",
            "a/../b",
        ] {
            let err = validate_extra("python", &venv(bad))
                .unwrap_err()
                .to_string();
            assert_eq!(
                err,
                format!("dependencies.python.venv_path: invalid path {bad:?}")
            );
        }
    }

    #[test]
    fn validate_extra_checks_rust_toolchain() {
        let tc = |t: &str| extra("toolchain", crate::config::ExtraValue::String(t.into()));
        validate_extra("rust", &tc("nightly-2024-01-01")).unwrap();
        let err = validate_extra("rust", &tc("--force-non-host"))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "dependencies.rust.toolchain: invalid toolchain \"--force-non-host\""
        );
    }
}
