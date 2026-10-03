use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::PackageManager;
use crate::config::Dependency;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NixStyle {
    /// Modern Nix with flakes: `nix profile install --profile <path> nixpkgs#attr`
    Profile,
    /// Legacy Nix: `nix-env --profile <path> -iA nixpkgs.attr`
    Env,
}

/// Searches PATH then well-known Nix installation directories for a binary.
/// Returns `None` only if the binary cannot be found anywhere.
fn find_nix_binary(name: &str) -> Option<PathBuf> {
    if let Ok(p) = which::which(name) {
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

/// Resolves a Nix binary by name, falling back to the bare name if not found.
fn resolve_nix_binary(name: &str) -> PathBuf {
    find_nix_binary(name).unwrap_or_else(|| PathBuf::from(name))
}

impl NixPackageManager {
    /// Construct with an explicit project root so the profile path is always
    /// relative to the project, not the shell's current working directory.
    pub fn for_project(project_root: &Path) -> Self {
        let profile_path = project_root.join(".devy").join("nix-profile");
        Self {
            profile_path,
            style: std::sync::OnceLock::new(),
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
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    std::fs::write(&tmp, body).with_context(|| format!("Failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("Failed to write {}", path.display()))
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
        std::fs::write(&file, text.replace(&rewrite.from, &rewrite.to))
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

// ── Service management — macOS (launchd) ──────────────────────────────────────

#[cfg(any(test, target_os = "macos"))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A launchd agent plist that runs `program` with `env` in `working_dir`, logging to `log`.
#[cfg(any(test, target_os = "macos"))]
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

#[cfg(target_os = "macos")]
fn launchagent_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("$HOME not set")?;
    Ok(PathBuf::from(home).join("Library").join("LaunchAgents"))
}

#[cfg(target_os = "macos")]
fn launchagent_path(name: &str) -> Result<PathBuf> {
    Ok(launchagent_dir()?.join(format!("sh.devy.{name}.plist")))
}

/// Where the launchd agent for `name` writes stdout and stderr: `$TMPDIR/devy-<name>.log`.
#[cfg(any(test, target_os = "macos"))]
fn launchagent_log_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("devy-{name}.log"))
}

/// The user journal of the systemd unit for `name`, which has no log file of its own.
#[cfg(any(test, target_os = "linux"))]
fn systemd_log_source(name: &str, lines: u32, follow: bool) -> super::LogSource {
    super::LogSource::journal(&format!("devy-{name}.service"), true, lines, follow)
}

/// Where `devy logs` reads service `name`'s output on this platform.
fn service_log_source(name: &str, lines: u32, follow: bool) -> super::LogSource {
    #[cfg(target_os = "macos")]
    {
        let _ = (lines, follow);
        super::LogSource::Files(vec![launchagent_log_path(name)])
    }
    #[cfg(target_os = "linux")]
    return systemd_log_source(name, lines, follow);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (name, lines, follow);
        super::LogSource::Unsupported(
            "Service management is not supported on this platform with the nix backend".into(),
        )
    }
}

#[cfg(target_os = "macos")]
fn write_launchagent(
    name: &str,
    launch: &LaunchSpec,
    exec_dir: &Path,
    profile_bin: &Path,
) -> Result<()> {
    let dir = launchagent_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    let plist = launchagent_plist(
        &format!("sh.devy.{name}"),
        &program_arguments(launch, exec_dir),
        &unit_environment(launch, profile_bin),
        launch.working_dir.as_deref(),
        &launchagent_log_path(name),
    );
    let path = launchagent_path(name)?;
    std::fs::write(&path, plist).with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> Result<()> {
    let status = Command::new("launchctl")
        .args(args)
        .status()
        .context("Failed to run launchctl")?;
    if !status.success() {
        bail!("launchctl {} failed", args.join(" "));
    }
    Ok(())
}

/// Whether launchd knows the agent at all (loaded), running or not.
#[cfg(target_os = "macos")]
fn is_loaded_macos(name: &str) -> bool {
    Command::new("launchctl")
        .args(["list", &format!("sh.devy.{name}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn is_running_macos(name: &str) -> Result<bool> {
    let label = format!("sh.devy.{name}");
    let out = Command::new("launchctl")
        .args(["list", &label])
        .output()
        .context("Failed to run launchctl list")?;
    if !out.status.success() {
        return Ok(false);
    }
    // Output contains "PID" = <num>; key is absent when service is stopped.
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.contains("\"PID\"") || stdout.contains("PID ="))
}

#[cfg(target_os = "macos")]
fn start_service_macos(
    name: &str,
    launch: &LaunchSpec,
    exec_dir: &Path,
    profile_bin: &Path,
) -> Result<()> {
    // Always rewrite and reload so port and config changes take effect.
    write_launchagent(name, launch, exec_dir, profile_bin)?;
    let plist = launchagent_path(name)?;
    let plist_str = plist
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 plist path"))?;
    if is_loaded_macos(name) {
        launchctl(&["unload", plist_str])?;
    }
    launchctl(&["load", plist_str])?;
    launchctl(&["start", &format!("sh.devy.{name}")])
}

#[cfg(target_os = "macos")]
fn stop_service_macos(name: &str) -> Result<()> {
    let label = format!("sh.devy.{name}");
    let _ = launchctl(&["stop", &label]); // best-effort; may not be running
    let plist = launchagent_path(name)?;
    if plist.exists() {
        let plist_str = plist
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 plist path"))?;
        launchctl(&["unload", plist_str])?;
    }
    Ok(())
}

// ── Service management — Linux (systemd user units) ───────────────────────────

/// Quotes one word for a systemd unit file: double-quoted with C-style escapes,
/// and `%` doubled so specifiers are not expanded. `$` is doubled too when `exec`,
/// since ExecStart expands `$VAR`.
#[cfg(any(test, target_os = "linux"))]
fn systemd_quote(s: &str, exec: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '%' => out.push_str("%%"),
            '$' if exec => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A systemd user unit that runs `program` with `env` in `working_dir`.
#[cfg(any(test, target_os = "linux"))]
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

#[cfg(target_os = "linux")]
fn systemd_user_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("$HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("systemd")
        .join("user"))
}

#[cfg(target_os = "linux")]
fn systemd_unit_path(name: &str) -> Result<PathBuf> {
    Ok(systemd_user_dir()?.join(format!("devy-{name}.service")))
}

#[cfg(target_os = "linux")]
fn write_systemd_unit(
    name: &str,
    launch: &LaunchSpec,
    exec_dir: &Path,
    profile_bin: &Path,
) -> Result<()> {
    let dir = systemd_user_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    let unit = systemd_unit(
        name,
        &program_arguments(launch, exec_dir),
        &unit_environment(launch, profile_bin),
        launch.working_dir.as_deref(),
    );
    let path = systemd_unit_path(name)?;
    std::fs::write(&path, unit).with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(target_os = "linux")]
fn systemctl_user(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("Failed to run systemctl")?;
    if !status.success() {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_running_linux(name: &str) -> Result<bool> {
    let unit = format!("devy-{name}");
    let status = Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", &unit])
        .status()
        .context("Failed to run systemctl is-active")?;
    Ok(status.success())
}

#[cfg(target_os = "linux")]
fn start_service_linux(
    name: &str,
    launch: &LaunchSpec,
    exec_dir: &Path,
    profile_bin: &Path,
) -> Result<()> {
    // Always rewrite and reload so port and config changes take effect.
    write_systemd_unit(name, launch, exec_dir, profile_bin)?;
    systemctl_user(&["daemon-reload"])?;
    systemctl_user(&["start", &format!("devy-{name}")])
}

#[cfg(target_os = "linux")]
fn stop_service_linux(name: &str) -> Result<()> {
    let unit = format!("devy-{name}");
    systemctl_user(&["stop", &unit])
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
        output::step(
            "Installing Nix via the Determinate Installer \
             (https://install.determinate.systems). \
             Transport is secured with TLS; install Nix manually: https://nixos.org/download/",
        );
        let status = Command::new("sh")
            .arg("-c")
            .arg(concat!(
                "curl --proto '=https' --tlsv1.2 --connect-timeout 30 --max-time 300 -sSf -L ",
                "https://install.determinate.systems/nix",
                " | sh -s -- install --no-confirm"
            ))
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("Failed to run Nix install script")?;

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
        // create the profile symlink (.devy/nix-profile → nix store path).
        if let Some(parent) = self.profile_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
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

    fn is_service_running(&self, name: &str) -> Result<bool> {
        #[cfg(target_os = "macos")]
        return is_running_macos(name);

        #[cfg(target_os = "linux")]
        return is_running_linux(name);

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        bail!("Service management is not supported on this platform with the nix backend");
    }

    fn start_service(&self, name: &str, launch: Option<&LaunchSpec>) -> Result<()> {
        let launch = launch.ok_or_else(|| unsupported_service(name))?;
        let profile_bin = self.profile_bin();
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

        #[cfg(target_os = "macos")]
        return start_service_macos(name, launch, &exec_dir, &profile_bin);

        #[cfg(target_os = "linux")]
        return start_service_linux(name, launch, &exec_dir, &profile_bin);

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        bail!("Service management is not supported on this platform with the nix backend");
    }

    fn stop_service(&self, name: &str) -> Result<()> {
        #[cfg(target_os = "macos")]
        return stop_service_macos(name);

        #[cfg(target_os = "linux")]
        return stop_service_linux(name);

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        bail!("Service management is not supported on this platform with the nix backend");
    }

    fn resolved_version(&self, dep: &Dependency) -> Result<Option<String>> {
        self.query_version(dep).map(Option::flatten)
    }

    fn log_source(&self, name: &str, lines: u32, follow: bool) -> Result<super::LogSource> {
        Ok(service_log_source(name, lines, follow))
    }

    /// Advertises the project-local Nix profile bin dir so shadowenv adds it to PATH.
    /// This ensures `devy up` activates the project's Nix packages without touching
    /// the user's global profile or requiring a manual PATH change.
    fn path_prepends(&self, _project_root: &Path) -> Vec<String> {
        vec![self.profile_bin().to_string_lossy().into_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::{LogCommandKind, LogSource};

    #[test]
    fn launchagent_log_path_is_in_tmpdir() {
        assert_eq!(
            launchagent_log_path("redis"),
            std::env::temp_dir().join("devy-redis.log")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_logs_are_the_launchagent_log_file() {
        // write_launchagent points StandardOutPath/StandardErrorPath at this same helper.
        let pm = NixPackageManager::for_project(&crate::test_support::tmp_dir());
        assert_eq!(
            pm.log_source("redis", 100, false).unwrap(),
            LogSource::Files(vec![launchagent_log_path("redis")])
        );
    }

    #[test]
    fn systemd_logs_read_the_user_journal() {
        for (follow, tail) in [(false, &[][..]), (true, &["-f"][..])] {
            let LogSource::Command(cmd) = systemd_log_source("redis", 30, follow) else {
                panic!("expected a command");
            };
            let mut expected = vec![
                "--user",
                "-u",
                "devy-redis.service",
                "-n",
                "30",
                "--no-pager",
                "-o",
                "cat",
            ];
            expected.extend(tail);
            assert_eq!(cmd.program, "journalctl");
            assert_eq!(cmd.args, expected);
            assert_eq!(cmd.kind, LogCommandKind::UserJournal);
        }
    }

    #[test]
    fn nix_name_is_nix() {
        let pm = NixPackageManager::for_project(&crate::test_support::tmp_dir());
        assert_eq!(pm.name(), "nix");
    }

    #[test]
    fn nix_is_available_when_nix_binary_found() {
        // is_available() returns true iff the resolved nix binary exists.
        // find_nix_binary checks PATH then standard locations, so this holds
        // even when Nix is installed but not on PATH.
        let pm = NixPackageManager::for_project(&crate::test_support::tmp_dir());
        let expected = find_nix_binary("nix").is_some();
        assert_eq!(pm.is_available(), expected);
    }

    #[test]
    fn profile_path_is_project_local() {
        let dir = crate::test_support::tmp_dir();
        let pm = NixPackageManager::for_project(&dir);
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
        let pm = NixPackageManager::for_project(&crate::test_support::tmp_dir());
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
        let pm = NixPackageManager::for_project(&crate::test_support::tmp_dir());
        let err = pm.start_service("someservice", None).unwrap_err();
        assert!(
            err.to_string()
                .contains("not yet supported with the nix backend"),
            "{err}"
        );
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
            &[("RABBITMQ_NODE_PORT".into(), "5 & 6".into())],
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
            &[("MINIO_ROOT_PASSWORD".into(), "p\"w 100%$".into())],
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

    fn pm_with_missing_profile() -> NixPackageManager {
        NixPackageManager {
            profile_path: PathBuf::from("/tmp/devy_test_nonexistent_profile_xyzzy"),
            style: std::sync::OnceLock::new(),
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
        let pm = NixPackageManager::for_project(&dir);
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
}
