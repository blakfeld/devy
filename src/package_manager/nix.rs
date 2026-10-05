use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::PackageManager;
use crate::config::Dependency;
use crate::installers;
use crate::modules::{LaunchSpec, SeedDir};
use crate::output;

pub struct NixPackageManager {
    /// Project-local Nix profile: `<project_root>/.devy/nix-profile`.
    /// All installs target this profile so packages are scoped to the project.
    profile_path: PathBuf,
    /// Cached result of probing whether the installed Nix supports flakes-era
    /// `nix profile` commands. Computed once on first use; safe to cache because
    /// style detection only runs after `ensure_available` completes.
    style: std::sync::OnceLock<NixStyle>,
    /// Recorded in each unit as `DEVY_PROJECT_ROOT`, and used to recognize this
    /// project's units under outdated names.
    project_root: PathBuf,
    /// `modules::helpers::project_slug` of the project: part of every unit and log name,
    /// so services of different projects and checkouts never share a unit.
    slug: String,
    services: ServiceHost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NixStyle {
    /// Modern Nix with flakes: `nix profile install --profile <path> nixpkgs#attr`
    Profile,
    /// Legacy Nix: `nix-env --profile <path> -iA nixpkgs.attr`
    Env,
}

/// Searches PATH (ignoring entries inside the project root) then well-known Nix
/// installation directories for a binary.
/// Returns `None` only if the binary cannot be found anywhere.
fn find_nix_binary(name: &str) -> Option<PathBuf> {
    if let Some(p) = crate::fs_safe::which_outside_project(name) {
        return Some(p);
    }
    // Standard paths for Nix installations that may not be on PATH in non-login shells:
    //   - Determinate Installer (macOS & Linux): /nix/var/nix/profiles/default/bin
    //   - Single-user official installer: ~/.nix-profile/bin
    //   - NixOS system profile: /run/current-system/sw/bin
    let home_nix = std::env::var("HOME")
        .map(|h| format!("{h}/.nix-profile/bin/{name}"))
        .unwrap_or_default();
    let candidates = [
        format!("/nix/var/nix/profiles/default/bin/{name}"),
        home_nix,
        format!("/run/current-system/sw/bin/{name}"),
    ];
    candidates
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .find(|p| p.exists())
}

/// Resolves a Nix binary by name, falling back to its Determinate/multi-user location
/// when not found. Never a bare name: `Command` would search the full PATH, including
/// the project directories `find_nix_binary` skips.
fn resolve_nix_binary(name: &str) -> PathBuf {
    find_nix_binary(name)
        .unwrap_or_else(|| PathBuf::from(format!("/nix/var/nix/profiles/default/bin/{name}")))
}

impl NixPackageManager {
    /// Construct with an explicit project root so the profile path is always
    /// relative to the project, not the shell's current working directory.
    /// `project_name` is devy.yml's `name`, which with the root makes the project slug
    /// in service names.
    pub fn for_project(project_root: &Path, project_name: &str) -> Self {
        Self::with_services(project_root, project_name, ServiceHost::native())
    }

    fn with_services(project_root: &Path, project_name: &str, services: ServiceHost) -> Self {
        let profile_path = project_root.join(".devy").join("nix-profile");
        Self {
            profile_path,
            style: std::sync::OnceLock::new(),
            project_root: project_root.to_path_buf(),
            slug: crate::modules::helpers::project_slug(project_name, project_root),
            services,
        }
    }

    fn profile_bin(&self) -> PathBuf {
        self.profile_path.join("bin")
    }

    /// Resolves the `nix` binary on every call so post-bootstrap installs are
    /// picked up without requiring the caller to reconstruct the PM struct.
    fn nix_bin(&self) -> PathBuf {
        resolve_nix_binary("nix")
    }

    fn nix_env_bin(&self) -> PathBuf {
        resolve_nix_binary("nix-env")
    }

    fn effective_style(&self) -> NixStyle {
        *self.style.get_or_init(|| detect_style(&self.nix_bin()))
    }

    /// `<project_root>/.devy/nix-env-attrs.json`, beside the profile.
    fn env_manifest_path(&self) -> PathBuf {
        self.profile_path.with_file_name("nix-env-attrs.json")
    }
}

/// Probe whether the installed Nix supports `nix profile` (flakes-era CLI).
fn detect_style(nix_bin: &Path) -> NixStyle {
    let ok = Command::new(nix_bin)
        .args(["profile", "list"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        NixStyle::Profile
    } else {
        output::info("nix profile list unavailable — using legacy nix-env install style");
        NixStyle::Env
    }
}

// ── Profile (flakes) helpers ──────────────────────────────────────────────────

fn profile_list_json(nix_bin: &Path, profile_path: &Path) -> Result<serde_json::Value> {
    let out = Command::new(nix_bin)
        .args(["profile", "list", "--json", "--profile"])
        .arg(profile_path)
        .output()
        .context("Failed to run `nix profile list --json`")?;
    serde_json::from_slice(&out.stdout).context("Failed to parse `nix profile list --json` output")
}

/// Finds the profile entry for nixpkgs attribute `attr`: `Some` when it's installed,
/// holding its version when one is known.
///
/// Entries are matched on the exact attribute devy installed (the last component of
/// `attrPath`), so `nodejs_22` and `nodejs` are different packages, and mapped names like
/// `mysql84` (pname `mysql`) or `jdk21` (pname `openjdk`) are recognized. Entries without
/// an `attrPath` fall back to `pname`, then to the element name.
///
/// The version is the entry's `version` field, or (Nix ≥ 2.20 records none) the one
/// derived from its store paths.
fn profile_find_pkg(json: &serde_json::Value, attr: &str) -> Option<Option<String>> {
    profile_find_entry(json, attr).map(|entry| {
        if let Some(v) = entry.get("version").and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
        let paths: Vec<&str> = entry
            .get("storePaths")
            .and_then(|p| p.as_array())
            .map(|a| a.iter().filter_map(|p| p.as_str()).collect())
            .unwrap_or_default();
        version_from_store_paths(&paths)
    })
}

/// The version in the name of an entry's main output store path, split the way Nix's
/// `parseDrvName` does: at the first `-` followed by a digit. The main output is the
/// name that prefixes every other output's name (`mysql-8.4.11`, not
/// `mysql-8.4.11-man`). `None` when there's no main output or its name has no version.
fn version_from_store_paths(paths: &[&str]) -> Option<String> {
    // `/nix/store/<hash>-<name>`; hashes never contain `-`.
    let names: Vec<&str> = paths
        .iter()
        .filter_map(|p| p.rsplit('/').next()?.split_once('-').map(|(_, n)| n))
        .collect();
    let main = names
        .iter()
        .find(|n| names.iter().all(|other| other.starts_with(**n)))?;
    let split = main
        .char_indices()
        .find(|&(i, c)| c == '-' && main[i + 1..].starts_with(|d: char| d.is_ascii_digit()))?
        .0;
    Some(main[split + 1..].to_string())
}

/// The profile entry for nixpkgs attribute `attr` (see `profile_find_pkg` for matching).
fn profile_find_entry<'a>(
    json: &'a serde_json::Value,
    attr: &str,
) -> Option<&'a serde_json::Value> {
    let matches = |entry: &serde_json::Value, key: Option<&str>| {
        if let Some(path) = entry.get("attrPath").and_then(|a| a.as_str()) {
            // `legacyPackages.<system>.<attr>`; attrs may themselves contain dots.
            return path == attr || path.ends_with(&format!(".{attr}"));
        }
        if let Some(pname) = entry.get("pname").and_then(|p| p.as_str()) {
            return pname == attr;
        }
        key == Some(attr)
    };
    // Old format (Nix < 2.18): a JSON array of entries.
    if let Some(arr) = json.as_array() {
        return arr.iter().find(|e| matches(e, None));
    }
    // New format (Nix ≥ 2.18): {"version":2|3,"elements":{"<name>":{...}}}.
    let elements = json.get("elements")?.as_object()?;
    elements
        .iter()
        .find(|(key, e)| matches(e, Some(key.as_str())))
        .map(|(_, e)| e)
}

/// Candidate `bin/` directories of `attr`'s own store paths in the profile. Outputs are
/// listed in no particular order (e.g. `-man` may come first), so callers pick the first
/// one that exists.
fn profile_package_bins(json: &serde_json::Value, attr: &str) -> Vec<PathBuf> {
    profile_find_entry(json, attr)
        .and_then(|e| e.get("storePaths"))
        .and_then(|p| p.as_array())
        .map(|paths| {
            paths
                .iter()
                .filter_map(|p| p.as_str())
                .map(|p| PathBuf::from(p).join("bin"))
                .collect()
        })
        .unwrap_or_default()
}

/// Profile priority for packages that must win name conflicts (lower wins; the default
/// is 5). mariadb and mysql both ship `mysql`, `mysqldump`, `mysqld` and more, so
/// mariadb's tools take those names whichever of the two is installed first.
fn install_priority(attr: &str) -> Option<u32> {
    (attr == "mariadb" || attr.starts_with("mariadb_")).then_some(4)
}

/// The arguments (after the binary) and extra environment for installing `attr`.
/// `allow_unfree` sets `NIXPKGS_ALLOW_UNFREE=1` and `allow_insecure` sets
/// `NIXPKGS_ALLOW_INSECURE=1` on this one child; flake references evaluate purely, so
/// the profile style also needs `--impure` to see them.
#[derive(Debug, PartialEq, Eq)]
struct InstallCmd {
    args: Vec<String>,
    env: Vec<(&'static str, &'static str)>,
}

fn install_cmd(
    style: NixStyle,
    profile: &Path,
    attr: &str,
    allow_unfree: bool,
    allow_insecure: bool,
) -> InstallCmd {
    let profile = profile.to_string_lossy().into_owned();
    let mut args = match style {
        NixStyle::Profile => vec![
            "profile".into(),
            "install".into(),
            "--profile".into(),
            profile,
        ],
        NixStyle::Env => vec!["--profile".into(), profile],
    };
    if style == NixStyle::Profile {
        if let Some(p) = install_priority(attr) {
            args.extend(["--priority".into(), p.to_string()]);
        }
        if allow_unfree || allow_insecure {
            args.push("--impure".into());
        }
    }
    match style {
        NixStyle::Profile => args.push(format!("nixpkgs#{attr}")),
        NixStyle::Env => args.extend(["-iA".into(), format!("nixpkgs.{attr}")]),
    }
    let mut env = Vec::new();
    if allow_unfree {
        env.push(("NIXPKGS_ALLOW_UNFREE", "1"));
    }
    if allow_insecure {
        env.push(("NIXPKGS_ALLOW_INSECURE", "1"));
    }
    InstallCmd { args, env }
}

// ── nix-env (legacy) helpers ──────────────────────────────────────────────────

fn env_list_json(nix_env_bin: &Path, profile_path: &Path) -> Result<serde_json::Value> {
    let out = Command::new(nix_env_bin)
        .args(["--profile"])
        .arg(profile_path)
        .args(["-q", "--json"])
        .output()
        .context("Failed to run `nix-env -q --json`")?;
    serde_json::from_slice(&out.stdout).context("Failed to parse `nix-env -q --json` output")
}

/// Maps each nixpkgs attribute devy installed with `nix-env` to the derivation name that
/// install produced (`{"mysql84": "mysql-8.4.11"}`). `nix-env -q` doesn't record the
/// attribute an installed package came from, so this is the only link back to it.
type EnvManifest = std::collections::BTreeMap<String, String>;

/// A missing, unreadable or malformed manifest reads as empty: lookups then fall back to
/// matching package names.
fn read_env_manifest(path: &Path) -> EnvManifest {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Writes `manifest` to a temp file beside `path`, then renames it into place.
fn write_env_manifest(path: &Path, manifest: &EnvManifest) -> Result<()> {
    let body = serde_json::to_vec_pretty(manifest).context("Failed to serialize nix-env attrs")?;
    crate::fs_safe::write_atomic(path, &body, 0o644)
}

/// Finds the `nix-env -q --json` entry for nixpkgs attribute `attr`: `Some` when it's
/// installed, holding its version when one is known.
///
/// When the manifest records `attr`, only an entry with exactly that derivation name
/// matches. Attributes without a record (profiles from before the manifest) fall back to
/// matching `pname`, skipping entries another attribute's record claims, so `nodejs_22`'s
/// install never satisfies `nodejs`.
fn env_find_attr(
    json: &serde_json::Value,
    manifest: &EnvManifest,
    attr: &str,
) -> Option<Option<String>> {
    let entries = json.as_object()?;
    fn name_of<'a>(key: &'a str, e: &'a serde_json::Value) -> &'a str {
        e.get("name").and_then(|n| n.as_str()).unwrap_or(key)
    }
    let entry = match manifest.get(attr) {
        Some(name) => entries
            .iter()
            .find_map(|(key, e)| (name_of(key, e) == name).then_some(e)),
        None => entries.iter().find_map(|(key, e)| {
            let claimed = manifest.values().any(|n| n == name_of(key, e));
            let pname_matches = e.get("pname").and_then(|p| p.as_str()) == Some(attr);
            (pname_matches && !claimed).then_some(e)
        }),
    }?;
    Some(
        entry
            .get("version")
            .and_then(|v| v.as_str())
            .map(String::from),
    )
}

/// Arguments (after `nix-env`) that print the derivation `attr` would install. They
/// resolve `nixpkgs.<attr>` exactly as `-iA` does: `<nixpkgs>` from `NIX_PATH` may be a
/// different revision, whose name would never show up in the profile.
fn env_query_name_args(attr: &str) -> Vec<String> {
    vec!["-qaA".into(), format!("nixpkgs.{attr}"), "--json".into()]
}

/// The derivation name in `nix-env -qaA nixpkgs.<attr> --json` output
/// (`{"nixpkgs.<attr>": {"name": …}}`).
fn env_drv_name(json: &serde_json::Value) -> Option<String> {
    let mut entries = json.as_object()?.values();
    let entry = entries.next()?;
    if entries.next().is_some() {
        return None;
    }
    entry.get("name")?.as_str().map(String::from)
}

/// Runs `install` and, once it succeeds, records `attr` as `name` in the manifest. With
/// no name (the query failed) any stale record is dropped instead, so later lookups fall
/// back to matching package names rather than reinstalling forever.
fn env_install_and_record(
    manifest_path: &Path,
    attr: &str,
    name: Option<String>,
    install: impl FnOnce() -> Result<()>,
) -> Result<()> {
    install()?;
    let mut manifest = read_env_manifest(manifest_path);
    let changed = match name {
        Some(name) => manifest.insert(attr.to_string(), name.clone()) != Some(name),
        None => manifest.remove(attr).is_some(),
    };
    if changed {
        write_env_manifest(manifest_path, &manifest)?;
    }
    Ok(())
}

// ── Service management — shared ───────────────────────────────────────────────

/// Error for a service with no nix launch definition (only generic-module services).
fn unsupported_service(name: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Service management for '{}' is not yet supported with the nix backend. \
         Start it manually using the binary in .devy/nix-profile/bin/.",
        name
    )
}

/// `[<exec_dir>/<exec>, args…]`: the full command line a unit runs.
fn program_arguments(launch: &LaunchSpec, exec_dir: &Path) -> Vec<String> {
    std::iter::once(exec_dir.join(&launch.exec).to_string_lossy().into_owned())
        .chain(launch.args.iter().cloned())
        .collect()
}

/// Refuses a unit whose command line, environment or working directory holds a control
/// character. Unit files are line-oriented (systemd also ends a line at a lone `\r` or
/// NUL), so one in a devy.yml value such as a password — or in the project path that
/// becomes `WorkingDirectory=` — could otherwise inject directives. Errors name the
/// variable or argument position, never the value.
fn ensure_no_control_chars(
    program: &[String],
    env: &[(String, String)],
    working_dir: Option<&Path>,
) -> Result<()> {
    if working_dir.is_some_and(|d| d.to_string_lossy().chars().any(char::is_control)) {
        bail!("service working directory contains a control character (newline, CR, NUL, …)");
    }
    if let Some(i) = program.iter().position(|a| a.chars().any(char::is_control)) {
        bail!("service argument {i} contains a control character (newline, CR, NUL, …)");
    }
    if let Some((k, _)) = env
        .iter()
        .find(|(k, v)| k.chars().any(char::is_control) || v.chars().any(char::is_control))
    {
        bail!(
            "service environment variable {} contains a control character (newline, CR, NUL, …)",
            k.escape_debug()
        );
    }
    Ok(())
}

/// The launch environment plus a PATH that finds the project profile's binaries first,
/// so wrapper scripts (kafka, rabbitmq) can find their helpers.
fn unit_environment(launch: &LaunchSpec, profile_bin: &Path) -> Vec<(String, String)> {
    let mut env = launch.env.clone();
    if !env.iter().any(|(k, _)| k == "PATH") {
        env.push((
            "PATH".into(),
            format!(
                "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                profile_bin.to_string_lossy()
            ),
        ));
    }
    env
}

