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
    fn backend_package(&self, pm: &dyn PackageManager, dep: &Dependency) -> Option<Dependency> {
        Some(super::pkg_dep(self, pm, dep, self.name_for(pm)))
    }

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
        super::install_backend(self, pm, dep)
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

/// What a rewrite of a config file keeps from the file it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeptAttrs {
    /// Permission bits for the new file.
    mode: u32,
    /// The replaced file's (uid, gid), given to the new file (Unix only).
    owner: Option<(u32, u32)>,
}

/// What rewriting the regular file at `path` keeps: its own mode with
/// setuid/setgid/sticky and group/world write dropped (`& 0o755`), so a rewrite never
/// widens a `0600` file that holds secrets, and its owner and group, so root rewriting a
/// `root:mysql 0640` file doesn't leave it `root:root`. With no such file (or on
/// Windows): `0o644` and no owner, so a new file is created as the caller.
fn existing_attrs(path: &std::path::Path) -> KeptAttrs {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if let Ok(meta) = std::fs::symlink_metadata(path)
            && meta.is_file()
        {
            return KeptAttrs {
                mode: meta.permissions().mode() & 0o755,
                owner: Some((meta.uid(), meta.gid())),
            };
        }
    }
    let _ = path;
    KeptAttrs {
        mode: 0o644,
        owner: None,
    }
}

/// What [`write_owned_config`] did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OwnedWrite {
    Changed,
    Unchanged,
    /// The write failed; holds the warning to show.
    Failed(String),
}

/// The note shown when devy writes or removes a service config: brew and apt servers
/// read their config only at start.
pub(super) const RESTART_NOTE: &str = "restart the service if it is already running";

/// The fix named when devy can't write its database config `content`: only a file
/// holding exactly these lines stops the warning. The `# devy-managed` first line is
/// explained, since it lets a later `up` at the default port delete the file.
pub(super) fn exact_config_fix(content: &str) -> String {
    format!(
        "create or replace it with exactly these lines. The first line, `{}`, lets devy delete the file when the port returns to the default; drop that line if you put other settings in the file (devy then keeps warning, since the file is no longer exactly its own):\n{content}",
        super::loopback::DEVY_MARKER
    )
}

/// Writes `content` to `path` when it differs, creating the parent directory. A rewrite
/// keeps an existing file's mode minus setuid/setgid/sticky and group/world write
/// (`& 0o755`) and, on Unix, its owner and group; a new file gets `0o644` and the
/// caller's ids. When the owner or group can't be kept (a non-root user can't hand a
/// file to someone else), nothing is replaced and the write fails. On a change it tells
/// the user to restart the service. A failure is returned as a warning naming `path`,
/// what it means (`consequence`) and `fix` — what to do, naming only devy's lines — never
/// the existing file, which may be the user's own config with credentials in it.
pub(super) fn write_owned_config(
    path: &std::path::Path,
    content: &str,
    fix: &str,
    what: &str,
    consequence: &str,
) -> OwnedWrite {
    write_owned_config_keeping(path, content, fix, what, consequence, existing_attrs)
}

/// [`write_owned_config`] with the lookup of what a rewrite keeps injected, so tests
/// can stand in a replaced file owned by someone else.
fn write_owned_config_keeping(
    path: &std::path::Path,
    content: &str,
    fix: &str,
    what: &str,
    consequence: &str,
    kept: impl FnOnce(&std::path::Path) -> KeptAttrs,
) -> OwnedWrite {
    // Capped and non-blocking. A symlink (a dotfile manager's, say) is compared through,
    // but only when it resolves to a regular file: identical content is left alone, and
    // anything else counts as changed, so `write_atomic` replaces it or refuses the link.
    let cap = content.len() as u64 + 1;
    if crate::fs_safe::read_regular_capped_following_links(path, cap)
        .is_ok_and(|old| old == content.as_bytes())
    {
        return OwnedWrite::Unchanged;
    }
    let KeptAttrs { mode, owner } = kept(path);
    let result = path
        .parent()
        .map_or(Ok(()), |dir| {
            std::fs::create_dir_all(dir).map_err(anyhow::Error::from)
        })
        .and_then(|()| {
            crate::fs_safe::write_atomic_with_owner(path, content.as_bytes(), mode, owner)
        });
    match result {
        Ok(()) => {
            crate::output::info(&format!(
                "{what}: wrote {} — {RESTART_NOTE}",
                path.display()
            ));
            OwnedWrite::Changed
        }
        Err(e) => match e.downcast_ref::<crate::fs_safe::OwnerNotKept>() {
            // Typically a non-root rewrite of a file another user owns (one an earlier
            // `sudo devy up` left root-owned, say): ownership never changes silently.
            Some(not_kept) => OwnedWrite::Failed(format!(
                "{what}: could not write {path}: devy can't give the new file its owner and group ({uid}:{gid}) ({err}), so it left {path} unchanged and {consequence} — `sudo chown` it to yourself or delete it so devy can rewrite it, or {fix}",
                path = path.display(),
                uid = not_kept.uid,
                gid = not_kept.gid,
                err = not_kept.source,
            )),
            None => OwnedWrite::Failed(format!(
                "{what}: could not write {} ({e:#}), so {consequence} — {fix}",
                path.display()
            )),
        },
    }
}

/// Removes the regular file at `path` when devy wrote it (its first line is
/// `# devy-managed`), telling the user `note` (usually [`RESTART_NOTE`]); any other file
/// is left alone. Returns whether the file was removed. A failed removal is a warning.
pub(super) fn remove_owned_config(path: &std::path::Path, what: &str, note: &str) -> bool {
    // Only a small regular file (not a symlink or FIFO) whose first line is exactly the
    // marker; devy's database configs are a few lines.
    let owned = crate::fs_safe::read_regular_capped(path, 4096).is_ok_and(|bytes| {
        String::from_utf8_lossy(&bytes)
            .lines()
            .next()
            .map(|l| l.trim_end_matches('\r'))
            == Some(super::loopback::DEVY_MARKER)
    });
    if !owned {
        return false;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {
            crate::output::info(&format!("{what}: removed {} — {note}", path.display()));
            true
        }
        Err(e) => {
            crate::output::warn(&format!(
                "{what}: could not remove {} ({e}) — delete it, or the service keeps devy's old settings",
                path.display()
            ));
            false
        }
    }
}

/// Name of the devy-owned option file written into Homebrew's `my.cnf.d`.
const BREW_MYSQL_INCLUDE_FILE: &str = "devy.cnf";

/// Name of the devy-managed option file written into apt's `/etc/mysql/conf.d`. A file
/// of devy's own, so devy never replaces (or removes) a `my.cnf` someone else put there.
const CONF_D_MYSQL_FILE: &str = "devy.cnf";

/// The warning while a `my.cnf` exists in apt's `conf.d` (`config_dir`), whatever devy's
/// settings. Both MySQL and MariaDB read `!includedir` files in `strcmp` order and let
/// the last value win, so `my.cnf` is read after devy's `devy.cnf` and any `port` in it
/// overrides devy's, or the default 3306 when devy has nothing to write. Older devy
/// versions wrote that file with a random port (without the `# devy-managed` marker), so
/// devy can't tell it from the user's and only warns. Only its existence is checked: the
/// file is never read or shown, since it may hold credentials.
fn legacy_conf_d_my_cnf_warning(config_dir: &std::path::Path) -> Option<String> {
    let legacy = config_dir.join("my.cnf");
    std::fs::symlink_metadata(&legacy).ok()?;
    Some(format!(
        "{} is read after {}, so settings in it (such as `port`) override devy's settings and the default port 3306 — if an older devy wrote it, delete it (`sudo rm {}`) and restart the service",
        legacy.display(),
        config_dir.join(CONF_D_MYSQL_FILE).display(),
        legacy.display()
    ))
}

/// Writes MySQL-compatible settings (MySQL and MariaDB share the config format) for
/// `service` (named in messages).
///
/// Under brew, `config_dir` is the global `$(brew --prefix)/etc` (which mysqld and mariadbd
/// read; the keg's own `etc` is ignored), whose `my.cnf` belongs to the user: devy writes
/// `my.cnf.d/devy.cnf` instead, and on every run warns (without editing) while an existing
/// `my.cnf` doesn't `!includedir` that directory and devy's settings differ from the
/// defaults. With no `my.cnf` at all, devy creates one containing only the include.
/// Elsewhere (apt's `conf.d`, which the server already includes) it writes a devy-managed
/// `devy.cnf` into `config_dir`, never touching a `my.cnf` there (which an older devy, or
/// the user, wrote; [`mysql_family_post_setup`] warns that it overrides `devy.cnf`). A
/// write or read devy can't make (`/etc` without root, an
/// unwritable brew prefix, an unreadable `my.cnf`) is a warning naming the lines to put
/// in place, never an error.
pub(super) fn write_mysql_config(
    pm_name: &str,
    service: &str,
    config_dir: &std::path::Path,
    port: u16,
    cli_args: Option<&str>,
) {
    // Sanitized once: skipped tokens are warned about here.
    let args = sanitized_mysql_args(cli_args);
    let customized = port != 3306 || !args.is_empty();
    let ini = mysql_ini(port, &args);

    if pm_name != "brew" {
        // The marker lets a later `up` at the defaults remove the file.
        let ini = format!("{}\n{ini}", super::loopback::DEVY_MARKER);
        let target = config_dir.join(CONF_D_MYSQL_FILE);
        let consequence = format!("port {port} and devy's cli_args settings are not applied");
        if let OwnedWrite::Failed(msg) = write_owned_config(
            &target,
            &ini,
            &exact_config_fix(&ini),
            service,
            &consequence,
        ) {
            crate::output::warn(&msg);
        }
        return;
    }

    let include_dir = config_dir.join("my.cnf.d");
    let target = include_dir.join(BREW_MYSQL_INCLUDE_FILE);
    let consequence =
        format!("port {port} and devy's cli_args and loopback settings are not applied");
    let fix = format!("create or replace it with exactly these lines:\n{ini}");
    if let OwnedWrite::Failed(msg) = write_owned_config(&target, &ini, &fix, service, &consequence)
    {
        crate::output::warn(&msg);
    }

    let main_cnf = config_dir.join("my.cnf");
    let include_line = format!("!includedir {}", include_dir.display());
    match crate::fs_safe::read_regular_capped_following_links(&main_cnf, MY_CNF_CAP) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            if let Some(warning) = missing_include_warning(config_dir, &text, port, customized) {
                crate::output::warn(&format!("{service}: {warning}"));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let include = format!("{include_line}\n");
            if let Err(e) = crate::fs_safe::write_atomic(&main_cnf, include.as_bytes(), 0o644) {
                crate::output::warn(&format!(
                    "{service}: could not create {} ({e:#}), so devy's settings in {} are not applied — create it with the line `{include_line}`",
                    main_cnf.display(),
                    target.display()
                ));
            }
        }
        // At the defaults the user's my.cnf already describes the server, as in
        // `missing_include_warning`.
        Err(e) if customized => crate::output::warn(&format!(
            "{service}: could not read {} ({e}) — devy left it unchanged; make sure it has the line `{include_line}` so devy's settings in {} take effect",
            main_cnf.display(),
            target.display()
        )),
        Err(_) => {}
    }
}