/// Runs the launch spec's one-time init step unless its marker already exists.
/// `init.cmd[0]` is resolved in `exec_dir`; PATH points at `profile_bin`.
fn run_init(name: &str, launch: &LaunchSpec, exec_dir: &Path, profile_bin: &Path) -> Result<()> {
    let Some(init) = &launch.init else {
        return Ok(());
    };
    if init.marker.exists() {
        return Ok(());
    }
    let Some((prog, args)) = init.cmd.split_first() else {
        bail!("Failed to initialize {name}: empty init command");
    };
    output::step(&format!("Initializing {name} ({})", init.cmd.join(" ")));
    let out = Command::new(exec_dir.join(prog))
        .args(args)
        .envs(unit_environment(launch, profile_bin))
        .current_dir(launch.working_dir.as_deref().unwrap_or(Path::new("/")))
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("Failed to initialize {name}: could not run `{prog}`"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "Failed to initialize {name}: `{}` exited with {}\n{}",
            init.cmd.join(" "),
            out.status,
            stderr.trim_end()
        );
    }
    Ok(())
}

/// Applies the launch spec's package-dependent parts against `package_root` (the
/// `exec_package`'s store path): seeds each missing `seed_dirs` destination, appends
/// the `conditional_args` whose package path exists, and sets `package_env`. Returns
/// the spec to launch.
fn prepare_package_files(
    name: &str,
    launch: &LaunchSpec,
    package_root: Option<&Path>,
) -> Result<LaunchSpec> {
    let mut launch = launch.clone();
    if launch.seed_dirs.is_empty()
        && launch.conditional_args.is_empty()
        && launch.package_env.is_empty()
    {
        return Ok(launch);
    }
    let Some(root) = package_root else {
        bail!("Failed to prepare {name} config: could not locate the package's config directory");
    };
    for seed in &launch.seed_dirs {
        if seed.to.exists() {
            continue;
        }
        let from = root.join(&seed.from_package);
        if !from.is_dir() {
            bail!(
                "Failed to prepare {name} config: could not locate the package's config directory"
            );
        }
        output::step(&format!(
            "Copying {name} {} to {}",
            seed.from_package,
            seed.to.display()
        ));
        seed_dir(&from, seed).with_context(|| format!("Failed to prepare {name} config"))?;
    }
    let extra: Vec<String> = launch
        .conditional_args
        .iter()
        .filter(|c| root.join(&c.package_path).exists())
        .flat_map(|c| c.args.iter().cloned())
        .collect();
    launch.args.extend(extra);
    let env: Vec<(String, String)> = launch
        .package_env
        .iter()
        .map(|(var, path)| {
            let value = if path.is_empty() {
                root.to_path_buf()
            } else {
                root.join(path)
            };
            (var.clone(), value.to_string_lossy().into_owned())
        })
        .collect();
    launch.env.extend(env);
    Ok(launch)
}

/// Copies `from` to `seed.to` and applies `seed.rewrites`, through a sibling staging
/// directory, so an interrupted seed is redone on the next start instead of leaving a
/// partial copy behind.
fn seed_dir(from: &Path, seed: &SeedDir) -> Result<()> {
    let to = seed.to.as_path();
    let parent = to.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("could not create {}", parent.display()))?;
    let file_name = to.file_name().unwrap_or_default().to_string_lossy();
    let staging = parent.join(format!(".{file_name}.seeding"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("could not remove {}", staging.display()))?;
    }
    copy_writable(from, &staging)?;
    for rewrite in &seed.rewrites {
        let file = staging.join(&rewrite.file);
        if !file.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("could not read {}", file.display()))?;
        let rewritten = text.replace(&rewrite.from, &rewrite.to);
        crate::fs_safe::write_atomic(&file, rewritten.as_bytes(), 0o644)
            .with_context(|| format!("could not write {}", file.display()))?;
    }
    std::fs::rename(&staging, to)
        .with_context(|| format!("could not move {} to {}", staging.display(), to.display()))
}

/// Recursively copies `from` (following symlinks) to `to`, adding owner-write to every
/// copied entry: Nix store files are read-only.
fn copy_writable(from: &Path, to: &Path) -> Result<()> {
    let copy_err = || format!("could not copy {} to {}", from.display(), to.display());
    if std::fs::metadata(from).with_context(copy_err)?.is_dir() {
        std::fs::create_dir(to).with_context(copy_err)?;
        for entry in std::fs::read_dir(from).with_context(copy_err)? {
            let entry = entry.with_context(copy_err)?;
            copy_writable(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(from, to).with_context(copy_err)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(to).with_context(copy_err)?.permissions();
        perms.set_mode(perms.mode() | 0o200);
        std::fs::set_permissions(to, perms).with_context(copy_err)?;
    }
    Ok(())
}

// ── Service management — unit names and files ────────────────────────────────

/// The service manager nix-run services live under: launchd agents on macOS, systemd
/// user units on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitKind {
    Launchd,
    Systemd,
}

impl UnitKind {
    /// This platform's service manager.
    fn native() -> Self {
        if cfg!(target_os = "macos") {
            Self::Launchd
        } else {
            Self::Systemd
        }
    }
}

/// The launchd label, systemd unit and log file name of one nix-run service.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceNames {
    /// `sh.devy.<slug>.<name>`
    label: String,
    /// `devy-<slug>-<name>.service`
    unit: String,
    /// `devy-<slug>-<name>.log`, in devy's private per-user directory.
    log_file: String,
}

/// The names of service `name` in the project with slug `slug` (see
/// `modules::helpers::project_slug`). With no slug, the project-less names devy used
/// before services were scoped to a project: `sh.devy.<name>`, `devy-<name>.service`
/// and `<name>.log`.
fn service_names(slug: Option<&str>, name: &str) -> ServiceNames {
    match slug {
        Some(slug) => ServiceNames {
            label: format!("sh.devy.{slug}.{name}"),
            unit: format!("devy-{slug}-{name}.service"),
            log_file: format!("devy-{slug}-{name}.log"),
        },
        None => ServiceNames {
            label: format!("sh.devy.{name}"),
            unit: format!("devy-{name}.service"),
            log_file: format!("{name}.log"),
        },
    }
}

impl ServiceNames {
    /// What the service manager calls the unit: the label or the unit name.
    fn id(&self, kind: UnitKind) -> &str {
        match kind {
            UnitKind::Launchd => &self.label,
            UnitKind::Systemd => &self.unit,
        }
    }

    /// The unit's file name in the unit directory.
    fn file_name(&self, kind: UnitKind) -> String {
        match kind {
            UnitKind::Launchd => format!("{}.plist", self.label),
            UnitKind::Systemd => self.unit.clone(),
        }
    }
}

/// The project slug in the file name of a per-project unit of service `name`, e.g.
/// `app-1a2b3c4d` in `sh.devy.app-1a2b3c4d.redis.plist`. `None` for any other file.
fn slug_in_file_name<'a>(kind: UnitKind, file_name: &'a str, name: &str) -> Option<&'a str> {
    let slug = match kind {
        UnitKind::Launchd => {
            let (slug, rest) = file_name
                .strip_prefix("sh.devy.")?
                .strip_suffix(".plist")?
                .split_once('.')?;
            (rest == name).then_some(slug)?
        }
        UnitKind::Systemd => file_name
            .strip_prefix("devy-")?
            .strip_suffix(".service")?
            .strip_suffix(name)?
            .strip_suffix('-')?,
    };
    (!slug.is_empty()).then_some(slug)
}

/// A unit file of a service under an outdated name that belongs to this project.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StaleUnit {
    /// The launchd label or systemd unit name.
    id: String,
    path: PathBuf,
}

/// What `find_stale_units` reads from a unit file.
#[derive(Debug, Default, PartialEq, Eq)]
struct UnitInfo {
    /// The command line and working directory.
    paths: Vec<String>,
    /// The recorded `DEVY_PROJECT_ROOT`.
    project_root: Option<String>,
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The unescaped `<string>` values that follow `<key>{key}</key>` in `xml`: the single
/// string, or each string of an array.
fn plist_values(xml: &str, key: &str) -> Vec<String> {
    let marker = format!("<key>{}</key>", xml_escape(key));
    let Some(start) = xml.find(&marker) else {
        return Vec::new();
    };
    let rest = xml[start + marker.len()..].trim_start();
    let body = if let Some(array) = rest.strip_prefix("<array>") {
        &array[..array.find("</array>").unwrap_or(array.len())]
    } else if rest.starts_with("<string>") {
        rest.find("</string>")
            .map_or(rest, |end| &rest[..end + "</string>".len()])
    } else {
        return Vec::new();
    };
    body.split("<string>")
        .skip(1)
        .filter_map(|s| s.split_once("</string>"))
        .map(|(v, _)| xml_unescape(v))
        .collect()
}

/// Reads a plist written by `launchagent_plist`. Its keys are looked up only in their own
/// section, so an environment variable named like a top-level key is never mistaken for
/// it: `ProgramArguments` comes before `EnvironmentVariables`, `WorkingDirectory` after.
fn parse_plist(xml: &str) -> UnitInfo {
    const ENV_KEY: &str = "<key>EnvironmentVariables</key>";
    let (before, env, after) = match xml.split_once(ENV_KEY) {
        Some((before, rest)) => match rest.split_once("</dict>") {
            Some((env, after)) => (before, env, after),
            None => (before, rest, ""),
        },
        None => (xml, "", xml),
    };
    let mut paths = plist_values(before, "ProgramArguments");
    paths.extend(plist_values(after, "WorkingDirectory"));
    UnitInfo {
        paths,
        project_root: plist_values(env, "DEVY_PROJECT_ROOT").into_iter().next(),
    }
}

/// Splits a unit file value into words, undoing `systemd_quote`: double-quoted words
/// with C-style escapes, or bare words; `%%` is `%`, and with `exec` `$$` is `$`.
fn systemd_words(s: &str, exec: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            return words;
        };
        let quoted = first == '"';
        if quoted {
            chars.next();
        }
        let mut word = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' if quoted => break,
                c if c.is_whitespace() && !quoted => break,
                '\\' if quoted => match chars.next() {
                    Some('n') => word.push('\n'),
                    Some('r') => word.push('\r'),
                    Some(other) => word.push(other),
                    None => {}
                },
                '%' if chars.peek() == Some(&'%') => {
                    chars.next();
                    word.push('%');
                }
                '$' if exec && chars.peek() == Some(&'$') => {
                    chars.next();
                    word.push('$');
                }
                c => word.push(c),
            }
        }
        words.push(word);
    }
}

/// Reads a unit written by `systemd_unit`.
fn parse_systemd_unit(unit: &str) -> UnitInfo {
    let mut info = UnitInfo::default();
    for line in unit.lines() {
        if let Some(v) = line.strip_prefix("ExecStart=") {
            info.paths.extend(systemd_words(v, true));
        } else if let Some(v) = line.strip_prefix("WorkingDirectory=") {
            info.paths.push(v.replace("%%", "%"));
        } else if let Some(v) = line.strip_prefix("Environment=") {
            for word in systemd_words(v, false) {
                if let Some(root) = word.strip_prefix("DEVY_PROJECT_ROOT=") {
                    info.project_root = Some(root.to_string());
                }
            }
        }
    }
    info
}

fn parse_unit(kind: UnitKind, contents: &str) -> UnitInfo {
    match kind {
        UnitKind::Launchd => parse_plist(contents),
        UnitKind::Systemd => parse_systemd_unit(contents),
    }
}

/// Whether one of `values` (an argument, a `--flag=value` argument's value, or a working
/// directory) is inside `root`'s `.devy/data` or `.devy/nix-profile`. Paths are compared
/// by component, so `/src/app2/.devy/data` is not inside `/src/app`.
fn points_into_project(values: &[String], root: &Path) -> bool {
    let devy = root.join(".devy");
    let (data, profile) = (devy.join("data"), devy.join("nix-profile"));
    values.iter().any(|v| {
        std::iter::once(v.as_str())
            .chain(v.split_once('=').map(|(_, value)| value))
            .map(Path::new)
            .any(|p| p.starts_with(&data) || p.starts_with(&profile))
    })
}

/// The unit files in `dir` that run service `name` for the project at `root` under an
/// outdated name:
/// - the legacy name, when its command line or working directory points into `root`'s
///   `.devy/data` or `.devy/nix-profile`;
/// - a per-project name with a slug other than `slug` but the same root hash (the
///   project `name` changed), when its `DEVY_PROJECT_ROOT` is `root`.
///
/// Only regular files are considered; an unreadable directory, or a root that isn't
/// valid UTF-8, has none.
fn find_stale_units(
    kind: UnitKind,
    dir: &Path,
    slug: &str,
    name: &str,
    root: &Path,
) -> Vec<StaleUnit> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let legacy_file = service_names(None, name).file_name(kind);
    // The hash part of the slug: it depends only on the root, so it's unchanged by a rename.
    let hash = slug.rsplit_once('-').map_or(slug, |(_, h)| h);
    // Units record their root as UTF-8: a root that isn't can't be matched faithfully,
    // and a lossy match could retire or adopt another project's unit.
    let Some(root_str) = root.to_str() else {
        return Vec::new();
    };
    let mut stale: Vec<StaleUnit> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().into_string().ok()?;
            let legacy = file_name == legacy_file;
            if !legacy {
                let other = slug_in_file_name(kind, &file_name, name)?;
                if other == slug || other.rsplit_once('-').map(|(_, h)| h) != Some(hash) {
                    return None;
                }
            }
            let path = entry.path();
            if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                return None;
            }
            let info = parse_unit(kind, &std::fs::read_to_string(&path).ok()?);
            let owned = if legacy {
                points_into_project(&info.paths, root)
            } else {
                info.project_root.as_deref() == Some(root_str)
            };
            let id = match kind {
                UnitKind::Launchd => file_name.strip_suffix(".plist")?.to_string(),
                UnitKind::Systemd => file_name,
            };
            owned.then_some(StaleUnit { id, path })
        })
        .collect();
    stale.sort();
    stale
}

/// A devy unit whose recorded project root is a removed checkout (see
/// `config::is_removed_checkout`): what `devy prune` removes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrphanedUnit {
    /// The launchd label or systemd unit name.
    pub id: String,
    pub path: PathBuf,
    /// The recorded `DEVY_PROJECT_ROOT`.
    pub root: String,
}

/// The launchd label or systemd unit name of unit file `file_name` when it is named like
/// a devy unit (`sh.devy.*.plist`, `devy-*.service`) and holds only the characters devy
/// puts in unit names, so it can never read as a flag or a pattern to the service manager.
fn devy_unit_id(kind: UnitKind, file_name: &str) -> Option<&str> {
    let (id, rest) = match kind {
        UnitKind::Launchd => {
            let id = file_name.strip_suffix(".plist")?;
            (id, id.strip_prefix("sh.devy.")?)
        }
        UnitKind::Systemd => (
            file_name,
            file_name.strip_prefix("devy-")?.strip_suffix(".service")?,
        ),
    };
    let valid = !rest.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '@'));
    valid.then_some(id)
}

/// Reads unit file `path` of unit `id` for `devy prune`: its recorded root, when it is a
/// regular file recording a root that is a removed checkout. A plist whose `Label` isn't
/// `id` is skipped: launchctl would stop and unload the wrong agent, leaving it running.
fn orphaned_root(kind: UnitKind, id: &str, path: &Path) -> Option<String> {
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    if kind == UnitKind::Launchd {
        // `Label` comes before `EnvironmentVariables`, so an environment variable named
        // `Label` is never read as it.
        let top = contents
            .split("<key>EnvironmentVariables</key>")
            .next()
            .unwrap_or_default();
        if plist_values(top, "Label").first().map(String::as_str) != Some(id) {
            return None;
        }
    }
    let root = parse_unit(kind, &contents).project_root?;
    crate::config::is_removed_checkout(&root).then_some(root)
}

/// The devy units in `dir` whose recorded `DEVY_PROJECT_ROOT` is a removed checkout.
/// Units without a recorded root (legacy ones) are never included: their owner is
/// unknown. Only regular files are considered; an unreadable directory has none.
fn find_orphaned_units(kind: UnitKind, dir: &Path) -> Vec<OrphanedUnit> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut units: Vec<OrphanedUnit> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().into_string().ok()?;
            let id = devy_unit_id(kind, &file_name)?.to_string();
            let path = entry.path();
            let root = orphaned_root(kind, &id, &path)?;
            Some(OrphanedUnit { id, path, root })
        })
        .collect();
    units.sort();
    units
}