/// Largest Homebrew `my.cnf` devy reads to look for its `!includedir` line.
const MY_CNF_CAP: u64 = 1024 * 1024;

/// devy's `[mysqld]` settings for `port` and `args` (from [`sanitized_mysql_args`]),
/// ending with the loopback binds.
fn mysql_ini(port: u16, args: &[(String, String)]) -> String {
    let mut ini = format!("[mysqld]\nport = {}\n", port);
    for (key, val) in args {
        ini.push_str(&format!("{} = {}\n", key, val));
    }
    // Last, so they win: without Homebrew's stock my.cnf nothing else keeps the server
    // (and MySQL's X Protocol listener) off other interfaces. `loose-` makes MariaDB,
    // which has no X Protocol, ignore the second option instead of refusing to start.
    ini.push_str("bind-address = 127.0.0.1\nloose-mysqlx-bind-address = 127.0.0.1\n");
    ini
}

/// The warning for a Homebrew `my.cnf` (whose content is `text`) in `config_dir` that
/// doesn't include `my.cnf.d` while devy's settings are `customized`. At defaults a
/// user's my.cnf (Homebrew's stock MySQL one binds loopback) already describes the
/// server, so only devy's own settings need the include.
fn missing_include_warning(
    config_dir: &std::path::Path,
    text: &str,
    port: u16,
    customized: bool,
) -> Option<String> {
    let include_dir = config_dir.join("my.cnf.d");
    if !customized || includes_dir(text, &include_dir) {
        return None;
    }
    Some(format!(
        "{} does not include {} — devy left it unchanged; add `!includedir {}` to it so port {port} and devy's cli_args settings take effect",
        config_dir.join("my.cnf").display(),
        include_dir.display(),
        include_dir.display()
    ))
}

/// [`missing_include_warning`] for `devy check`: reads the `my.cnf` in `config_dir` and
/// writes nothing. `None` when there is no readable my.cnf (`up` creates one with the
/// include): the read is capped, never blocks, and follows a symlink only to a regular
/// file.
pub(super) fn brew_mysql_include_warning(
    config_dir: &std::path::Path,
    port: u16,
    cli_args: Option<&str>,
) -> Option<String> {
    let customized = port != 3306
        || cli_args.is_some_and(|a| a.split_whitespace().any(|t| allowed_mysql_arg(t).is_some()));
    let bytes =
        crate::fs_safe::read_regular_capped_following_links(&config_dir.join("my.cnf"), MY_CNF_CAP)
            .ok()?;
    missing_include_warning(
        config_dir,
        &String::from_utf8_lossy(&bytes),
        port,
        customized,
    )
}

/// `Module::backend_config_warnings` for MySQL and MariaDB, outside docker and read-only:
/// under brew, the warning for a `my.cnf` that doesn't include devy's `my.cnf.d`; under
/// apt, the warning for a `conf.d/my.cnf` that overrides devy's settings and the default
/// port (existence only, as in [`mysql_family_post_setup`]).
pub(super) fn mysql_family_backend_config_warnings(
    service: &str,
    dep: &Dependency,
    pm: &dyn PackageManager,
) -> Vec<String> {
    let Some(config_dir) = (!dep.docker && matches!(pm.name(), "brew" | "apt"))
        .then(|| pm.service_config_dir(service))
        .flatten()
    else {
        return vec![];
    };
    if pm.name() == "apt" {
        return legacy_conf_d_my_cnf_warning(&config_dir)
            .into_iter()
            .collect();
    }
    let port = super::extra_port(dep, "port", 3306).unwrap_or(3306);
    let cli_args = dep.extra.get("cli_args").and_then(|v| v.as_str());
    brew_mysql_include_warning(&config_dir, port, cli_args)
        .into_iter()
        .collect()
}