// ── Service management — launchd plist ────────────────────────────────────────

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A launchd agent plist that runs `program` with `env` in `working_dir`, logging to `log`.
fn launchagent_plist(
    label: &str,
    program: &[String],
    env: &[(String, String)],
    working_dir: Option<&Path>,
    log: &Path,
) -> String {
    let working_dir = working_dir
        .map(|d| {
            format!(
                "    <key>WorkingDirectory</key>\n    <string>{}</string>\n",
                xml_escape(&d.to_string_lossy())
            )
        })
        .unwrap_or_default();
    let args: String = program
        .iter()
        .map(|a| format!("        <string>{}</string>\n", xml_escape(a)))
        .collect();
    let env_entries: String = env
        .iter()
        .map(|(k, v)| {
            format!(
                "        <key>{}</key>\n        <string>{}</string>\n",
                xml_escape(k),
                xml_escape(v)
            )
        })
        .collect();
    let label_e = xml_escape(label);
    let log_e = xml_escape(&log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label_e}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>EnvironmentVariables</key>
    <dict>
{env_entries}    </dict>
{working_dir}    <key>KeepAlive</key>
    <false/>
    <key>RunAtLoad</key>
    <false/>
    <key>StandardOutPath</key>
    <string>{log_e}</string>
    <key>StandardErrorPath</key>
    <string>{log_e}</string>
</dict>
</plist>
"#,
    )
}

// ── Service management — systemd unit ─────────────────────────────────────────