/// MySQL/MariaDB `post_setup` for backends with a config dir. Under brew devy always
/// writes `my.cnf.d/devy.cnf`, even at defaults: it carries the loopback bind, which
/// nothing else sets once the stock my.cnf is gone (and brew MariaDB's never did).
/// Elsewhere settings are written only when the port or `cli_args` differ from the
/// defaults, and at the defaults devy's own `devy.cnf` is removed. Under nix the port and
/// cli_args go on the command line instead; an unapplied explicit port is reported by
/// the shared port resolver. Under brew, which takes the port from devy's config file
/// (so the resolver doesn't warn), a missing config dir with non-default settings is
/// warned about here; apt always has one (`/etc/mysql/conf.d`). Under apt every run also
/// warns while a `conf.d/my.cnf` exists, whether devy wrote or removed settings: an older
/// devy left a random port there, which overrides `devy.cnf` and the default port alike.
pub(super) fn mysql_family_post_setup(
    pm: &dyn PackageManager,
    service: &str,
    port: u16,
    cli_args: Option<&str>,
) -> Result<()> {
    let customized = port != 3306 || cli_args.is_some();
    match pm.service_config_dir(service) {
        Some(config_dir) => {
            if customized || pm.name() == "brew" {
                write_mysql_config(pm.name(), service, &config_dir, port, cli_args);
            } else {
                remove_owned_config(&config_dir.join(CONF_D_MYSQL_FILE), service, RESTART_NOTE);
            }
            if pm.name() == "apt"
                && let Some(warning) = legacy_conf_d_my_cnf_warning(&config_dir)
            {
                crate::output::warn(&format!("{service}: {warning}"));
            }
            Ok(())
        }
        None if customized && pm.name() == "brew" => {
            crate::output::warn(&format!(
                "{service}: port {port} and devy's cli_args settings are not applied — devy found no {service} config directory for brew; put these lines in $(brew --prefix)/etc/my.cnf.d/devy.cnf:\n{}",
                mysql_ini(port, &sanitized_mysql_args(cli_args))
            ));
            Ok(())
        }
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

    fn mock_pm(
        name: &'static str,
        dir: &std::path::Path,
    ) -> crate::package_manager::MockPackageManager {
        crate::package_manager::MockPackageManager {
            name,
            config_dir: Some(dir.to_path_buf()),
            ..Default::default()
        }
    }

    /// Scenario "Unwritable file warned on every run".
    #[cfg(unix)]
    #[test]
    fn apt_mysql_unwritable_conf_dir_warns_instead_of_failing() {
        let Some(dir) = crate::test_support::read_only_dir(|_| {}) else {
            return; // root can write anywhere
        };
        let pm = mock_pm("apt", &dir);
        let file = dir.join("devy.cnf").display().to_string();
        for _ in 0..2 {
            let mut result = None;
            let msgs = crate::output::with_warn_messages(|| {
                result = Some(mysql_family_post_setup(&pm, "mysql", 3307, None));
            });
            result.unwrap().unwrap();
            assert!(
                msgs.iter()
                    .any(|m| m.contains(&file) && m.contains("port = 3307")),
                "{msgs:?}"
            );
            // Only a file holding exactly devy's lines silences it, so the warning says
            // so (not "add"), and explains the marker line.
            assert!(
                msgs.iter().any(|m| m.contains("exactly these lines")
                    && m.contains("`# devy-managed`")
                    && m.contains("drop that line")
                    && m.contains("\n# devy-managed\n[mysqld]\nport = 3307\n")
                    && !m.contains("add:")),
                "{msgs:?}"
            );
        }
    }

    /// Scenario "Restart needed": writing and removing an apt database config both report
    /// a change (which is when the restart note is shown); an unchanged file doesn't.
    #[test]
    fn apt_db_config_write_and_removal_report_a_change() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("devy.cnf");
        let content = format!(
            "{}\n{}",
            super::super::loopback::DEVY_MARKER,
            mysql_ini(3307, &[])
        );
        let write = || write_owned_config(&path, &content, "x", "mysql", "x");
        assert_eq!(write(), OwnedWrite::Changed);
        assert_eq!(write(), OwnedWrite::Unchanged);
        assert!(remove_owned_config(&path, "mysql", RESTART_NOTE));
        assert!(!path.exists());
        assert!(!remove_owned_config(&path, "mysql", RESTART_NOTE));
        // Through the module path too: the file is written, then removed at the default.
        let pm = mock_pm("apt", &dir);
        mysql_family_post_setup(&pm, "mysql", 3307, None).unwrap();
        assert!(path.is_file());
        mysql_family_post_setup(&pm, "mysql", 3306, None).unwrap();
        assert!(!path.exists());
    }

    /// A symlink to a regular file that already holds devy's content (a dotfile manager's
    /// link) is left alone; other content is refused rather than written through.
    #[cfg(unix)]
    #[test]
    fn write_owned_config_compares_through_a_symlink_without_writing_through_it() {
        let dir = crate::test_support::tmp_dir();
        let elsewhere = crate::test_support::tmp_dir();
        let target = elsewhere.join("my.cnf");
        let link = dir.join("my.cnf");
        std::fs::write(&target, "a\n").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            write_owned_config(&link, "a\n", "a", "mysql", "x"),
            OwnedWrite::Unchanged
        );
        let OwnedWrite::Failed(msg) = write_owned_config(&link, "b\n", "b", "mysql", "x") else {
            panic!("writing through the symlink must be refused");
        };
        assert!(msg.contains("refusing to write"), "{msg}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "a\n");
        // A link to a FIFO is never read (nor blocked on): it counts as changed.
        let fifo = elsewhere.join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let fifo_link = dir.join("fifo.cnf");
        std::os::unix::fs::symlink(&fifo, &fifo_link).unwrap();
        assert!(matches!(
            write_owned_config(&fifo_link, "a\n", "a", "mysql", "x"),
            OwnedWrite::Failed(_)
        ));
    }

    /// An unwritable brew prefix (no `my.cnf`, and none can be created) never fails `up`:
    /// it warns, naming the `!includedir` line to add.
    #[cfg(unix)]
    #[test]
    fn brew_unwritable_prefix_warns_instead_of_failing() {
        let Some(prefix) = crate::test_support::read_only_dir(|_| {}) else {
            return; // root can write anywhere
        };
        let pm = mock_pm("brew", &prefix);
        let mut result = None;
        let msgs = crate::output::with_warn_messages(|| {
            result = Some(mysql_family_post_setup(&pm, "mysql", 3307, None));
        });
        result.unwrap().unwrap();
        let include = format!("!includedir {}", prefix.join("my.cnf.d").display());
        let my_cnf = prefix.join("my.cnf").display().to_string();
        assert!(
            msgs.iter().any(|m| m.contains("could not create")
                && m.contains(&my_cnf)
                && m.contains(&include)),
            "{msgs:?}"
        );
    }

    /// A `my.cnf` that is a FIFO (or a link to one) is neither read nor blocked on: `up`
    /// warns and skips it, and `devy check` reports nothing for it.
    #[cfg(unix)]
    #[test]
    fn brew_fifo_my_cnf_does_not_hang() {
        let prefix = crate::test_support::tmp_dir();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(prefix.join("my.cnf"))
                .status()
                .unwrap()
                .success()
        );
        let pm = mock_pm("brew", &prefix);
        let mut result = None;
        let msgs = crate::output::with_warn_messages(|| {
            result = Some(mysql_family_post_setup(&pm, "mysql", 3307, None));
        });
        result.unwrap().unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains("could not read") && m.contains("!includedir")),
            "{msgs:?}"
        );
        assert_eq!(brew_mysql_include_warning(&prefix, 3307, None), None);
    }

    /// A symlinked `my.cnf` (dotfile managers link it) is read through to its regular
    /// target, so its include is found.
    #[cfg(unix)]
    #[test]
    fn brew_symlinked_my_cnf_is_read_through() {
        let prefix = crate::test_support::tmp_dir();
        let elsewhere = crate::test_support::tmp_dir();
        let target = elsewhere.join("my.cnf");
        std::fs::write(&target, "[mysqld]\n").unwrap();
        std::os::unix::fs::symlink(&target, prefix.join("my.cnf")).unwrap();
        assert!(brew_mysql_include_warning(&prefix, 3307, None).is_some());
        std::fs::write(
            &target,
            format!("!includedir {}\n", prefix.join("my.cnf.d").display()),
        )
        .unwrap();
        assert_eq!(brew_mysql_include_warning(&prefix, 3307, None), None);
    }

    /// Without a config dir under brew, non-default settings are reported as not applied,
    /// naming the file and lines; defaults and nix stay quiet.
    #[test]
    fn mysql_family_without_config_dir_warns_on_brew() {
        let no_dir = |name| crate::package_manager::MockPackageManager {
            name,
            config_dir: None,
            ..Default::default()
        };
        let file = "$(brew --prefix)/etc/my.cnf.d/devy.cnf";
        let pm = no_dir("brew");
        let msgs = crate::output::with_warn_messages(|| {
            mysql_family_post_setup(&pm, "mariadb", 3307, None).unwrap();
        });
        assert!(
            msgs.iter().any(|m| m.contains("port 3307")
                && m.contains("not applied")
                && m.contains(file)
                && m.contains("port = 3307")),
            "{msgs:?}"
        );
        let msgs = crate::output::with_warn_messages(|| {
            mysql_family_post_setup(&pm, "mysql", 3306, Some("--max-connections=50")).unwrap();
        });
        assert!(
            msgs.iter()
                .any(|m| m.contains(file) && m.contains("max-connections = 50")),
            "{msgs:?}"
        );
        let msgs = crate::output::with_warn_messages(|| {
            mysql_family_post_setup(&pm, "mysql", 3306, None).unwrap();
        });
        assert!(msgs.is_empty(), "{msgs:?}");
        let msgs = crate::output::with_warn_messages(|| {
            mysql_family_post_setup(&no_dir("nix"), "mysql", 3307, None).unwrap();
        });
        assert!(msgs.is_empty(), "{msgs:?}");
    }

    #[test]
    fn mysql_family_backend_config_warnings_cover_brew_and_apt_outside_docker() {
        let prefix = crate::test_support::tmp_dir();
        std::fs::write(prefix.join("my.cnf"), "[mysqld]\n").unwrap();
        let dep = |docker| {
            let mut extra = std::collections::HashMap::new();
            extra.insert(
                "port".into(),
                crate::config::ExtraValue::Number(3307u64.into()),
            );
            let mut dep = Dependency::with_extra("mariadb", extra);
            dep.docker = docker;
            dep
        };
        for service in ["mysql", "mariadb"] {
            let warnings = mysql_family_backend_config_warnings(
                service,
                &dep(false),
                &mock_pm("brew", &prefix),
            );
            assert_eq!(warnings.len(), 1, "{service}: {warnings:?}");
            assert!(warnings[0].contains("!includedir"), "{warnings:?}");
            // The module wires it up.
            assert_eq!(
                crate::modules::get(service)
                    .backend_config_warnings(&dep(false), &mock_pm("brew", &prefix)),
                warnings
            );
            assert!(
                mysql_family_backend_config_warnings(
                    service,
                    &dep(true),
                    &mock_pm("brew", &prefix)
                )
                .is_empty()
            );
            for backend in ["nix", "winget"] {
                assert!(
                    mysql_family_backend_config_warnings(
                        service,
                        &dep(false),
                        &mock_pm(backend, &prefix)
                    )
                    .is_empty(),
                    "{service} under {backend}"
                );
            }
        }
    }

    /// Scenario "Legacy my.cnf at the default port", for `devy check`: under apt a
    /// `conf.d/my.cnf` is reported whatever the port (existence only, never read or
    /// changed); without one, or for a docker dep, nothing is.
    #[test]
    fn mysql_family_backend_config_warnings_report_apt_legacy_my_cnf() {
        let dir = crate::test_support::tmp_dir();
        let pm = mock_pm("apt", &dir);
        let with_port = |port: Option<u64>, docker: bool| {
            let mut extra = std::collections::HashMap::new();
            if let Some(p) = port {
                extra.insert("port".into(), crate::config::ExtraValue::Number(p.into()));
            }
            let mut dep = Dependency::with_extra("mysql", extra);
            dep.docker = docker;
            dep
        };
        for service in ["mysql", "mariadb"] {
            for port in [None, Some(3307)] {
                assert!(
                    mysql_family_backend_config_warnings(service, &with_port(port, false), &pm)
                        .is_empty(),
                    "{service} {port:?}: no my.cnf"
                );
            }
        }
        let foreign = "[client]\npassword = SENTINEL\n[mysqld]\nport = 51000\n";
        std::fs::write(dir.join("my.cnf"), foreign).unwrap();
        let my_cnf = dir.join("my.cnf").display().to_string();
        for service in ["mysql", "mariadb"] {
            for port in [None, Some(3307)] {
                let warnings =
                    mysql_family_backend_config_warnings(service, &with_port(port, false), &pm);
                assert_eq!(warnings.len(), 1, "{service} {port:?}: {warnings:?}");
                assert!(
                    warnings[0].starts_with(&format!("{my_cnf} is read after"))
                        && warnings[0].contains("the default port 3306")
                        && warnings[0].contains(&format!("sudo rm {my_cnf}"))
                        && !warnings[0].contains("SENTINEL"),
                    "{warnings:?}"
                );
                assert_eq!(
                    crate::modules::get(service)
                        .backend_config_warnings(&with_port(port, false), &pm),
                    warnings
                );
                assert!(
                    mysql_family_backend_config_warnings(service, &with_port(port, true), &pm)
                        .is_empty()
                );
            }
        }
        assert_eq!(
            std::fs::read(dir.join("my.cnf")).unwrap(),
            foreign.as_bytes()
        );
        assert_eq!(
            std::fs::read_dir(&*dir).unwrap().count(),
            1,
            "nothing written"
        );
    }

    /// Scenario "MySQL on apt".
    #[test]
    fn apt_mysql_config_is_devy_managed() {
        let dir = crate::test_support::tmp_dir();
        let msgs = crate::output::with_warn_messages(|| {
            mysql_family_post_setup(&mock_pm("apt", &dir), "mysql", 3307, None).unwrap();
        });
        let text = std::fs::read_to_string(dir.join("devy.cnf")).unwrap();
        assert!(
            text.starts_with("# devy-managed\n[mysqld]\nport = 3307\n"),
            "{text}"
        );
        assert!(!dir.join("my.cnf").exists(), "devy never writes my.cnf");
        assert!(msgs.is_empty(), "no my.cnf, no override warning: {msgs:?}");
    }

    /// Scenarios "Foreign file kept" (writing) and "Legacy my.cnf at the default port": a
    /// `my.cnf` in apt's `conf.d` is left byte-identical, and since it is read after
    /// `devy.cnf` every run warns (once) that it overrides devy's settings and the default
    /// port, whether devy writes settings or not, without showing its content.
    #[test]
    fn apt_mysql_writes_devy_cnf_beside_a_foreign_my_cnf() {
        let dir = crate::test_support::tmp_dir();
        let foreign = "[client]\npassword = SENTINEL\n[mysqld]\nport = 51000\n";
        std::fs::write(dir.join("my.cnf"), foreign).unwrap();
        let pm = mock_pm("apt", &dir);
        for service in ["mysql", "mariadb", "mysql"] {
            let msgs = crate::output::with_warn_messages(|| {
                mysql_family_post_setup(&pm, service, 3307, None).unwrap();
            });
            assert_eq!(
                std::fs::read(dir.join("my.cnf")).unwrap(),
                foreign.as_bytes()
            );
            assert!(
                std::fs::read_to_string(dir.join("devy.cnf"))
                    .unwrap()
                    .contains("port = 3307\n")
            );
            assert_legacy_my_cnf_warned_once(&dir, service, &msgs);
        }
        // At the defaults devy removes its `devy.cnf`, but an older devy's random port in
        // `my.cnf` still overrides the default 3306 that DATABASE_URL names.
        for service in ["mysql", "mariadb", "mysql"] {
            let msgs = crate::output::with_warn_messages(|| {
                mysql_family_post_setup(&pm, service, 3306, None).unwrap();
            });
            assert!(!dir.join("devy.cnf").exists());
            assert_eq!(
                std::fs::read(dir.join("my.cnf")).unwrap(),
                foreign.as_bytes()
            );
            assert_legacy_my_cnf_warned_once(&dir, service, &msgs);
        }
    }

    /// `msgs` holds exactly one legacy `conf.d/my.cnf` warning for `service`, naming the
    /// default port and how to delete the file, and never the file's content.
    fn assert_legacy_my_cnf_warned_once(dir: &std::path::Path, service: &str, msgs: &[String]) {
        let my_cnf = dir.join("my.cnf").display().to_string();
        let warned = msgs
            .iter()
            .filter(|m| {
                m.starts_with(&format!("{service}: {my_cnf} is read after"))
                    && m.contains("override devy's settings and the default port 3306")
                    && m.contains(&format!("sudo rm {my_cnf}"))
            })
            .count();
        assert_eq!(warned, 1, "{service}: {msgs:?}");
        assert!(msgs.iter().all(|m| !m.contains("SENTINEL")), "{msgs:?}");
    }

    /// Without a `conf.d/my.cnf`, apt at the defaults is silent.
    #[test]
    fn apt_mysql_defaults_without_my_cnf_are_silent() {
        let dir = crate::test_support::tmp_dir();
        let pm = mock_pm("apt", &dir);
        for service in ["mysql", "mariadb"] {
            let msgs = crate::output::with_warn_messages(|| {
                mysql_family_post_setup(&pm, service, 3306, None).unwrap();
            });
            assert!(msgs.is_empty(), "{service}: {msgs:?}");
        }
        assert_eq!(std::fs::read_dir(&*dir).unwrap().count(), 0);
    }

    /// The warning for an unwritable file names devy's lines, never the existing file's.
    #[cfg(unix)]
    #[test]
    fn unwritable_owned_config_warning_never_shows_the_existing_file() {
        let Some(dir) = crate::test_support::read_only_dir(|d| {
            std::fs::write(d.join("my.cnf"), "[client]\npassword = SENTINEL\n").unwrap();
        }) else {
            return; // root can write anywhere
        };
        let path = dir.join("my.cnf");
        let OwnedWrite::Failed(msg) = write_owned_config(
            &path,
            "[mysqld]\nport = 3307\n",
            "port = 3307",
            "mysql",
            "x",
        ) else {
            panic!("the write must fail");
        };
        assert!(msg.contains("port = 3307") && msg.contains(&path.display().to_string()));
        assert!(!msg.contains("SENTINEL"), "{msg}");
    }

    #[test]
    fn write_owned_config_writes_only_on_change_and_keeps_the_mode() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("sub").join("devy.conf");
        let write = |c: &str| write_owned_config(&path, c, c, "postgresql", "x");
        assert_eq!(write("a\n"), OwnedWrite::Changed);
        assert_eq!(write("a\n"), OwnedWrite::Unchanged);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(write("b\n"), OwnedWrite::Changed);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// A rewrite keeps the replaced file's owner and group; a new file has none to keep.
    #[cfg(unix)]
    #[test]
    fn write_owned_config_keeps_owner_and_group() {
        use std::os::unix::fs::MetadataExt;
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("devy.cnf");
        assert_eq!(
            existing_attrs(&path),
            KeptAttrs {
                mode: 0o644,
                owner: None
            }
        );
        std::fs::write(&path, "a\n").unwrap();
        let before = std::fs::metadata(&path).unwrap();
        assert_eq!(
            existing_attrs(&path).owner,
            Some((before.uid(), before.gid()))
        );
        assert_eq!(
            write_owned_config(&path, "b\n", "b", "mysql", "x"),
            OwnedWrite::Changed
        );
        let after = std::fs::metadata(&path).unwrap();
        assert_ne!(after.ino(), before.ino());
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
    }

    /// When the replaced file belongs to someone devy can't hand the new file to (here
    /// root, stood in through the injected lookup), nothing is replaced: the write fails
    /// with the usual warning naming only devy's lines. Skipped as root, who may chown.
    #[cfg(unix)]
    #[test]
    fn write_owned_config_fails_without_replacing_when_owner_cant_be_kept() {
        use std::os::unix::fs::MetadataExt;
        let dir = crate::test_support::tmp_dir();
        let probe = dir.join("probe");
        std::fs::write(&probe, "").unwrap();
        if std::os::unix::fs::chown(&probe, Some(0), Some(0)).is_ok() {
            return; // root
        }
        std::fs::remove_file(&probe).unwrap();
        let path = dir.join("devy.cnf");
        std::fs::write(&path, "[client]\npassword = SENTINEL\n").unwrap();
        let ino = std::fs::metadata(&path).unwrap().ino();
        let root_owned = |_: &std::path::Path| KeptAttrs {
            mode: 0o640,
            owner: Some((0, 0)),
        };
        let OwnedWrite::Failed(msg) = write_owned_config_keeping(
            &path,
            "[mysqld]\nport = 3307\n",
            "port = 3307",
            "mysql",
            "port 3307 is not applied",
            root_owned,
        ) else {
            panic!("the write must fail when the owner can't be kept");
        };
        assert!(
            msg.starts_with(&format!("mysql: could not write {}", path.display()))
                && msg.contains("its owner and group (0:0)")
                && msg.contains(&format!(
                    "left {} unchanged and port 3307 is not applied",
                    path.display()
                ))
                && msg.contains("`sudo chown` it to yourself or delete it")
                && msg.ends_with("or port = 3307"),
            "{msg}"
        );
        assert!(!msg.contains("SENTINEL"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[client]\npassword = SENTINEL\n"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), ino);
        let names: Vec<_> = std::fs::read_dir(&*dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("devy.cnf")]);
    }

    /// The current user's supplementary groups (`getgroups`), empty on error.
    #[cfg(unix)]
    fn supplementary_groups() -> Vec<u32> {
        // SAFETY: with a size of 0 `getgroups` writes nothing and returns the count.
        let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        let Ok(len) = usize::try_from(n) else {
            return Vec::new();
        };
        let mut groups = vec![0 as libc::gid_t; len];
        // SAFETY: `groups` is a live buffer of exactly `n` gids.
        let n = unsafe { libc::getgroups(n, groups.as_mut_ptr()) };
        let Ok(len) = usize::try_from(n) else {
            return Vec::new();
        };
        groups.truncate(len);
        groups
    }

    /// A rewrite keeps a group other than the user's primary one when the user is a
    /// member of it (no privilege needed). Skipped when the user has only one group.
    #[cfg(unix)]
    #[test]
    fn write_owned_config_keeps_another_group_of_the_user() {
        use std::os::unix::fs::MetadataExt;
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("devy.cnf");
        std::fs::write(&path, "a\n").unwrap();
        let created_gid = std::fs::metadata(&path).unwrap().gid();
        // A group the user can hand the file to: changing to it must succeed unprivileged.
        let Some(other) = supplementary_groups()
            .into_iter()
            .find(|&g| g != created_gid && std::os::unix::fs::chown(&path, None, Some(g)).is_ok())
        else {
            return; // only one group
        };
        let before = std::fs::metadata(&path).unwrap();
        assert_eq!(before.gid(), other);
        assert_eq!(
            write_owned_config(&path, "b\n", "b", "mysql", "x"),
            OwnedWrite::Changed
        );
        let after = std::fs::metadata(&path).unwrap();
        assert_ne!(after.ino(), before.ino(), "replaced, not written in place");
        assert_eq!((after.uid(), after.gid()), (before.uid(), other));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\n");
    }

    #[test]
    fn brew_mysql_include_warning_repeats() {
        let prefix = crate::test_support::tmp_dir();
        std::fs::write(prefix.join("my.cnf"), "[mysqld]\nuser = me\n").unwrap();
        let pm = mock_pm("brew", &prefix);
        for run in 0..2 {
            let msgs = crate::output::with_warn_messages(|| {
                mysql_family_post_setup(&pm, "mysql", 3307, None).unwrap();
            });
            assert!(
                msgs.iter()
                    .any(|m| m.contains("!includedir") && m.contains("3307")),
                "run {run}: {msgs:?}"
            );
            assert!(msgs.iter().all(|m| !m.contains("user = me")), "{msgs:?}");
        }
    }

    /// Scenarios "Port returns to the default" and "Foreign file kept".
    #[test]
    fn default_port_removes_devy_managed_db_config() {
        let dir = crate::test_support::tmp_dir();
        let pm = mock_pm("apt", &dir);
        let root = std::path::Path::new("/tmp");
        for (name, file) in [
            ("postgresql", "devy.conf"),
            ("mysql", "devy.cnf"),
            ("mariadb", "devy.cnf"),
        ] {
            let path = dir.join(file);
            std::fs::write(&path, "# devy-managed\nport = 51000\n").unwrap();
            crate::modules::get(name)
                .post_setup(&Dependency::simple(name), &pm, root)
                .unwrap();
            assert!(!path.exists(), "{name}: devy's {file} must be removed");
        }
        // A `devy.cnf` without the marker, and any `my.cnf` (devy never writes it any
        // more, even one that starts with the marker), are left alone.
        let foreign = "[mysqld]\nport = 3310\n";
        let marked = "# devy-managed\n[mysqld]\nport = 3311\n";
        std::fs::write(dir.join("devy.cnf"), foreign).unwrap();
        std::fs::write(dir.join("my.cnf"), marked).unwrap();
        for name in ["mysql", "mariadb"] {
            crate::modules::get(name)
                .post_setup(&Dependency::simple(name), &pm, root)
                .unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("devy.cnf")).unwrap(),
            foreign
        );
        assert_eq!(std::fs::read_to_string(dir.join("my.cnf")).unwrap(), marked);
    }

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