/// Quotes one word for a systemd unit file: double-quoted with C-style escapes,
/// and `%` doubled so specifiers are not expanded. `$` is doubled too when `exec`,
/// since ExecStart expands `$VAR`.
fn systemd_quote(s: &str, exec: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            // systemd also ends a line at a lone CR; callers reject control characters,
            // but never emit a raw one.
            '\r' => out.push_str("\\r"),
            '%' => out.push_str("%%"),
            '$' if exec => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A systemd user unit that runs `program` with `env` in `working_dir`.
fn systemd_unit(
    name: &str,
    program: &[String],
    env: &[(String, String)],
    working_dir: Option<&Path>,
) -> String {
    // WorkingDirectory= takes a bare path; only specifiers need escaping.
    let working_dir = working_dir
        .map(|d| {
            format!(
                "WorkingDirectory={}\n",
                d.to_string_lossy().replace('%', "%%")
            )
        })
        .unwrap_or_default();
    let exec_start = program
        .iter()
        .map(|a| systemd_quote(a, true))
        .collect::<Vec<_>>()
        .join(" ");
    let env_lines: String = env
        .iter()
        .map(|(k, v)| {
            format!(
                "Environment={}\n",
                systemd_quote(&format!("{k}={v}"), false)
            )
        })
        .collect();
    format!(
        "[Unit]\nDescription=devy managed {name} service\n\n\
         [Service]\nExecStart={exec_start}\n{env_lines}{working_dir}Restart=on-failure\n\n\
         [Install]\nWantedBy=default.target\n"
    )
}

// ── Service management — service manager commands ─────────────────────────────

/// The service-manager commands behind nix-run services. `id` is a launchd label or a
/// systemd unit name and `path` its unit file. Tests substitute a fake.
trait ServiceControl {
    fn is_running(&self, id: &str) -> Result<bool>;
    /// Loads the unit file at `path` (reloading it when already loaded) and starts `id`.
    fn start(&self, id: &str, path: &Path) -> Result<()>;
    fn stop(&self, id: &str, path: &Path) -> Result<()>;
    /// Stops `id` and unloads or disables it, before its unit file is deleted.
    fn retire(&self, id: &str, path: &Path) -> Result<()>;
    /// Called after unit files were deleted.
    fn files_removed(&self) -> Result<()>;
}

/// How many 100 ms polls `retire` waits for a stopped launchd agent's process to exit.
const RETIRE_WAIT_POLLS: u32 = 50;

/// launchctl's fixed, SIP-protected location: never looked up on PATH, where a project
/// environment could shadow it.
const LAUNCHCTL: &str = "/bin/launchctl";

fn launchctl(args: &[&str]) -> Result<()> {
    let status = Command::new(LAUNCHCTL)
        .args(args)
        .status()
        .context("Failed to run launchctl")?;
    if !status.success() {
        bail!("launchctl {} failed", args.join(" "));
    }
    Ok(())
}

/// Whether launchd knows the agent at all (loaded), running or not.
fn launchd_is_loaded(label: &str) -> bool {
    Command::new(LAUNCHCTL)
        .args(["list", label])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 unit file path"))
}

/// `systemctl` from PATH outside the project or a system directory (see
/// `package_manager::system_tool`), never a project-local shim.
fn systemctl_program() -> Result<std::path::PathBuf> {
    super::require_system_tool("systemctl")
}

fn systemctl_user(args: &[&str]) -> Result<()> {
    let status = Command::new(systemctl_program()?)
        .arg("--user")
        .args(args)
        .status()
        .context("Failed to run systemctl")?;
    if !status.success() {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}

/// The platform's service manager: `launchctl` or `systemctl --user`.
struct SystemControl(UnitKind);

impl ServiceControl for SystemControl {
    fn is_running(&self, id: &str) -> Result<bool> {
        match self.0 {
            UnitKind::Launchd => {
                let out = Command::new(LAUNCHCTL)
                    .args(["list", id])
                    .output()
                    .context("Failed to run launchctl list")?;
                if !out.status.success() {
                    return Ok(false);
                }
                // Output contains "PID" = <num>; key is absent when service is stopped.
                let stdout = String::from_utf8_lossy(&out.stdout);
                Ok(stdout.contains("\"PID\"") || stdout.contains("PID ="))
            }
            UnitKind::Systemd => {
                let status = Command::new(systemctl_program()?)
                    .args(["--user", "is-active", "--quiet", id])
                    .status()
                    .context("Failed to run systemctl is-active")?;
                Ok(status.success())
            }
        }
    }

    fn start(&self, id: &str, path: &Path) -> Result<()> {
        match self.0 {
            UnitKind::Launchd => {
                let path = path_str(path)?;
                if launchd_is_loaded(id) {
                    launchctl(&["unload", path])?;
                }
                launchctl(&["load", path])?;
                launchctl(&["start", id])
            }
            UnitKind::Systemd => {
                systemctl_user(&["daemon-reload"])?;
                systemctl_user(&["start", id])
            }
        }
    }

    fn stop(&self, id: &str, path: &Path) -> Result<()> {
        match self.0 {
            UnitKind::Launchd => {
                let _ = launchctl(&["stop", id]); // best-effort; may not be running
                if path.exists() {
                    launchctl(&["unload", path_str(path)?])?;
                }
                Ok(())
            }
            UnitKind::Systemd => systemctl_user(&["stop", id]),
        }
    }

    fn retire(&self, id: &str, path: &Path) -> Result<()> {
        match self.0 {
            UnitKind::Launchd => {
                let _ = launchctl(&["stop", id]); // best-effort; may not be running
                // The per-project unit starts next on the same port and data directory,
                // so wait (bounded) for the old process to exit before unloading.
                for _ in 0..RETIRE_WAIT_POLLS {
                    if !self.is_running(id)? {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if launchd_is_loaded(id) {
                    launchctl(&["unload", path_str(path)?])?;
                }
                Ok(())
            }
            UnitKind::Systemd => {
                // A failed stop is fine only if the unit isn't running; otherwise deleting
                // its file would orphan a process holding the port and data directory.
                if let Err(e) = systemctl_user(&["stop", id])
                    && self.is_running(id)?
                {
                    return Err(e);
                }
                // devy never enables its units; disabling is best-effort cleanup.
                let _ = systemctl_user(&["disable", id]);
                Ok(())
            }
        }
    }

    fn files_removed(&self) -> Result<()> {
        match self.0 {
            UnitKind::Launchd => Ok(()),
            UnitKind::Systemd => systemctl_user(&["daemon-reload"]),
        }
    }
}

/// Where the nix backend keeps service units and logs, and what runs them.
struct ServiceHost {
    kind: UnitKind,
    /// `None`: the platform's user unit directory.
    unit_dir: Option<PathBuf>,
    /// `None`: devy's private per-user directory.
    log_dir: Option<PathBuf>,
    control: Box<dyn ServiceControl>,
}

impl ServiceHost {
    fn native() -> Self {
        let kind = UnitKind::native();
        Self {
            kind,
            unit_dir: None,
            log_dir: None,
            control: Box::new(SystemControl(kind)),
        }
    }

    /// The directory holding the unit files: the override, else the platform's.
    fn unit_dir(&self) -> Result<PathBuf> {
        match &self.unit_dir {
            Some(dir) => Ok(dir.clone()),
            None => default_unit_dir(),
        }
    }
}

/// `~/Library/LaunchAgents` on macOS, `~/.config/systemd/user` on Linux.
fn default_unit_dir() -> Result<PathBuf> {
    let home = || {
        std::env::var("HOME")
            .context("$HOME not set")
            .map(PathBuf::from)
    };
    #[cfg(target_os = "macos")]
    return Ok(home()?.join("Library").join("LaunchAgents"));
    #[cfg(target_os = "linux")]
    return Ok(home()?.join(".config").join("systemd").join("user"));
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = home;
        bail!("Service management is not supported on this platform with the nix backend")
    }
}

/// Writes a unit or plist readable only by the user: it can hold credentials from
/// devy.yml (MinIO keys, the Meilisearch master key).
fn write_private_unit(path: &Path, contents: &str) -> Result<()> {
    crate::fs_safe::write_atomic(path, contents.as_bytes(), 0o600)
}

impl NixPackageManager {
    /// This project's names for service `name`.
    fn names(&self, name: &str) -> ServiceNames {
        service_names(Some(&self.slug), name)
    }

    fn unit_dir(&self) -> Result<PathBuf> {
        self.services.unit_dir()
    }

    /// The project root as recorded in a unit's `DEVY_PROJECT_ROOT`. A root that isn't
    /// valid UTF-8 can't be recorded faithfully, and a lossy record would make
    /// `devy prune` and the ownership check mistake the unit for another root's, so
    /// services aren't run for it.
    fn project_root_str(&self) -> Result<&str> {
        self.project_root.to_str().ok_or_else(|| {
            anyhow::anyhow!(
                "devy cannot run services for a project whose path is not valid UTF-8: {}",
                self.project_root.display()
            )
        })
    }

    /// The directory holding launchd log files, for reading them. When it exists it must,
    /// like the directory `prepare_unit` writes into, be private to the user (owned by
    /// them, mode 0700); one that isn't is refused rather than read, so another user
    /// can't plant log output. Nothing is created: while it is missing there are no logs,
    /// and `devy logs` checks the directory again whenever it opens a file there.
    fn checked_log_dir(&self) -> Result<PathBuf> {
        let dir = match &self.services.log_dir {
            Some(dir) => dir.clone(),
            None => crate::fs_safe::user_dir_path(),
        };
        crate::fs_safe::existing_private_dir(&dir).context("refusing to read service logs")?;
        Ok(dir)
    }

    /// The unit files that run `name` for this project under an outdated name.
    fn stale_units(&self, name: &str) -> Result<Vec<StaleUnit>> {
        let dir = self.unit_dir()?;
        Ok(find_stale_units(
            self.services.kind,
            &dir,
            &self.slug,
            name,
            &self.project_root,
        ))
    }

    /// Stops, unloads and deletes this project's units of `name` under outdated names.
    fn migrate_stale_units(&self, name: &str) -> Result<()> {
        let stale = self.stale_units(name)?;
        if stale.is_empty() {
            return Ok(());
        }
        let control = &self.services.control;
        for unit in &stale {
            control
                .retire(&unit.id, &unit.path)
                .with_context(|| format!("Failed to stop {}", unit.id))?;
            std::fs::remove_file(&unit.path)
                .with_context(|| format!("Failed to remove {}", unit.path.display()))?;
        }
        control.files_removed()?;
        output::skip(&format!("migrated {name} to a per-project service name"));
        Ok(())
    }

    /// Writes the unit file for `name` and returns its path.
    #[cfg(test)]
    fn write_unit(
        &self,
        name: &str,
        launch: &LaunchSpec,
        exec_dir: &Path,
        profile_bin: &Path,
    ) -> Result<PathBuf> {
        let (path, contents) = self.prepare_unit(name, launch, exec_dir, profile_bin)?;
        write_private_unit(&path, &contents)?;
        Ok(path)
    }

    /// Everything about writing the unit file for `name` that can fail before the write
    /// itself: computes its path and contents, refuses control characters and another
    /// project's unit at that path, and creates the unit directory. Returns the path and
    /// contents for `write_private_unit`.
    fn prepare_unit(
        &self,
        name: &str,
        launch: &LaunchSpec,
        exec_dir: &Path,
        profile_bin: &Path,
    ) -> Result<(PathBuf, String)> {
        let kind = self.services.kind;
        let names = self.names(name);
        let dir = self.unit_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;

        let program = program_arguments(launch, exec_dir);
        let mut env = unit_environment(launch, profile_bin);
        env.retain(|(k, _)| k != PROJECT_ROOT_VAR);
        env.push((
            PROJECT_ROOT_VAR.into(),
            self.project_root_str()?.to_string(),
        ));
        let working_dir = launch.working_dir.as_deref();
        ensure_no_control_chars(&program, &env, working_dir)?;
        let contents = match kind {
            UnitKind::Launchd => {
                // launchd opens the log file as the user; its directory must be private to them.
                let log_dir = match &self.services.log_dir {
                    Some(dir) => dir.clone(),
                    None => crate::fs_safe::user_dir()?,
                };
                let log = log_dir.join(&names.log_file);
                launchagent_plist(&names.label, &program, &env, working_dir, &log)
            }
            UnitKind::Systemd => systemd_unit(name, &program, &env, working_dir),
        };
        let path = dir.join(names.file_name(kind));
        self.ensure_own_unit(&path)?;
        Ok((path, contents))
    }

    /// Refuses to replace or stop the unit file at `path` when it records another
    /// project's root: two roots whose slugs collide would otherwise share a unit.
    fn ensure_own_unit(&self, path: &Path) -> Result<()> {
        if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
            return Ok(());
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        match parse_unit(self.services.kind, &contents).project_root {
            Some(root) if Path::new(&root) != self.project_root => bail!(
                "{} belongs to the project at {root}; devy will not replace or stop it",
                path.display()
            ),
            _ => Ok(()),
        }
    }

    /// Whether this project owns a unit of `name` under its legacy name. Best-effort:
    /// false when the unit directory can't be found.
    fn owns_legacy_unit(&self, name: &str) -> bool {
        let legacy = service_names(None, name);
        let id = legacy.id(self.services.kind);
        self.stale_units(name)
            .is_ok_and(|units| units.iter().any(|u| u.id == id))
    }

    /// Where `devy logs` reads service `name`'s output: the launchd agent's log file or
    /// the systemd unit's user journal. While the per-project source doesn't exist and
    /// this project owns a legacy-named unit, the legacy log file or unit is read
    /// instead. Legacy names were shared by every project, so another project's legacy
    /// logs are never shown. Launchd log files are read only from a private directory
    /// (see [`Self::checked_log_dir`]).
    fn service_log_source(&self, name: &str, lines: u32, follow: bool) -> Result<super::LogSource> {
        let (names, legacy) = (self.names(name), service_names(None, name));
        Ok(match self.services.kind {
            UnitKind::Launchd => {
                let dir = self.checked_log_dir()?;
                let (new, old) = (dir.join(&names.log_file), dir.join(&legacy.log_file));
                let path = if !new.exists() && old.exists() && self.owns_legacy_unit(name) {
                    old
                } else {
                    new
                };
                super::LogSource::Files(vec![path])
            }
            UnitKind::Systemd => {
                let new_exists = self.unit_dir().is_ok_and(|d| d.join(&names.unit).exists());
                let unit = if !new_exists && self.owns_legacy_unit(name) {
                    &legacy.unit
                } else {
                    &names.unit
                };
                super::LogSource::journal(unit, true, lines, follow)
            }
        })
    }
}

/// The environment variable recording a unit's project root.
const PROJECT_ROOT_VAR: &str = "DEVY_PROJECT_ROOT";

// ── Service management — pruning removed checkouts ────────────────────────────

/// Finds and removes the units of removed checkouts in the user's unit directory, for
/// `devy prune`. Needs no project: each unit records its own root.
pub(crate) struct UnitPruner {
    host: ServiceHost,
}

// Windows compiles this module only for tests, which don't use the native pruner.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
impl UnitPruner {
    /// The user's launchd agents or systemd user units, and the platform's service manager.
    pub(crate) fn native() -> Self {
        Self {
            host: ServiceHost::native(),
        }
    }

    /// What `devy prune` calls these units.
    pub(crate) fn kind_name(&self) -> &'static str {
        match self.host.kind {
            UnitKind::Launchd => "launchd agent",
            UnitKind::Systemd => "systemd unit",
        }
    }

    /// The units whose recorded project root is a removed checkout.
    pub(crate) fn find(&self) -> Result<Vec<OrphanedUnit>> {
        Ok(find_orphaned_units(self.host.kind, &self.host.unit_dir()?))
    }

    /// Stops `unit`, unloads or disables it, and deletes its file. Fails, leaving it in
    /// place, when the file no longer records the same removed checkout: it may have
    /// changed since it was listed. Call `finish` after the last removal.
    pub(crate) fn remove(&self, unit: &OrphanedUnit) -> Result<()> {
        if orphaned_root(self.host.kind, &unit.id, &unit.path).as_deref()
            != Some(unit.root.as_str())
        {
            bail!("{} changed since it was listed; left it in place", unit.id);
        }
        self.host
            .control
            .retire(&unit.id, &unit.path)
            .with_context(|| format!("Failed to stop {}", unit.id))?;
        std::fs::remove_file(&unit.path)
            .with_context(|| format!("Failed to remove {}", unit.path.display()))
    }

    /// Tells the service manager unit files were deleted (`systemctl --user daemon-reload`).
    pub(crate) fn finish(&self) -> Result<()> {
        self.host.control.files_removed()
    }
}

/// A service manager with nothing running that refuses to change anything.
#[cfg(test)]
struct InertControl;

#[cfg(test)]
impl ServiceControl for InertControl {
    fn is_running(&self, _: &str) -> Result<bool> {
        Ok(false)
    }
    fn start(&self, id: &str, _: &Path) -> Result<()> {
        bail!("test service manager: start {id}")
    }
    fn stop(&self, id: &str, _: &Path) -> Result<()> {
        bail!("test service manager: stop {id}")
    }
    fn retire(&self, id: &str, _: &Path) -> Result<()> {
        bail!("test service manager: retire {id}")
    }
    fn files_removed(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
impl NixPackageManager {
    /// A backend for tests elsewhere in the crate: units in `unit_dir` (launchd agents
    /// when `launchd`, else systemd units), logs in `log_dir`, and a service manager
    /// with nothing running.
    pub(crate) fn with_test_dirs(
        project_root: &Path,
        project_name: &str,
        launchd: bool,
        unit_dir: &Path,
        log_dir: &Path,
    ) -> Self {
        let kind = if launchd {
            UnitKind::Launchd
        } else {
            UnitKind::Systemd
        };
        let services = ServiceHost {
            kind,
            unit_dir: Some(unit_dir.to_path_buf()),
            log_dir: Some(log_dir.to_path_buf()),
            control: Box::new(InertControl),
        };
        Self::with_services(project_root, project_name, services)
    }

    /// This project's systemd unit and log file name for service `name`.
    pub(crate) fn unit_and_log_names(&self, name: &str) -> (String, String) {
        let n = self.names(name);
        (n.unit, n.log_file)
    }
}

// ── PackageManager impl ───────────────────────────────────────────────────────

impl NixPackageManager {
    /// `attr`'s own `bin/` in the project profile. `None` for the legacy nix-env style
    /// or when it can't be determined; callers fall back to the merged profile `bin/`.
    fn package_bin(&self, attr: &str) -> Option<PathBuf> {
        if !self.profile_path.exists() || self.effective_style() != NixStyle::Profile {
            return None;
        }
        let json = profile_list_json(&self.nix_bin(), &self.profile_path).ok()?;
        profile_package_bins(&json, attr)
            .into_iter()
            .find(|bin| bin.is_dir())
    }

    /// `Some` when `dep` is installed in the project profile, holding its version when
    /// one is known.
    fn query_version(&self, dep: &Dependency) -> Result<Option<Option<String>>> {
        // Profile symlink doesn't exist yet → nothing installed in it. Guard here
        // to prevent Nix from falling back to the user's global profile, which would
        // cause packages installed globally (but not in this project) to appear installed.
        if !self.profile_path.exists() {
            return Ok(None);
        }
        match self.effective_style() {
            NixStyle::Profile => {
                let json = profile_list_json(&self.nix_bin(), &self.profile_path)?;
                Ok(profile_find_pkg(&json, &dep.name))
            }
            NixStyle::Env => {
                let json = env_list_json(&self.nix_env_bin(), &self.profile_path)?;
                let manifest = read_env_manifest(&self.env_manifest_path());
                Ok(env_find_attr(&json, &manifest, &dep.name))
            }
        }
    }

    /// The derivation name `nix-env -iA nixpkgs.<attr>` will install, or `None` when the
    /// query fails (the install then reports the real error).
    fn env_query_drv_name(&self, attr: &str, env: &[(&str, &str)]) -> Option<String> {
        let out = Command::new(self.nix_env_bin())
            .args(env_query_name_args(attr))
            .envs(env.iter().copied())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        env_drv_name(&serde_json::from_slice(&out.stdout).ok()?)
    }
}

/// Arguments for the Determinate Systems installer: a non-interactive install.
fn bootstrap_args() -> Vec<std::ffi::OsString> {
    ["install", "--no-confirm"].map(Into::into).to_vec()
}

impl PackageManager for NixPackageManager {
    fn name(&self) -> &str {
        "nix"
    }

    fn install_url(&self) -> &str {
        "https://nixos.org/download/"
    }

    fn is_available(&self) -> bool {
        self.nix_bin().exists()
    }

    fn bootstrap(&self) -> Result<()> {
        output::step(&format!(
            "Installing Nix via the Determinate Installer ({}), verified against its pinned SHA-256",
            installers::NIX.url
        ));
        let status = installers::run_script(
            &installers::NIX,
            installers::Interpreter::Sh,
            &bootstrap_args(),
            &[],
        )?;
        if !status.success() {
            bail!("Nix installation failed");
        }
        Ok(())
    }

    fn is_package_installed(&self, dep: &Dependency) -> Result<bool> {
        self.query_version(dep).map(|v| v.is_some())
    }

    fn install_package(&self, dep: &Dependency) -> Result<()> {
        // nix requires the profile's parent directory to exist before it can
        // create the profile symlink (.devy/nix-profile → nix store path). The
        // env-style manifest (`.devy/nix-env-attrs.json`) is written beside it later.
        crate::fs_safe::ensure_devy_dir(&self.project_root)?;
        let style = self.effective_style();
        let cmd = install_cmd(
            style,
            &self.profile_path,
            &dep.name,
            dep.allow_unfree,
            dep.allow_insecure,
        );
        // The step line omits `--profile <path>`, which is the same for every install.
        let (bin, shown) = match style {
            NixStyle::Profile => (
                self.nix_bin(),
                format!("nix profile install {}", cmd.args[4..].join(" ")),
            ),
            NixStyle::Env => (
                self.nix_env_bin(),
                format!("nix-env {}", cmd.args[2..].join(" ")),
            ),
        };
        if dep.allow_unfree {
            output::info(&format!(
                "{0}: nixpkgs#{0} is unfree — allowing unfree packages for this install",
                dep.name
            ));
        }
        if dep.allow_insecure {
            output::warn(&format!(
                "{0}: nixpkgs#{0} is marked insecure by nixpkgs — allowing insecure packages for this install",
                dep.name
            ));
        }
        let install = || {
            output::step(&shown);
            let status = Command::new(bin)
                .args(&cmd.args)
                .envs(cmd.env.iter().copied())
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .with_context(|| format!("Failed to run: {shown}"))?;
            if !status.success() {
                bail!("`{shown}` failed — check output above");
            }
            Ok(())
        };
        match style {
            NixStyle::Profile => install(),
            NixStyle::Env => {
                let name = self.env_query_drv_name(&dep.name, &cmd.env);
                env_install_and_record(&self.env_manifest_path(), &dep.name, name, install)
            }
        }
    }

    /// Read-only: when the service doesn't run under its per-project name, one of this
    /// project's units under an outdated name may still run it.
    fn is_service_running(&self, name: &str) -> Result<bool> {
        let control = &self.services.control;
        if control.is_running(self.names(name).id(self.services.kind))? {
            return Ok(true);
        }
        // Best-effort: status must not fail where the unit dir can't be found.
        for unit in self.stale_units(name).unwrap_or_default() {
            if control.is_running(&unit.id)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn start_service(&self, name: &str, launch: Option<&LaunchSpec>) -> Result<()> {
        let launch = launch.ok_or_else(|| unsupported_service(name))?;
        self.project_root_str()?;
        // The unit runs binaries from the profile: it must be a real link into the store,
        // and the unit names the verified store path, so a link swapped later (e.g. by a
        // `git pull`) isn't followed on the next restart.
        let verified = crate::fs_safe::verified_nix_profile(&self.project_root)?;
        let profile_bin = verified.map_or_else(|| self.profile_bin(), |p| p.join("bin"));
        let package_bin = launch
            .exec_package
            .as_deref()
            .and_then(|attr| self.package_bin(attr));
        let exec_dir = package_bin.clone().unwrap_or_else(|| profile_bin.clone());
        let package_root = package_bin.as_deref().and_then(Path::parent);
        let needs_root = !launch.seed_dirs.is_empty()
            || !launch.conditional_args.is_empty()
            || !launch.package_env.is_empty();
        if needs_root && package_root.is_none() && self.effective_style() == NixStyle::Env {
            bail!(
                "Failed to prepare {name} config: devy can only locate the package's files \
                 with flakes-era Nix (`nix profile`), not legacy `nix-env`"
            );
        }
        let launch = &prepare_package_files(name, launch, package_root)?;
        run_init(name, launch, &exec_dir, &profile_bin)?;

        // Always rewrite and reload so port and config changes take effect.
        let (path, contents) = self.prepare_unit(name, launch, &exec_dir, &profile_bin)?;
        // Retire units under outdated names only once every check before the write has
        // passed, so a start that fails early leaves a running legacy unit running.
        self.migrate_stale_units(name)?;
        write_private_unit(&path, &contents)?;
        let id = self.names(name);
        self.services
            .control
            .start(id.id(self.services.kind), &path)
    }

    fn stop_service(&self, name: &str) -> Result<()> {
        self.migrate_stale_units(name)?;
        let kind = self.services.kind;
        let names = self.names(name);
        let path = self.unit_dir()?.join(names.file_name(kind));
        if !path.exists() {
            // Usually never written under this name (only a legacy unit ran, and it was
            // just migrated). A best-effort stop still covers a loaded unit whose file
            // was deleted; with no unit to stop it fails, which is fine.
            let _ = self.services.control.stop(names.id(kind), &path);
            return Ok(());
        }
        self.ensure_own_unit(&path)?;
        self.services.control.stop(names.id(kind), &path)
    }

    fn migrate_service(&self, name: &str) -> Result<()> {
        self.migrate_stale_units(name)
    }

    fn needs_service_migration(&self, name: &str) -> bool {
        self.stale_units(name).is_ok_and(|units| !units.is_empty())
    }

    fn uses_legacy_service_name(&self, name: &str) -> bool {
        self.owns_legacy_unit(name)
    }

    fn resolved_version(&self, dep: &Dependency) -> Result<Option<String>> {
        self.query_version(dep).map(Option::flatten)
    }

    fn log_source(&self, name: &str, lines: u32, follow: bool) -> Result<super::LogSource> {
        self.service_log_source(name, lines, follow)
    }

    /// Advertises the project-local Nix profile bin dir so shadowenv adds it to PATH.
    /// This ensures `devy up` activates the project's Nix packages without touching
    /// the user's global profile or requiring a manual PATH change.
    fn path_prepends(&self, _project_root: &Path) -> Vec<String> {
        // A `.devy/nix-profile` that isn't a link into /nix/store never goes on PATH.
        if let Err(e) = crate::fs_safe::verified_nix_profile(&self.project_root) {
            output::warn(&format!("{e:#}"));
            return Vec::new();
        }
        vec![self.profile_bin().to_string_lossy().into_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::{LogCommandKind, LogSource};

    // ── service names, stale units and migration ──────────────────────────────

    #[derive(Default)]
    struct FakeState {
        running: std::collections::BTreeSet<String>,
        calls: Vec<String>,
    }

    /// Records service-manager calls and tracks which ids are running.
    #[derive(Clone, Default)]
    struct FakeControl(std::rc::Rc<std::cell::RefCell<FakeState>>);

    impl FakeControl {
        fn set_running(&self, id: &str) {
            self.0.borrow_mut().running.insert(id.to_string());
        }
        fn calls(&self) -> Vec<String> {
            self.0.borrow().calls.clone()
        }
        fn record(&self, call: String) {
            self.0.borrow_mut().calls.push(call);
        }
    }

    impl ServiceControl for FakeControl {
        fn is_running(&self, id: &str) -> Result<bool> {
            Ok(self.0.borrow().running.contains(id))
        }
        fn start(&self, id: &str, _: &Path) -> Result<()> {
            self.record(format!("start {id}"));
            self.set_running(id);
            Ok(())
        }
        fn stop(&self, id: &str, _: &Path) -> Result<()> {
            self.record(format!("stop {id}"));
            self.0.borrow_mut().running.remove(id);
            Ok(())
        }
        fn retire(&self, id: &str, _: &Path) -> Result<()> {
            self.record(format!("retire {id}"));
            self.0.borrow_mut().running.remove(id);
            Ok(())
        }
        fn files_removed(&self) -> Result<()> {
            self.record("files_removed".into());
            Ok(())
        }
    }

    /// Temp unit and log directories and a fake service manager.
    struct Host {
        units: crate::test_support::TempDir,
        logs: crate::test_support::TempDir,
        control: FakeControl,
    }

    fn host() -> Host {
        Host {
            units: crate::test_support::tmp_dir(),
            logs: crate::test_support::private_tmp_dir(),
            control: FakeControl::default(),
        }
    }

    impl Host {
        fn pm(&self, kind: UnitKind, root: &Path, name: &str) -> NixPackageManager {
            NixPackageManager::with_services(
                root,
                name,
                ServiceHost {
                    kind,
                    unit_dir: Some(self.units.to_path_buf()),
                    log_dir: Some(self.logs.to_path_buf()),
                    control: Box::new(self.control.clone()),
                },
            )
        }

        /// Writes a unit file for `id` (a label or unit name) that runs redis with its
        /// data in `data_dir`, recording `project_root` when given (legacy units don't).
        fn write(
            &self,
            kind: UnitKind,
            id: &str,
            data_dir: &Path,
            project_root: Option<&Path>,
        ) -> PathBuf {
            let program = vec![
                "/nix/store/x-redis/bin/redis-server".to_string(),
                "--dir".to_string(),
                data_dir.to_string_lossy().into_owned(),
            ];
            let env: Vec<(String, String)> = project_root
                .map(|r| {
                    (
                        PROJECT_ROOT_VAR.to_string(),
                        r.to_string_lossy().into_owned(),
                    )
                })
                .into_iter()
                .collect();
            let (file, contents) = match kind {
                UnitKind::Launchd => (
                    format!("{id}.plist"),
                    launchagent_plist(id, &program, &env, Some(data_dir), Path::new("/l.log")),
                ),
                UnitKind::Systemd => (
                    id.to_string(),
                    systemd_unit("redis", &program, &env, Some(data_dir)),
                ),
            };
            let path = self.units.join(file);
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    const KINDS: [UnitKind; 2] = [UnitKind::Launchd, UnitKind::Systemd];

    fn slug(name: &str, root: &str) -> String {
        crate::modules::helpers::project_slug(name, Path::new(root))
    }

    fn data(root: &Path) -> PathBuf {
        crate::modules::nix_data_dir(root, "redis")
    }

    #[test]
    fn service_names_include_the_project_slug() {
        let h = host();
        let pm = h.pm(UnitKind::native(), Path::new("/src/app"), "app");
        let slug = slug("app", "/src/app");
        let (prefix, hash) = slug.rsplit_once('-').unwrap();
        assert_eq!(prefix, "app");
        assert_eq!(hash.len(), 8);
        let names = pm.names("redis");
        assert_eq!(names.label, format!("sh.devy.app-{hash}.redis"));
        assert_eq!(names.unit, format!("devy-app-{hash}-redis.service"));
        assert_eq!(names.log_file, format!("devy-app-{hash}-redis.log"));
        assert_eq!(names.id(UnitKind::Launchd), names.label);
        assert_eq!(names.id(UnitKind::Systemd), names.unit);
        assert_eq!(
            names.file_name(UnitKind::Launchd),
            format!("sh.devy.app-{hash}.redis.plist")
        );
    }

    #[test]
    fn legacy_service_names_have_no_project() {
        let names = service_names(None, "redis");
        assert_eq!(names.label, "sh.devy.redis");
        assert_eq!(names.unit, "devy-redis.service");
        assert_eq!(names.log_file, "redis.log");
        assert_eq!(names.file_name(UnitKind::Launchd), "sh.devy.redis.plist");
    }

    #[test]
    fn slug_in_file_name_matches_only_this_service() {
        let l = UnitKind::Launchd;
        let s = UnitKind::Systemd;
        let name = "redis";
        assert_eq!(
            slug_in_file_name(l, "sh.devy.app-1a2b3c4d.redis.plist", name),
            Some("app-1a2b3c4d")
        );
        assert_eq!(slug_in_file_name(l, "sh.devy.redis.plist", name), None);
        assert_eq!(
            slug_in_file_name(l, "sh.devy.app-1a2b3c4d.postgresql.plist", name),
            None
        );
        assert_eq!(
            slug_in_file_name(s, "devy-app-1a2b3c4d-redis.service", name),
            Some("app-1a2b3c4d")
        );
        assert_eq!(slug_in_file_name(s, "devy-redis.service", name), None);
        assert_eq!(
            slug_in_file_name(s, "devy-app-1a2b3c4d-xredis.service", name),
            None
        );
    }

    #[test]
    fn points_into_project_matches_whole_path_components() {
        let root = Path::new("/src/app");
        let yes = |v: &str| points_into_project(&[v.to_string()], root);
        assert!(yes("/src/app/.devy/data/redis"));
        assert!(yes("/src/app/.devy/nix-profile/bin/redis-server"));
        assert!(yes("--datadir=/src/app/.devy/data/mysql"));
        assert!(!yes("/src/app2/.devy/data/redis"));
        assert!(!yes("/src/app/.devy/database"));
        assert!(!yes("/src/app"));
        assert!(!yes("--port"));
    }

    #[test]
    fn systemd_words_undo_systemd_quote() {
        let words = ["/a b/\"q\"\\x%i$HOME", "p\"w 100%$", ""];
        for exec in [true, false] {
            let line = words
                .iter()
                .map(|w| systemd_quote(w, exec))
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(systemd_words(&line, exec), words);
        }
        assert_eq!(systemd_words("/bin/x  -p 1", true), ["/bin/x", "-p", "1"]);
    }

    #[test]
    fn parse_plist_ignores_env_vars_named_like_top_level_keys() {
        let plist = launchagent_plist(
            "sh.devy.redis",
            &["/bin/redis".into(), "/a & <b>".into()],
            &[
                ("WorkingDirectory".into(), "/src/app/.devy/data/x".into()),
                (PROJECT_ROOT_VAR.into(), "/src/a&b".into()),
            ],
            Some(Path::new("/w")),
            Path::new("/l.log"),
        );
        let info = parse_plist(&plist);
        assert_eq!(info.paths, ["/bin/redis", "/a & <b>", "/w"]);
        assert_eq!(info.project_root.as_deref(), Some("/src/a&b"));
    }

    #[test]
    fn find_stale_units_finds_an_owned_legacy_unit() {
        for kind in KINDS {
            let h = host();
            let root = Path::new("/src/app");
            let id = service_names(None, "redis").id(kind).to_string();
            let path = h.write(kind, &id, &data(root), None);
            let found = find_stale_units(kind, &h.units, &slug("app", "/src/app"), "redis", root);
            assert_eq!(found, [StaleUnit { id, path }], "{kind:?}");
        }
    }

    #[test]
    fn find_stale_units_ignores_another_projects_legacy_unit() {
        for kind in KINDS {
            let h = host();
            let id = service_names(None, "redis").id(kind).to_string();
            h.write(kind, &id, &data(Path::new("/src/app2")), None);
            h.write(kind, &id, &data(Path::new("/src/other")), None);
            let root = Path::new("/src/app");
            let found = find_stale_units(kind, &h.units, &slug("app", "/src/app"), "redis", root);
            assert!(found.is_empty(), "{kind:?}: {found:?}");
        }
    }

    #[test]
    fn find_stale_units_finds_a_renamed_slug_unit_of_this_root() {
        for kind in KINDS {
            let h = host();
            let root = Path::new("/src/app");
            let old = service_names(Some(&slug("app", "/src/app")), "redis");
            let path = h.write(kind, old.id(kind), &data(root), Some(root));
            // Same old project name, but another root: not ours.
            let other = service_names(Some(&slug("app", "/src/app-feat")), "redis");
            let feat = Path::new("/src/app-feat");
            h.write(kind, other.id(kind), &data(feat), Some(feat));
            // This project's current unit is never stale.
            let current = service_names(Some(&slug("shop", "/src/app")), "redis");
            h.write(kind, current.id(kind), &data(root), Some(root));
            let found = find_stale_units(kind, &h.units, &slug("shop", "/src/app"), "redis", root);
            assert_eq!(
                found,
                [StaleUnit {
                    id: old.id(kind).to_string(),
                    path
                }],
                "{kind:?}"
            );
        }
    }

    #[test]
    fn find_stale_units_ignores_other_services_and_missing_dirs() {
        let root = Path::new("/src/app");
        let h = host();
        h.write(
            UnitKind::Launchd,
            "sh.devy.postgresql",
            &crate::modules::nix_data_dir(root, "postgresql"),
            None,
        );
        let slug = slug("app", "/src/app");
        assert!(find_stale_units(UnitKind::Launchd, &h.units, &slug, "redis", root).is_empty());
        let missing = h.units.join("missing");
        assert!(find_stale_units(UnitKind::Launchd, &missing, &slug, "redis", root).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn find_stale_units_skips_symlinks() {
        let h = host();
        let root = Path::new("/src/app");
        let target = h.write(UnitKind::Launchd, "elsewhere", &data(root), None);
        std::os::unix::fs::symlink(&target, h.units.join("sh.devy.redis.plist")).unwrap();
        let slug = slug("app", "/src/app");
        assert!(find_stale_units(UnitKind::Launchd, &h.units, &slug, "redis", root).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn start_migrates_an_owned_legacy_unit_and_leaves_others() {
        for kind in KINDS {
            let h = host();
            let root = crate::test_support::tmp_dir();
            let pm = h.pm(kind, &root, "app");
            let legacy = service_names(None, "redis");
            let owned = h.write(kind, legacy.id(kind), &data(&root), None);
            h.control.set_running(legacy.id(kind));
            pm.start_service("redis", Some(&redis_launch())).unwrap();
            assert!(!owned.exists(), "{kind:?}: legacy unit must be removed");
            let new = pm.names("redis");
            assert!(h.units.join(new.file_name(kind)).is_file());
            assert_eq!(
                h.control.calls(),
                [
                    format!("retire {}", legacy.id(kind)),
                    "files_removed".into(),
                    format!("start {}", new.id(kind)),
                ],
                "{kind:?}"
            );

            // Another project's legacy unit is never touched.
            let h = host();
            let pm = h.pm(kind, &root, "app");
            let foreign = h.write(kind, legacy.id(kind), &data(Path::new("/src/other")), None);
            h.control.set_running(legacy.id(kind));
            pm.start_service("redis", Some(&redis_launch())).unwrap();
            assert!(foreign.is_file(), "{kind:?}");
            assert_eq!(h.control.calls(), [format!("start {}", new.id(kind))]);
            assert!(pm.is_service_running("redis").unwrap());
        }
    }

    #[test]
    fn stop_migrates_an_owned_legacy_unit() {
        for kind in KINDS {
            let h = host();
            let root = Path::new("/src/app");
            let pm = h.pm(kind, root, "app");
            let legacy = service_names(None, "redis");
            let owned = h.write(kind, legacy.id(kind), &data(root), None);
            h.control.set_running(legacy.id(kind));
            // As `devy stop` runs it: the running legacy unit counts as running, and the
            // migration is what stops it; the never-written per-project unit only gets a
            // best-effort stop.
            crate::commands::service::stop_impl(
                &crate::config::Dependency::simple("redis"),
                &crate::service_runner::PackageRunner::new(&pm, root),
            )
            .unwrap();
            assert!(!owned.exists(), "{kind:?}");
            assert!(!pm.is_service_running("redis").unwrap());
            assert_eq!(
                h.control.calls(),
                [
                    format!("retire {}", legacy.id(kind)),
                    "files_removed".into(),
                    format!("stop {}", pm.names("redis").id(kind)),
                ]
            );
            // Once the per-project unit exists, stop stops it.
            h.write(kind, pm.names("redis").id(kind), &data(root), Some(root));
            pm.stop_service("redis").unwrap();
            assert_eq!(
                h.control.calls().last().unwrap(),
                &format!("stop {}", pm.names("redis").id(kind))
            );
        }
    }

    #[test]
    fn running_check_falls_back_to_an_owned_legacy_unit_without_removing_it() {
        for kind in KINDS {
            let h = host();
            let root = Path::new("/src/app");
            let pm = h.pm(kind, root, "app");
            let legacy = service_names(None, "redis");
            let owned = h.write(kind, legacy.id(kind), &data(root), None);
            assert!(!pm.is_service_running("redis").unwrap(), "{kind:?}");
            h.control.set_running(legacy.id(kind));
            assert!(pm.is_service_running("redis").unwrap(), "{kind:?}");
            assert!(pm.uses_legacy_service_name("redis"));
            assert!(owned.is_file(), "{kind:?}: status must not migrate");
            assert!(h.control.calls().is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn running_check_ignores_another_projects_legacy_unit() {
        let h = host();
        let kind = UnitKind::Launchd;
        let pm = h.pm(kind, Path::new("/src/app"), "app");
        h.write(kind, "sh.devy.redis", &data(Path::new("/src/other")), None);
        h.control.set_running("sh.devy.redis");
        assert!(!pm.is_service_running("redis").unwrap());
        assert!(!pm.uses_legacy_service_name("redis"));
        pm.migrate_service("redis").unwrap();
        assert!(h.units.join("sh.devy.redis.plist").is_file());
        assert!(h.control.calls().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn failed_start_leaves_a_running_legacy_unit_in_place() {
        for kind in KINDS {
            let h = host();
            let root = crate::test_support::tmp_dir();
            let pm = h.pm(kind, &root, "app");
            let legacy = service_names(None, "redis");
            let owned = h.write(kind, legacy.id(kind), &data(&root), None);
            h.control.set_running(legacy.id(kind));
            // A profile that isn't a link into the store fails the start before any unit
            // is written.
            std::fs::create_dir_all(root.join(".devy")).unwrap();
            std::fs::write(root.join(crate::fs_safe::NIX_PROFILE), "").unwrap();
            pm.start_service("redis", Some(&redis_launch()))
                .unwrap_err();
            assert!(owned.is_file(), "{kind:?}: legacy unit must survive");
            assert!(
                h.control.calls().is_empty(),
                "{kind:?}: {:?}",
                h.control.calls()
            );
            assert!(pm.is_service_running("redis").unwrap());
            assert!(!h.units.join(pm.names("redis").file_name(kind)).exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn running_legacy_unit_needs_a_start_that_replaces_it() {
        for kind in KINDS {
            let h = host();
            let root = crate::test_support::tmp_dir();
            let pm = h.pm(kind, &root, "app");
            let legacy = service_names(None, "redis");
            h.write(kind, legacy.id(kind), &data(&root), None);
            h.control.set_running(legacy.id(kind));
            // What `devy up` and `devy start` check before deciding to start.
            let runner = crate::service_runner::PackageRunner::new(&pm, &root);
            let dep = crate::config::Dependency::simple("redis");
            assert!(crate::service_runner::ServiceRunner::is_running(&runner, &dep).unwrap());
            assert!(crate::service_runner::ServiceRunner::needs_migration(
                &runner, &dep
            ));
            pm.start_service("redis", Some(&redis_launch())).unwrap();
            assert!(!crate::service_runner::ServiceRunner::needs_migration(
                &runner, &dep
            ));
            assert!(pm.is_service_running("redis").unwrap());
        }
    }

    #[test]
    fn renamed_slug_unit_needs_migration_but_is_not_a_legacy_name() {
        for kind in KINDS {
            let h = host();
            let root = Path::new("/src/app");
            let pm = h.pm(kind, root, "shop");
            let old = service_names(Some(&slug("app", "/src/app")), "redis");
            h.write(kind, old.id(kind), &data(root), Some(root));
            assert!(pm.needs_service_migration("redis"), "{kind:?}");
            assert!(!pm.uses_legacy_service_name("redis"), "{kind:?}");

            let legacy = service_names(None, "redis");
            h.write(kind, legacy.id(kind), &data(root), None);
            assert!(pm.uses_legacy_service_name("redis"), "{kind:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_project_root_is_refused_before_migrating() {
        use std::os::unix::ffi::OsStrExt;
        let base = crate::test_support::tmp_dir();
        let root = base.join(std::ffi::OsStr::from_bytes(b"app-\xff"));
        // Best-effort: macOS (APFS) refuses non-UTF-8 names. The refusal needs no files.
        let _ = std::fs::create_dir(&root);
        for kind in KINDS {
            let h = host();
            let pm = h.pm(kind, &root, "app");
            let legacy = service_names(None, "redis");
            let owned = h.write(kind, legacy.id(kind), &data(&root), None);
            h.control.set_running(legacy.id(kind));
            let err = pm
                .start_service("redis", Some(&redis_launch()))
                .unwrap_err();
            assert!(format!("{err:#}").contains("not valid UTF-8"), "{err:#}");
            let err = pm
                .write_unit(
                    "redis",
                    &redis_launch(),
                    Path::new("/p/bin"),
                    Path::new("/p/bin"),
                )
                .unwrap_err();
            assert!(format!("{err:#}").contains("not valid UTF-8"), "{err:#}");
            assert!(owned.is_file(), "{kind:?}");
            assert!(h.control.calls().is_empty(), "{kind:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_project_root_has_no_stale_units() {
        use std::os::unix::ffi::OsStrExt;
        // No files under the root are needed: matching is by recorded text.
        let root = Path::new("/src").join(std::ffi::OsStr::from_bytes(b"app-\xff"));
        let lossy = PathBuf::from(root.to_string_lossy().into_owned());
        let current = crate::modules::helpers::project_slug("shop", &root);
        let old = crate::modules::helpers::project_slug("app", &root);
        for kind in KINDS {
            let h = host();
            // Units recording the lossy form of the root, as another project at that
            // (valid UTF-8) path would: a lossy comparison would claim them.
            h.write(
                kind,
                service_names(Some(&old), "redis").id(kind),
                &data(&lossy),
                Some(&lossy),
            );
            h.write(
                kind,
                service_names(None, "redis").id(kind),
                &data(&lossy),
                None,
            );
            let found = find_stale_units(kind, &h.units, &current, "redis", &root);
            assert!(found.is_empty(), "{kind:?}: {found:?}");
            let pm = h.pm(kind, &root, "shop");
            assert!(!pm.needs_service_migration("redis"), "{kind:?}");
            assert!(!pm.uses_legacy_service_name("redis"), "{kind:?}");
            pm.migrate_service("redis").unwrap();
            assert!(h.control.calls().is_empty(), "{kind:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn start_failing_before_the_unit_is_written_leaves_a_running_legacy_unit() {
        for kind in KINDS {
            // A control character in the launch spec.
            let h = host();
            let root = crate::test_support::tmp_dir();
            let pm = h.pm(kind, &root, "app");
            let legacy = service_names(None, "redis");
            let owned = h.write(kind, legacy.id(kind), &data(&root), None);
            h.control.set_running(legacy.id(kind));
            let mut launch = redis_launch();
            launch.args.push("bad\nvalue".into());
            let err = pm.start_service("redis", Some(&launch)).unwrap_err();
            assert!(
                format!("{err:#}").contains("control character"),
                "{kind:?}: {err:#}"
            );
            assert!(owned.is_file(), "{kind:?}: legacy unit must survive");
            assert!(
                h.control.calls().is_empty(),
                "{kind:?}: {:?}",
                h.control.calls()
            );
            assert!(!h.units.join(pm.names("redis").file_name(kind)).exists());

            // Another root's unit already at this project's per-project name.
            let h = host();
            let pm = h.pm(kind, &root, "app");
            let owned = h.write(kind, legacy.id(kind), &data(&root), None);
            h.control.set_running(legacy.id(kind));
            let other = Path::new("/src/elsewhere");
            let colliding = h.write(kind, pm.names("redis").id(kind), &data(other), Some(other));
            let before = std::fs::read_to_string(&colliding).unwrap();
            let err = pm
                .start_service("redis", Some(&redis_launch()))
                .unwrap_err();
            assert!(
                format!("{err:#}").contains("will not replace or stop it"),
                "{kind:?}: {err:#}"
            );
            assert!(owned.is_file(), "{kind:?}: legacy unit must survive");
            assert_eq!(std::fs::read_to_string(&colliding).unwrap(), before);
            assert!(
                h.control.calls().is_empty(),
                "{kind:?}: {:?}",
                h.control.calls()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn units_record_the_project_root() {
        let base = crate::test_support::tmp_dir();
        let root = base.join("my app 100%");
        std::fs::create_dir(&root).unwrap();
        for kind in KINDS {
            let h = host();
            let pm = h.pm(kind, &root, "app");
            let path = pm
                .write_unit(
                    "redis",
                    &redis_launch(),
                    Path::new("/p/bin"),
                    Path::new("/p/bin"),
                )
                .unwrap();
            let contents = std::fs::read_to_string(&path).unwrap();
            let root_s = root.to_string_lossy();
            match kind {
                UnitKind::Launchd => {
                    assert!(
                        contents.contains(&format!(
                            "<key>DEVY_PROJECT_ROOT</key>\n        <string>{root_s}</string>"
                        )),
                        "{contents}"
                    );
                    assert!(contents.contains(&format!(
                        "<string>{}</string>",
                        h.logs.join(pm.names("redis").log_file).display()
                    )));
                }
                UnitKind::Systemd => assert!(
                    contents.contains(&format!(
                        "Environment=\"DEVY_PROJECT_ROOT={}\"\n",
                        root_s.replace('%', "%%")
                    )),
                    "{contents}"
                ),
            }
            let info = parse_unit(kind, &contents);
            assert_eq!(info.project_root.as_deref(), Some(root_s.as_ref()));
        }
    }

    #[test]
    fn unit_project_root_overrides_a_launch_env_entry() {
        let h = host();
        let root = crate::test_support::tmp_dir();
        let pm = h.pm(UnitKind::Systemd, &root, "app");
        let mut launch = redis_launch();
        launch
            .env
            .push((PROJECT_ROOT_VAR.into(), "/elsewhere".into()));
        let path = pm
            .write_unit("redis", &launch, Path::new("/p/bin"), Path::new("/p/bin"))
            .unwrap();
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(!contents.contains("/elsewhere"), "{contents}");
        assert_eq!(contents.matches("DEVY_PROJECT_ROOT=").count(), 1);
    }

    // ── log sources ───────────────────────────────────────────────────────────

    #[test]
    fn launchd_logs_are_the_per_project_log_file() {
        let h = host();
        let pm = h.pm(UnitKind::Launchd, Path::new("/src/app"), "app");
        let expected = h.logs.join(pm.names("redis").log_file);
        assert_eq!(
            pm.log_source("redis", 100, false).unwrap(),
            LogSource::Files(vec![expected.clone()])
        );
        // Both exist: the per-project log wins.
        std::fs::write(&expected, "new\n").unwrap();
        std::fs::write(h.logs.join("redis.log"), "old\n").unwrap();
        assert_eq!(
            pm.log_source("redis", 100, false).unwrap(),
            LogSource::Files(vec![expected])
        );
    }

    /// A log directory that isn't private to the user is refused rather than read.
    #[cfg(unix)]
    #[test]
    fn launchd_logs_refuse_a_log_dir_that_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let h = host();
        let pm = h.pm(UnitKind::Launchd, Path::new("/src/app"), "app");
        std::fs::write(h.logs.join(pm.names("redis").log_file), "planted\n").unwrap();
        // Owned by another user.
        let err = crate::fs_safe::with_fake_owner(crate::fs_safe::current_uid() + 1, || {
            pm.log_source("redis", 100, false)
        })
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.starts_with("refusing to read service logs"), "{msg}");
        assert!(msg.contains("not owned by the current user"), "{msg}");
        // Group- or world-writable (or readable).
        for mode in [0o777, 0o770, 0o750] {
            std::fs::set_permissions(&h.logs, std::fs::Permissions::from_mode(mode)).unwrap();
            let msg = format!("{:#}", pm.log_source("redis", 100, false).unwrap_err());
            assert!(msg.contains(&format!("has mode {mode:03o}")), "{msg}");
        }
        std::fs::set_permissions(&h.logs, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(pm.log_source("redis", 100, false).is_ok());
    }

    /// Reading logs never creates the log directory; the expected file is still named.
    #[test]
    fn launchd_logs_without_a_log_dir_create_nothing() {
        let h = host();
        let missing = h.logs.join("missing");
        let pm = NixPackageManager::with_services(
            Path::new("/src/app"),
            "app",
            ServiceHost {
                kind: UnitKind::Launchd,
                unit_dir: Some(h.units.to_path_buf()),
                log_dir: Some(missing.clone()),
                control: Box::new(h.control.clone()),
            },
        );
        assert_eq!(
            pm.log_source("redis", 100, false).unwrap(),
            LogSource::Files(vec![missing.join(pm.names("redis").log_file)])
        );
        assert!(!missing.exists());
    }

    #[test]
    fn launchd_logs_fall_back_to_the_legacy_log_file() {
        let h = host();
        let root = Path::new("/src/app");
        let pm = h.pm(UnitKind::Launchd, root, "app");
        let legacy = h.logs.join("redis.log");
        std::fs::write(&legacy, "old\n").unwrap();
        // Another project's legacy agent: its shared log file is not ours to show.
        let agent = h.write(
            UnitKind::Launchd,
            "sh.devy.redis",
            &data(Path::new("/src/b")),
            None,
        );
        let new = h.logs.join(pm.names("redis").log_file);
        assert_eq!(
            pm.log_source("redis", 100, false).unwrap(),
            LogSource::Files(vec![new])
        );
        std::fs::remove_file(agent).unwrap();
        h.write(UnitKind::Launchd, "sh.devy.redis", &data(root), None);
        assert_eq!(
            pm.log_source("redis", 100, false).unwrap(),
            LogSource::Files(vec![legacy])
        );
    }

    fn journal_unit(source: LogSource) -> String {
        let LogSource::Command(cmd) = source else {
            panic!("expected a command");
        };
        assert_eq!(cmd.program, "journalctl");
        assert_eq!(cmd.kind, LogCommandKind::UserJournal);
        cmd.args[2].clone()
    }

    #[test]
    fn systemd_logs_read_the_user_journal() {
        let h = host();
        let pm = h.pm(UnitKind::Systemd, Path::new("/src/app"), "app");
        let unit = pm.names("redis").unit;
        for (follow, tail) in [(false, &[][..]), (true, &["-f"][..])] {
            let LogSource::Command(cmd) = pm.log_source("redis", 30, follow).unwrap() else {
                panic!("expected a command");
            };
            let mut expected = vec!["--user", "-u", &unit, "-n", "30", "--no-pager", "-o", "cat"];
            expected.extend(tail);
            assert_eq!(cmd.program, "journalctl");
            assert_eq!(cmd.args, expected);
            assert_eq!(cmd.kind, LogCommandKind::UserJournal);
        }
    }

    #[test]
    fn systemd_logs_ignore_another_projects_legacy_unit() {
        let h = host();
        let pm = h.pm(UnitKind::Systemd, Path::new("/src/app"), "app");
        let unit = pm.names("redis").unit;
        let other = data(Path::new("/src/other"));
        h.write(UnitKind::Systemd, "devy-redis.service", &other, None);
        assert_eq!(
            journal_unit(pm.log_source("redis", 30, false).unwrap()),
            unit
        );
    }

    #[test]
    fn units_of_a_colliding_root_are_never_replaced_or_stopped() {
        for kind in KINDS {
            let h = host();
            let root = Path::new("/src/app");
            let pm = h.pm(kind, root, "app");
            let names = pm.names("redis");
            // Same unit name, recorded for another root (a slug collision).
            let other = Path::new("/src/elsewhere");
            let path = h.write(kind, names.id(kind), &data(other), Some(other));
            let before = std::fs::read_to_string(&path).unwrap();
            let err = pm.stop_service("redis").unwrap_err().to_string();
            assert!(
                err.contains("belongs to the project at /src/elsewhere"),
                "{err}"
            );
            let err = pm
                .write_unit("redis", &redis_launch(), Path::new("/b"), Path::new("/b"))
                .unwrap_err()
                .to_string();
            assert!(err.contains("will not replace or stop it"), "{err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
            assert!(h.control.calls().is_empty(), "{kind:?}");
            // Its own unit is replaced as usual.
            h.write(kind, names.id(kind), &data(root), Some(root));
            pm.stop_service("redis").unwrap();
        }
    }

    #[test]
    fn systemd_logs_fall_back_to_the_legacy_unit() {
        let h = host();
        let root = Path::new("/src/app");
        let pm = h.pm(UnitKind::Systemd, root, "app");
        let unit = pm.names("redis").unit;
        h.write(UnitKind::Systemd, "devy-redis.service", &data(root), None);
        assert_eq!(
            journal_unit(pm.log_source("redis", 30, false).unwrap()),
            "devy-redis.service"
        );
        // Once the per-project unit exists, it is read.
        h.write(UnitKind::Systemd, &unit, &data(root), Some(root));
        assert_eq!(
            journal_unit(pm.log_source("redis", 30, false).unwrap()),
            unit
        );
    }

    #[test]
    fn nix_name_is_nix() {
        let pm = NixPackageManager::for_project(&crate::test_support::tmp_dir(), "app");
        assert_eq!(pm.name(), "nix");
    }

    #[test]
    fn nix_is_available_when_nix_binary_found() {
        // is_available() returns true iff the resolved nix binary exists.
        // find_nix_binary checks PATH then standard locations, so this holds
        // even when Nix is installed but not on PATH.
        let pm = NixPackageManager::for_project(&crate::test_support::tmp_dir(), "app");
        let expected = find_nix_binary("nix").is_some();
        assert_eq!(pm.is_available(), expected);
    }

    #[test]
    fn profile_path_is_project_local() {
        let dir = crate::test_support::tmp_dir();
        let pm = NixPackageManager::for_project(&dir, "app");
        assert!(
            pm.profile_path.ends_with(".devy/nix-profile"),
            "profile_path must be project-local, got: {}",
            pm.profile_path.display()
        );
        assert!(
            !pm.profile_path.to_string_lossy().contains(".nix-profile/"),
            "profile_path must not reference the global ~/.nix-profile"
        );
    }

    #[test]
    fn path_prepends_includes_profile_bin() {
        let pm = NixPackageManager::for_project(&crate::test_support::tmp_dir(), "app");
        let prepends = pm.path_prepends(std::path::Path::new("/irrelevant"));
        assert_eq!(prepends.len(), 1);
        // Use Path::ends_with (component-aware) rather than str::ends_with so the
        // check works on Windows where to_string_lossy() produces backslashes.
        assert!(std::path::Path::new(&prepends[0]).ends_with(".devy/nix-profile/bin"));
    }

    // ── service launch ────────────────────────────────────────────────────────

    fn redis_launch() -> LaunchSpec {
        LaunchSpec::new(
            "redis-server",
            [
                "--port".into(),
                "51000".into(),
                "--dir".into(),
                "/proj dir/.devy/data/redis".into(),
            ],
        )
    }

    #[test]
    fn start_service_without_launch_spec_is_unsupported() {
        let pm = NixPackageManager::for_project(&crate::test_support::tmp_dir(), "app");
        let err = pm.start_service("someservice", None).unwrap_err();
        assert!(
            err.to_string()
                .contains("not yet supported with the nix backend"),
            "{err}"
        );
    }

    #[test]
    fn ensure_no_control_chars_rejects_cr_nul_and_newline_without_echoing_value() {
        let program = vec!["/bin/meilisearch".to_string()];
        for bad in ["x\rExecStartPre=/bin/sh", "x\0y", "x\ny"] {
            let env = vec![("MEILI_MASTER_KEY".to_string(), bad.to_string())];
            let err = ensure_no_control_chars(&program, &env, None)
                .unwrap_err()
                .to_string();
            assert!(err.contains("MEILI_MASTER_KEY"), "{err}");
            assert!(!err.contains("ExecStartPre"), "{err}");
        }
        let bad_args = vec!["/bin/x".to_string(), "a\rb".to_string()];
        assert!(ensure_no_control_chars(&bad_args, &[], None).is_err());
        let ok_env = vec![("K".to_string(), "p@ss w\"o%rd$".to_string())];
        assert!(ensure_no_control_chars(&program, &ok_env, Some(Path::new("/srv/app"))).is_ok());
        let err = ensure_no_control_chars(
            &program,
            &ok_env,
            Some(Path::new("/srv/app\nExecStartPre=/bin/sh")),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("working directory"), "{err}");
        assert!(!err.contains("ExecStartPre"), "{err}");
    }

    #[test]
    fn systemd_quote_escapes_carriage_return() {
        let q = systemd_quote("a\rb", false);
        assert!(!q.contains('\r'));
        assert_eq!(q, "\"a\\rb\"");
    }

    #[cfg(unix)]
    #[test]
    fn program_arguments_prefix_profile_binary() {
        let args = program_arguments(&redis_launch(), Path::new("/p/.devy/nix-profile/bin"));
        assert_eq!(args[0], "/p/.devy/nix-profile/bin/redis-server");
        assert_eq!(
            args[1..],
            ["--port", "51000", "--dir", "/proj dir/.devy/data/redis"]
        );
    }

    #[test]
    fn unit_environment_adds_profile_path() {
        let mut launch = redis_launch();
        launch.env.push(("A".into(), "b".into()));
        let env = unit_environment(&launch, Path::new("/p/bin"));
        assert_eq!(env[0], ("A".to_string(), "b".to_string()));
        assert!(
            env.iter()
                .any(|(k, v)| k == "PATH" && v.starts_with("/p/bin:"))
        );
    }

    #[test]
    fn launchagent_plist_has_arguments_and_environment() {
        let plist = launchagent_plist(
            "sh.devy.redis",
            &[
                "/p/bin/redis-server".into(),
                "--dir".into(),
                "/a b/\"q\"&<x>".into(),
            ],
            &[
                ("RABBITMQ_NODE_PORT".into(), "5 & 6".into()),
                (PROJECT_ROOT_VAR.into(), "/src/my app 100% & co".into()),
            ],
            Some(Path::new("/p/.devy/data/redis")),
            Path::new("/tmp/devy-redis.log"),
        );
        assert!(plist.contains("<key>ProgramArguments</key>"));
        assert!(
            plist.contains("<string>/p/bin/redis-server</string>\n        <string>--dir</string>")
        );
        assert!(plist.contains("<string>/a b/&quot;q&quot;&amp;&lt;x&gt;</string>"));
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(
            plist.contains("<key>RABBITMQ_NODE_PORT</key>\n        <string>5 &amp; 6</string>")
        );
        assert!(plist.contains(
            "<key>DEVY_PROJECT_ROOT</key>\n        <string>/src/my app 100% &amp; co</string>"
        ));
        assert!(plist.contains("<key>KeepAlive</key>\n    <false/>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <false/>"));
        assert!(plist.contains("<string>/tmp/devy-redis.log</string>"));
        assert!(
            plist.contains("<key>WorkingDirectory</key>\n    <string>/p/.devy/data/redis</string>")
        );
    }

    #[test]
    fn systemd_unit_quotes_arguments_and_environment() {
        let unit = systemd_unit(
            "redis",
            &[
                "/p/bin/redis-server".into(),
                "--dir".into(),
                "/a b/\"q\"\\x%i$HOME".into(),
            ],
            &[
                ("MINIO_ROOT_PASSWORD".into(), "p\"w 100%$".into()),
                (PROJECT_ROOT_VAR.into(), "/src/my app 100%".into()),
            ],
            Some(Path::new("/p/data 100%")),
        );
        assert!(
            unit.contains(
                "ExecStart=\"/p/bin/redis-server\" \"--dir\" \"/a b/\\\"q\\\"\\\\x%%i$$HOME\"\n"
            ),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=\"MINIO_ROOT_PASSWORD=p\\\"w 100%%$\"\n"),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=\"DEVY_PROJECT_ROOT=/src/my app 100%%\"\n"),
            "{unit}"
        );
        assert_eq!(
            parse_systemd_unit(&unit).project_root.as_deref(),
            Some("/src/my app 100%")
        );
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WorkingDirectory=/p/data 100%%\n"), "{unit}");
    }

    /// A fake store package whose read-only directories are made writable again on
    /// drop, so its `TempDir` can be removed.
    #[cfg(unix)]
    struct FakeStore(crate::test_support::TempDir);

    #[cfg(unix)]
    impl std::ops::Deref for FakeStore {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    #[cfg(unix)]
    impl Drop for FakeStore {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            for d in ["config", "config/jvm.options.d"] {
                let _ = std::fs::set_permissions(
                    self.0.join(d),
                    std::fs::Permissions::from_mode(0o755),
                );
            }
        }
    }

    /// A fake read-only store package with `config/` (a nested dir included) and an
    /// optional security plugin, like nixpkgs' opensearch.
    #[cfg(unix)]
    fn fake_store_package(with_security: bool) -> FakeStore {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_support::tmp_dir();
        let config = root.join("config");
        std::fs::create_dir_all(config.join("jvm.options.d")).unwrap();
        std::fs::write(config.join("jvm.options"), "-Xms1g\n").unwrap();
        std::fs::write(config.join("jvm.options.d/heap.options"), "-Xmx1g\n").unwrap();
        if with_security {
            std::fs::create_dir_all(root.join("plugins/opensearch-security")).unwrap();
        }
        for f in ["config/jvm.options", "config/jvm.options.d/heap.options"] {
            std::fs::set_permissions(root.join(f), std::fs::Permissions::from_mode(0o444)).unwrap();
        }
        for d in ["config/jvm.options.d", "config"] {
            std::fs::set_permissions(root.join(d), std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        FakeStore(root)
    }

    fn search_launch(data: &Path) -> LaunchSpec {
        LaunchSpec {
            seed_dirs: vec![crate::modules::SeedDir {
                from_package: "config".into(),
                to: data.join("config"),
                rewrites: vec![
                    crate::modules::SeedRewrite {
                        file: "jvm.options".into(),
                        from: "-Xms1g".into(),
                        to: "-Xms2g".into(),
                    },
                    crate::modules::SeedRewrite {
                        file: "no-such.options".into(),
                        from: "a".into(),
                        to: "b".into(),
                    },
                ],
            }],
            conditional_args: vec![crate::modules::ConditionalArgs {
                package_path: "plugins/opensearch-security".into(),
                args: vec!["-E".into(), "plugins.security.disabled=true".into()],
            }],
            ..redis_launch()
        }
    }

    #[cfg(unix)]
    #[test]
    fn prepare_seeds_missing_config_writable() {
        use std::os::unix::fs::PermissionsExt;
        let root = fake_store_package(false);
        let data = crate::test_support::tmp_dir();
        prepare_package_files("opensearch", &search_launch(&data), Some(&root)).unwrap();
        for f in ["config/jvm.options", "config/jvm.options.d/heap.options"] {
            let path = data.join(f);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert!(mode & 0o200 != 0, "{f} must be owner-writable: {mode:o}");
            std::fs::write(&path, "edited").unwrap();
        }
        // The server creates its keystore in the copied config dir.
        std::fs::write(data.join("config/opensearch.keystore"), "").unwrap();
        assert!(!data.join(".config.seeding").exists());
    }

    #[cfg(unix)]
    #[test]
    fn prepare_applies_rewrites_on_first_seed_skipping_missing_files() {
        let root = fake_store_package(false);
        let data = crate::test_support::tmp_dir();
        prepare_package_files("opensearch", &search_launch(&data), Some(&root)).unwrap();
        assert_eq!(
            std::fs::read_to_string(data.join("config/jvm.options")).unwrap(),
            "-Xms2g\n"
        );
        assert!(!data.join("config/no-such.options").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("config/jvm.options")).unwrap(),
            "-Xms1g\n",
            "the package's own file must not change"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepare_leaves_existing_config_untouched() {
        let root = fake_store_package(false);
        let data = crate::test_support::tmp_dir();
        std::fs::create_dir_all(data.join("config")).unwrap();
        // Matches the rewrite's `from`; an existing seed must not be rewritten.
        std::fs::write(data.join("config/jvm.options"), "-Xms1g\n").unwrap();
        prepare_package_files("opensearch", &search_launch(&data), Some(&root)).unwrap();
        assert_eq!(
            std::fs::read_to_string(data.join("config/jvm.options")).unwrap(),
            "-Xms1g\n"
        );
        assert!(!data.join("config/jvm.options.d").exists());
    }

    #[cfg(unix)]
    #[test]
    fn prepare_adds_conditional_args_only_when_path_exists() {
        let data = crate::test_support::tmp_dir();
        let launch = search_launch(&data);
        let without = fake_store_package(false);
        let spec = prepare_package_files("opensearch", &launch, Some(&without)).unwrap();
        assert_eq!(spec.args, redis_launch().args);

        let data = crate::test_support::tmp_dir();
        let launch = search_launch(&data);
        let with = fake_store_package(true);
        let spec = prepare_package_files("opensearch", &launch, Some(&with)).unwrap();
        assert_eq!(
            spec.args[spec.args.len() - 2..],
            ["-E", "plugins.security.disabled=true"]
        );
    }

    #[test]
    fn prepare_without_package_root_errors() {
        let data = crate::test_support::tmp_dir();
        let err = prepare_package_files("elasticsearch", &search_launch(&data), None)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("Failed to prepare elasticsearch config"),
            "{err}"
        );
        assert!(!data.join("config").exists());
    }

    #[test]
    fn prepare_sets_package_env_from_root() {
        let root = crate::test_support::tmp_dir();
        let launch = LaunchSpec {
            package_env: vec![
                ("ES_HOME".into(), String::new()),
                ("ES_LIB".into(), "lib".into()),
            ],
            ..redis_launch()
        };
        let spec = prepare_package_files("elasticsearch", &launch, Some(&root)).unwrap();
        assert_eq!(
            spec.env,
            [
                ("ES_HOME".into(), root.to_string_lossy().into_owned()),
                (
                    "ES_LIB".into(),
                    root.join("lib").to_string_lossy().into_owned()
                ),
            ]
        );
        let err = prepare_package_files("elasticsearch", &launch, None)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("Failed to prepare elasticsearch config"),
            "{err}"
        );
    }

    #[test]
    fn prepare_without_package_parts_needs_no_root() {
        let spec = prepare_package_files("redis", &redis_launch(), None).unwrap();
        assert_eq!(spec, redis_launch());
    }

    #[test]
    fn run_init_skipped_when_marker_exists() {
        let dir = crate::test_support::tmp_dir();
        let marker = dir.join("PG_VERSION");
        std::fs::write(&marker, "16").unwrap();
        let launch = LaunchSpec {
            init: Some(crate::modules::InitStep {
                marker,
                // Would fail if run: the binary doesn't exist.
                cmd: vec!["no-such-initdb".into()],
            }),
            ..redis_launch()
        };
        assert!(run_init("postgresql", &launch, &dir, &dir).is_ok());
    }

    #[test]
    fn run_init_failure_reports_failed_to_initialize() {
        let dir = crate::test_support::tmp_dir();
        let launch = LaunchSpec {
            init: Some(crate::modules::InitStep {
                marker: dir.join("PG_VERSION"),
                cmd: vec!["no-such-initdb".into()],
            }),
            ..redis_launch()
        };
        let err = run_init("postgresql", &launch, &dir, &dir).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Failed to initialize postgresql"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_init_runs_command_from_profile_bin_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::tmp_dir();
        let script = dir.join("fake-initdb");
        // Appends to a counter file and creates the marker passed as $1.
        std::fs::write(
            &script,
            "#!/bin/sh\necho run >> \"$(dirname \"$1\")/count\"\ntouch \"$1\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let marker = dir.join("PG_VERSION");
        let launch = LaunchSpec {
            init: Some(crate::modules::InitStep {
                marker: marker.clone(),
                cmd: vec!["fake-initdb".into(), marker.to_string_lossy().into_owned()],
            }),
            ..redis_launch()
        };
        run_init("postgresql", &launch, &dir, &dir).unwrap();
        run_init("postgresql", &launch, &dir, &dir).unwrap();
        assert!(marker.exists());
        let count = std::fs::read_to_string(dir.join("count")).unwrap();
        assert_eq!(count.lines().count(), 1, "init must run exactly once");
    }

    #[cfg(unix)]
    #[test]
    fn run_init_nonzero_exit_fails() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::tmp_dir();
        let script = dir.join("bad-init");
        std::fs::write(&script, "#!/bin/sh\necho boom >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let launch = LaunchSpec {
            init: Some(crate::modules::InitStep {
                marker: dir.join("marker"),
                cmd: vec!["bad-init".into()],
            }),
            ..redis_launch()
        };
        let err = run_init("mysql", &launch, &dir, &dir)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("Failed to initialize mysql"), "{err}");
        assert!(err.contains("boom"), "stderr must be included: {err}");
    }

    #[test]
    fn profile_find_pkg_old_array_format() {
        let json = serde_json::json!([
            {"pname": "git", "version": "2.44.0"},
            {"pname": "redis", "version": "7.2.4"},
        ]);
        assert_eq!(profile_find_pkg(&json, "redis"), Some(Some("7.2.4".into())));
        assert_eq!(profile_find_pkg(&json, "curl"), None);
    }

    #[test]
    fn profile_find_pkg_new_nix_218_format_with_pname() {
        // Nix ≥ 2.18 returns {"version":2,"elements":{"name":{...}}}
        let json = serde_json::json!({
            "version": 2,
            "elements": {
                "redis-7.2.4": {
                    "attrPath": "legacyPackages.x86_64-linux.redis",
                    "pname": "redis",
                    "version": "7.2.4",
                    "active": true
                }
            }
        });
        assert_eq!(profile_find_pkg(&json, "redis"), Some(Some("7.2.4".into())));
        assert_eq!(profile_find_pkg(&json, "curl"), None);
    }

    #[test]
    fn profile_find_pkg_new_nix_218_format_attrpath_fallback() {
        // Some Nix 2.18+ elements omit pname; fall back to the last component of attrPath.
        let json = serde_json::json!({
            "version": 2,
            "elements": {
                "redis": {
                    "attrPath": "legacyPackages.x86_64-linux.redis",
                    "active": true
                }
            }
        });
        // Installed, but with no version and no store paths to derive one from.
        assert_eq!(profile_find_pkg(&json, "redis"), Some(None));
        assert_eq!(profile_find_pkg(&json, "curl"), None);
    }

    // Nix ≥ 2.20 (profile JSON v3): no pname or version, only attrPath.
    fn v3_profile(attrs: &[&str]) -> serde_json::Value {
        let elements: serde_json::Map<String, serde_json::Value> = attrs
            .iter()
            .map(|a| {
                (
                    a.to_string(),
                    serde_json::json!({
                        "active": true,
                        "attrPath": format!("legacyPackages.aarch64-darwin.{a}"),
                        "originalUrl": "flake:nixpkgs",
                        "storePaths": [format!("/nix/store/abc-{a}")],
                    }),
                )
            })
            .collect();
        serde_json::json!({ "version": 3, "elements": elements })
    }

    #[test]
    fn profile_find_pkg_derives_version_from_v3_store_path() {
        let json = serde_json::json!({
            "version": 3,
            "elements": {
                "redis": {
                    "attrPath": "legacyPackages.aarch64-darwin.redis",
                    "storePaths": ["/nix/store/0123456789abcdfghijklmnpqrsvwxyz-redis-8.6.3"]
                }
            }
        });
        assert_eq!(profile_find_pkg(&json, "redis"), Some(Some("8.6.3".into())));
    }

    #[test]
    fn profile_find_pkg_prefers_explicit_version() {
        let json = serde_json::json!({
            "version": 2,
            "elements": {
                "redis": {
                    "attrPath": "legacyPackages.x86_64-linux.redis",
                    "version": "7.2.4",
                    "storePaths": ["/nix/store/abc-redis-8.6.3"]
                }
            }
        });
        assert_eq!(profile_find_pkg(&json, "redis"), Some(Some("7.2.4".into())));
    }

    #[test]
    fn resolved_version_is_absent_not_unknown() {
        // What `resolved_version` reports for an installed entry with no derivable version.
        let json = serde_json::json!({
            "version": 3,
            "elements": {
                "hello": {
                    "attrPath": "legacyPackages.x86_64-linux.hello",
                    "storePaths": ["/nix/store/abc-hello-world"]
                }
            }
        });
        assert_eq!(profile_find_pkg(&json, "hello"), Some(None));
        assert_eq!(profile_find_pkg(&json, "hello").flatten(), None);
    }

    fn store_version(names: &[&str]) -> Option<String> {
        let paths: Vec<String> = names
            .iter()
            .map(|n| format!("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-{n}"))
            .collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        version_from_store_paths(&refs)
    }

    #[test]
    fn version_from_store_paths_splits_like_parse_drv_name() {
        assert_eq!(store_version(&["redis-8.6.3"]), Some("8.6.3".into()));
        assert_eq!(
            store_version(&["apache-kafka-2.13-4.3.1"]),
            Some("2.13-4.3.1".into())
        );
        assert_eq!(store_version(&["python3-3.14.7"]), Some("3.14.7".into()));
        assert_eq!(store_version(&["mongodb-ce-8.2.12"]), Some("8.2.12".into()));
        assert_eq!(store_version(&["awscli2-2.31.5"]), Some("2.31.5".into()));
    }

    #[test]
    fn version_from_store_paths_picks_main_output() {
        assert_eq!(
            store_version(&["mysql-8.4.11-man", "mysql-8.4.11"]),
            Some("8.4.11".into())
        );
        assert_eq!(
            store_version(&["python3-3.14.7", "python3-3.14.7-debug"]),
            Some("3.14.7".into())
        );
    }

    #[test]
    fn version_from_store_paths_without_version_is_none() {
        assert_eq!(store_version(&["hello-world"]), None);
        assert_eq!(store_version(&[]), None);
    }

    #[test]
    fn profile_find_pkg_matches_versioned_attr() {
        let json = v3_profile(&["nodejs_22"]);
        assert!(profile_find_pkg(&json, "nodejs_22").is_some());
    }

    #[test]
    fn profile_find_pkg_version_change_is_not_installed() {
        let json = v3_profile(&["nodejs_22"]);
        assert!(profile_find_pkg(&json, "nodejs_24").is_none());
        // The unversioned attr is a different package from nodejs_22.
        assert!(profile_find_pkg(&json, "nodejs").is_none());
    }

    #[test]
    fn profile_find_pkg_matches_unversioned_attr() {
        let json = v3_profile(&["jq", "nodejs"]);
        assert!(profile_find_pkg(&json, "nodejs").is_some());
        assert!(profile_find_pkg(&json, "nodejs_22").is_none());
    }

    #[test]
    fn profile_find_pkg_matches_mapped_attr_not_pname() {
        // mysql84's pname is "mysql" and jdk21's is "openjdk": match on attrPath.
        let json = serde_json::json!({
            "version": 2,
            "elements": {
                "mysql84": {
                    "attrPath": "legacyPackages.x86_64-linux.mysql84",
                    "pname": "mysql",
                    "version": "8.4.11"
                },
                "jdk21": {
                    "attrPath": "legacyPackages.x86_64-linux.jdk21",
                    "pname": "openjdk",
                    "version": "21.0.11"
                }
            }
        });
        assert_eq!(
            profile_find_pkg(&json, "mysql84"),
            Some(Some("8.4.11".into()))
        );
        assert_eq!(
            profile_find_pkg(&json, "jdk21"),
            Some(Some("21.0.11".into()))
        );
        assert_eq!(profile_find_pkg(&json, "mysql"), None);
    }

    #[test]
    fn mariadb_wins_profile_conflicts() {
        assert_eq!(install_priority("mariadb"), Some(4));
        assert_eq!(install_priority("mariadb_114"), Some(4));
        assert_eq!(install_priority("mysql84"), None);
        assert_eq!(install_priority("redis"), None);
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn install_cmd_profile_style() {
        let p = Path::new("/p/.devy/nix-profile");
        assert_eq!(
            install_cmd(NixStyle::Profile, p, "redis", false, false),
            InstallCmd {
                args: strs(&[
                    "profile",
                    "install",
                    "--profile",
                    "/p/.devy/nix-profile",
                    "nixpkgs#redis"
                ]),
                env: vec![],
            }
        );
        assert_eq!(
            install_cmd(NixStyle::Profile, p, "mongodb-ce", true, false),
            InstallCmd {
                args: strs(&[
                    "profile",
                    "install",
                    "--profile",
                    "/p/.devy/nix-profile",
                    "--impure",
                    "nixpkgs#mongodb-ce",
                ]),
                env: vec![("NIXPKGS_ALLOW_UNFREE", "1")],
            }
        );
        assert_eq!(
            install_cmd(NixStyle::Profile, p, "mariadb", false, false).args[4..],
            strs(&["--priority", "4", "nixpkgs#mariadb"])
        );
    }

    #[test]
    fn install_cmd_allows_insecure_alone_and_with_unfree() {
        let p = Path::new("/p/.devy/nix-profile");
        let insecure = install_cmd(NixStyle::Profile, p, "pkg", false, true);
        assert_eq!(insecure.args[4..], strs(&["--impure", "nixpkgs#pkg"]));
        assert_eq!(insecure.env, [("NIXPKGS_ALLOW_INSECURE", "1")]);
        let both = install_cmd(NixStyle::Profile, p, "elasticsearch", true, true);
        assert_eq!(both.args[4..], strs(&["--impure", "nixpkgs#elasticsearch"]));
        assert_eq!(
            both.env,
            [
                ("NIXPKGS_ALLOW_UNFREE", "1"),
                ("NIXPKGS_ALLOW_INSECURE", "1")
            ]
        );
        assert_eq!(
            install_cmd(NixStyle::Env, p, "elasticsearch", true, true).env,
            [
                ("NIXPKGS_ALLOW_UNFREE", "1"),
                ("NIXPKGS_ALLOW_INSECURE", "1")
            ]
        );
    }

    #[test]
    fn install_cmd_env_style() {
        let p = Path::new("/p/.devy/nix-profile");
        assert_eq!(
            install_cmd(NixStyle::Env, p, "redis", false, false),
            InstallCmd {
                args: strs(&["--profile", "/p/.devy/nix-profile", "-iA", "nixpkgs.redis"]),
                env: vec![],
            }
        );
        assert_eq!(
            install_cmd(NixStyle::Env, p, "vault", true, false),
            InstallCmd {
                args: strs(&["--profile", "/p/.devy/nix-profile", "-iA", "nixpkgs.vault"]),
                env: vec![("NIXPKGS_ALLOW_UNFREE", "1")],
            }
        );
    }

    #[test]
    fn profile_package_bins_lists_every_output() {
        let json = serde_json::json!({
            "version": 3,
            "elements": {
                "mysql84": {
                    "attrPath": "legacyPackages.aarch64-darwin.mysql84",
                    "storePaths": [
                        "/nix/store/aaa-mysql-8.4.11-man",
                        "/nix/store/bbb-mysql-8.4.11"
                    ]
                }
            }
        });
        assert_eq!(
            profile_package_bins(&json, "mysql84"),
            vec![
                PathBuf::from("/nix/store/aaa-mysql-8.4.11-man/bin"),
                PathBuf::from("/nix/store/bbb-mysql-8.4.11/bin"),
            ]
        );
        assert!(profile_package_bins(&json, "postgresql").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn run_init_resolves_command_in_exec_dir() {
        use std::os::unix::fs::PermissionsExt;
        let exec_dir = crate::test_support::tmp_dir();
        let profile_bin = crate::test_support::tmp_dir();
        let marker = exec_dir.join("marker");
        let script = exec_dir.join("fake-init");
        std::fs::write(&script, "#!/bin/sh\ntouch \"$1\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let launch = LaunchSpec {
            init: Some(crate::modules::InitStep {
                marker: marker.clone(),
                cmd: vec!["fake-init".into(), marker.to_string_lossy().into_owned()],
            }),
            ..redis_launch()
        };
        // fake-init exists only in exec_dir, not in the profile bin.
        run_init("mysql", &launch, &exec_dir, &profile_bin).unwrap();
        assert!(marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn bootstrap_runs_pinned_determinate_installer_with_install_no_confirm() {
        use crate::installers::{self, Interpreter, test_hooks};
        assert!(
            installers::NIX
                .url
                .starts_with("https://install.determinate.systems/nix/tag/v")
        );
        test_hooks::clear();
        test_hooks::serve(&installers::NIX, b"exit 0\n");
        let result = pm_with_missing_profile().bootstrap();
        let runs = test_hooks::runs();
        test_hooks::clear();
        result.unwrap();
        assert_eq!(
            runs,
            vec![(
                installers::NIX.name,
                Interpreter::Sh,
                vec!["install".into(), "--no-confirm".into()]
            )]
        );
    }

    #[cfg(unix)]
    #[test]
    fn bootstrap_failure_reports_nix_installation_failed() {
        use crate::installers::{self, test_hooks};
        test_hooks::clear();
        test_hooks::serve(&installers::NIX, b"exit 3\n");
        let err = pm_with_missing_profile().bootstrap().unwrap_err();
        test_hooks::clear();
        assert_eq!(err.to_string(), "Nix installation failed");
    }

    #[test]
    fn bootstrap_without_a_verified_download_fails_before_running_anything() {
        use crate::installers::test_hooks;
        test_hooks::clear();
        assert!(pm_with_missing_profile().bootstrap().is_err());
        assert!(test_hooks::runs().is_empty());
    }

    fn pm_with_missing_profile() -> NixPackageManager {
        NixPackageManager {
            profile_path: PathBuf::from("/tmp/devy_test_nonexistent_profile_xyzzy"),
            ..NixPackageManager::for_project(Path::new("/tmp/devy_test_xyzzy"), "app")
        }
    }

    #[test]
    fn is_package_installed_returns_false_when_profile_missing() {
        // If the profile symlink doesn't exist, Nix commands might fall back to the
        // user's global profile. We must short-circuit and return false here.
        let dep = crate::config::Dependency::simple("redis");
        assert!(
            !pm_with_missing_profile()
                .is_package_installed(&dep)
                .unwrap(),
            "must return false when the project profile does not exist"
        );
    }

    #[test]
    fn resolved_version_returns_none_when_profile_missing() {
        let dep = crate::config::Dependency::simple("redis");
        assert_eq!(
            pm_with_missing_profile().resolved_version(&dep).unwrap(),
            None
        );
    }

    // ── nix-env (legacy) style ──

    /// `nix-env -q --json` output for installed packages named `<pname>-<version>`.
    fn env_profile(names: &[&str]) -> serde_json::Value {
        let entries = names.iter().map(|name| {
            let (pname, version) = name.rsplit_once('-').unwrap();
            let entry = serde_json::json!({"name": name, "pname": pname, "version": version});
            (name.to_string(), entry)
        });
        serde_json::Value::Object(entries.collect())
    }

    fn manifest(pairs: &[(&str, &str)]) -> EnvManifest {
        pairs
            .iter()
            .map(|(a, n)| (a.to_string(), n.to_string()))
            .collect()
    }

    #[test]
    fn env_manifest_round_trips() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("nix-env-attrs.json");
        let m = manifest(&[("mysql84", "mysql-8.4.11"), ("jq", "jq-1.7.1")]);
        write_env_manifest(&path, &m).unwrap();
        assert_eq!(read_env_manifest(&path), m);
    }

    #[test]
    fn env_manifest_missing_or_malformed_reads_empty() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("nix-env-attrs.json");
        assert!(read_env_manifest(&path).is_empty());
        std::fs::write(&path, "{not json").unwrap();
        assert!(read_env_manifest(&path).is_empty());
        std::fs::write(&path, r#"["mysql84"]"#).unwrap();
        assert!(read_env_manifest(&path).is_empty());
    }

    #[test]
    fn env_manifest_write_replaces_atomically() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("nix-env-attrs.json");
        write_env_manifest(&path, &manifest(&[("redis", "redis-7.2.4")])).unwrap();
        let m = manifest(&[("jq", "jq-1.7.1")]);
        write_env_manifest(&path, &m).unwrap();
        assert_eq!(read_env_manifest(&path), m);
        // Only the manifest is left behind; the temp file was renamed over it.
        let files: Vec<_> = std::fs::read_dir(&*dir).unwrap().collect();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn env_find_attr_matches_recorded_renamed_attr() {
        let json = env_profile(&["mysql-8.4.11"]);
        let m = manifest(&[("mysql84", "mysql-8.4.11")]);
        assert_eq!(
            env_find_attr(&json, &m, "mysql84"),
            Some(Some("8.4.11".into()))
        );
    }

    #[test]
    fn env_find_attr_versioned_record_does_not_satisfy_unversioned() {
        let json = env_profile(&["nodejs-22.11.0"]);
        let m = manifest(&[("nodejs_22", "nodejs-22.11.0")]);
        assert_eq!(env_find_attr(&json, &m, "nodejs"), None);
    }

    #[test]
    fn env_find_attr_falls_back_to_pname_without_record() {
        let json = env_profile(&["jq-1.7.1", "git-2.44.0"]);
        let m = EnvManifest::new();
        assert_eq!(env_find_attr(&json, &m, "jq"), Some(Some("1.7.1".into())));
        assert_eq!(env_find_attr(&json, &m, "curl"), None);
    }

    #[test]
    fn env_find_attr_recorded_name_absent_is_not_installed() {
        // Recorded, then removed with `nix-env -e redis` (or upgraded by hand).
        let m = manifest(&[("redis", "redis-7.2.4")]);
        assert_eq!(
            env_find_attr(&env_profile(&["jq-1.7.1"]), &m, "redis"),
            None
        );
        // A record pins the exact name: a different redis doesn't fall back to pname.
        assert_eq!(
            env_find_attr(&env_profile(&["redis-7.4.0"]), &m, "redis"),
            None
        );
    }

    #[test]
    fn env_find_attr_version_comes_from_the_recorded_entry() {
        let json = env_profile(&["python3-3.13.1", "python3-3.12.8"]);
        let m = manifest(&[("python312", "python3-3.12.8")]);
        assert_eq!(
            env_find_attr(&json, &m, "python312"),
            Some(Some("3.12.8".into()))
        );
    }

    #[test]
    fn env_query_name_args_query_the_attr_from_nixpkgs() {
        assert_eq!(
            env_query_name_args("mysql84"),
            strs(&["-qaA", "nixpkgs.mysql84", "--json"])
        );
    }

    #[test]
    fn env_drv_name_reads_the_single_entry() {
        let json = serde_json::json!({"mysql84": {"name": "mysql-8.4.11", "pname": "mysql"}});
        assert_eq!(env_drv_name(&json), Some("mysql-8.4.11".into()));
        assert_eq!(env_drv_name(&serde_json::json!({})), None);
        let two = serde_json::json!({"a": {"name": "a-1"}, "b": {"name": "b-1"}});
        assert_eq!(env_drv_name(&two), None);
    }

    #[test]
    fn env_install_records_name_after_success() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("nix-env-attrs.json");
        env_install_and_record(&path, "mysql84", Some("mysql-8.4.11".into()), || Ok(())).unwrap();
        assert_eq!(
            read_env_manifest(&path),
            manifest(&[("mysql84", "mysql-8.4.11")])
        );
    }

    #[test]
    fn env_install_failure_records_nothing() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("nix-env-attrs.json");
        let err = env_install_and_record(&path, "mysql84", Some("mysql-8.4.11".into()), || {
            bail!("install failed")
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "install failed");
        assert!(!path.exists());
    }

    #[test]
    fn env_install_without_name_drops_stale_record() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("nix-env-attrs.json");
        let m = manifest(&[("redis", "redis-7.2.4"), ("jq", "jq-1.7.1")]);
        write_env_manifest(&path, &m).unwrap();
        env_install_and_record(&path, "redis", None, || Ok(())).unwrap();
        assert_eq!(read_env_manifest(&path), manifest(&[("jq", "jq-1.7.1")]));
    }

    #[test]
    fn env_style_lookup_uses_manifest_and_profile() {
        // What `query_version` does in the Env style: the manifest file plus the
        // profile listing decide `is_package_installed` and `resolved_version`.
        let dir = crate::test_support::tmp_dir();
        let pm = NixPackageManager::for_project(&dir, "app");
        let path = pm.env_manifest_path();
        assert_eq!(path, dir.join(".devy").join("nix-env-attrs.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_env_manifest(&path, &manifest(&[("nodejs_22", "nodejs-22.11.0")])).unwrap();

        let json = env_profile(&["nodejs-22.11.0", "jq-1.7.1"]);
        let lookup = |attr| env_find_attr(&json, &read_env_manifest(&path), attr);
        assert!(lookup("nodejs_22").is_some());
        assert_eq!(lookup("nodejs_22").flatten(), Some("22.11.0".into()));
        assert!(lookup("nodejs").is_none());
        assert_eq!(lookup("jq").flatten(), Some("1.7.1".into()));
    }

    // ── devy prune ──────────────────────────────────────────────────────────

    impl Host {
        fn pruner(&self, kind: UnitKind) -> UnitPruner {
            UnitPruner {
                host: ServiceHost {
                    kind,
                    unit_dir: Some(self.units.to_path_buf()),
                    log_dir: Some(self.logs.to_path_buf()),
                    control: Box::new(self.control.clone()),
                },
            }
        }
    }

    /// A live checkout (with a devy.yml) and the root of a removed one, in a temp dir.
    fn checkouts() -> (crate::test_support::TempDir, PathBuf, PathBuf) {
        let dir = crate::test_support::tmp_dir();
        let live = dir.join("app");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("devy.yml"), "name: app\n").unwrap();
        let gone = dir.join("app-feat");
        (dir, live, gone)
    }

    #[test]
    fn find_orphaned_units_finds_units_of_removed_checkouts_only() {
        for kind in KINDS {
            let h = host();
            let (_dir, live, gone) = checkouts();
            let stale = service_names(Some(&slug("app", &gone.to_string_lossy())), "redis");
            let stale_path = h.write(kind, stale.id(kind), &data(&gone), Some(&gone));
            let own = service_names(Some(&slug("app", &live.to_string_lossy())), "redis");
            h.write(kind, own.id(kind), &data(&live), Some(&live));
            // A legacy unit records no root: its owner is unknown, so it's never pruned.
            let legacy = service_names(None, "redis");
            h.write(kind, legacy.id(kind), &data(&gone), None);
            // Not devy's: another prefix, or characters devy never puts in a name.
            let foreign = match kind {
                UnitKind::Launchd => "com.example.redis",
                UnitKind::Systemd => "redis.service",
            };
            h.write(kind, foreign, &data(&gone), Some(&gone));
            let odd = match kind {
                UnitKind::Launchd => "sh.devy.a b",
                UnitKind::Systemd => "devy-a b.service",
            };
            h.write(kind, odd, &data(&gone), Some(&gone));
            // A relative recorded root is never treated as removed.
            let relative = service_names(Some("rel-00000000"), "redis");
            h.write(
                kind,
                relative.id(kind),
                &data(&gone),
                Some(Path::new("rel/app")),
            );

            let found = find_orphaned_units(kind, &h.units);
            assert_eq!(
                found,
                [OrphanedUnit {
                    id: stale.id(kind).to_string(),
                    path: stale_path,
                    root: gone.to_string_lossy().into_owned(),
                }],
                "{kind:?}"
            );
        }
    }

    #[test]
    fn find_orphaned_units_skips_a_plist_labeled_unlike_its_file() {
        let h = host();
        let (_dir, _live, gone) = checkouts();
        let path = h.write(
            UnitKind::Launchd,
            "sh.devy.app-1.other",
            &data(&gone),
            Some(&gone),
        );
        std::fs::rename(path, h.units.join("sh.devy.app-1.redis.plist")).unwrap();
        assert!(find_orphaned_units(UnitKind::Launchd, &h.units).is_empty());
    }

    #[test]
    fn find_orphaned_units_skips_directories_and_missing_dirs() {
        let h = host();
        let (_dir, _live, gone) = checkouts();
        std::fs::create_dir(h.units.join("sh.devy.app-1.redis.plist")).unwrap();
        assert!(find_orphaned_units(UnitKind::Launchd, &h.units).is_empty());
        assert!(find_orphaned_units(UnitKind::Launchd, &gone).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn find_orphaned_units_skips_symlinks() {
        let h = host();
        let (_dir, _live, gone) = checkouts();
        let target = h.write(UnitKind::Launchd, "elsewhere", &data(&gone), Some(&gone));
        std::os::unix::fs::symlink(&target, h.units.join("sh.devy.app-1.redis.plist")).unwrap();
        assert!(find_orphaned_units(UnitKind::Launchd, &h.units).is_empty());
    }

    #[test]
    fn devy_unit_id_accepts_devy_names_only() {
        let (l, s) = (UnitKind::Launchd, UnitKind::Systemd);
        assert_eq!(
            devy_unit_id(l, "sh.devy.app-1a2b3c4d.redis.plist"),
            Some("sh.devy.app-1a2b3c4d.redis")
        );
        assert_eq!(
            devy_unit_id(s, "devy-app-1a2b3c4d-redis.service"),
            Some("devy-app-1a2b3c4d-redis.service")
        );
        for (kind, name) in [
            (l, "sh.devy..plist"),
            (l, "sh.devy.redis"),
            (l, "sh.devy.x;y.plist"),
            (l, "com.example.plist"),
            (s, "devy-.service"),
            (s, "devy-redis.timer"),
            (s, "devy-$(x).service"),
            (s, "devy-a\nb.service"),
        ] {
            assert_eq!(devy_unit_id(kind, name), None, "{kind:?} {name:?}");
        }
    }

    #[test]
    fn pruner_retires_and_deletes_a_unit() {
        for kind in KINDS {
            let h = host();
            let (_dir, _live, gone) = checkouts();
            let names = service_names(Some(&slug("app", &gone.to_string_lossy())), "redis");
            let path = h.write(kind, names.id(kind), &data(&gone), Some(&gone));
            h.control.set_running(names.id(kind));
            let pruner = h.pruner(kind);
            let units = pruner.find().unwrap();
            assert_eq!(units.len(), 1, "{kind:?}");
            pruner.remove(&units[0]).unwrap();
            pruner.finish().unwrap();
            assert!(!path.exists(), "{kind:?}");
            assert_eq!(
                h.control.calls(),
                [format!("retire {}", names.id(kind)), "files_removed".into()],
                "{kind:?}"
            );
        }
    }

    #[test]
    fn pruner_leaves_a_unit_that_changed_since_it_was_listed() {
        for kind in KINDS {
            let h = host();
            let (_dir, live, gone) = checkouts();
            let names = service_names(Some(&slug("app", &gone.to_string_lossy())), "redis");
            let path = h.write(kind, names.id(kind), &data(&gone), Some(&gone));
            let pruner = h.pruner(kind);
            let units = pruner.find().unwrap();
            // The checkout came back before the user confirmed.
            std::fs::create_dir(&gone).unwrap();
            std::fs::write(gone.join("devy.yml"), "name: app\n").unwrap();
            let err = pruner.remove(&units[0]).unwrap_err().to_string();
            assert!(err.contains("changed since it was listed"), "{err}");
            // Or the file now records another root.
            std::fs::remove_file(gone.join("devy.yml")).unwrap();
            h.write(kind, names.id(kind), &data(&live), Some(&live));
            assert!(pruner.remove(&units[0]).is_err(), "{kind:?}");
            assert!(path.exists(), "{kind:?}");
            assert!(h.control.calls().is_empty(), "{kind:?}");
        }
    }
}
