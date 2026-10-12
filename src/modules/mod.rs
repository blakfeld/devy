pub(crate) mod helpers;

mod bun;
mod crystal;
mod dart;
mod deno;
mod dotnet;
mod elasticsearch;
mod elixir;
mod erlang;
mod gcloud;
mod generic;
mod java;
mod kafka;
mod kotlin;
mod loopback;
mod mailhog;
mod mariadb;
mod meilisearch;
mod memcached;
mod minio;
mod mongodb;
mod mysql;
mod nginx;
mod node;
mod opensearch;
mod postgres;
mod python;
mod rabbitmq;
mod redis;
mod ruby;
mod rust;
mod typescript;
mod vault;
mod zig;

use anyhow::{Context, Result};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::Dependency;
use crate::output;
use crate::package_manager::{LogSource, PackageManager};
pub(crate) use helpers::fnv1a;
#[cfg(test)]
use helpers::write_mysql_config;
use helpers::{PackageModule, extra_list, extra_port, node_pkg, pm_dep, run_cmd, tcp_ping};

/// How the setup step for `global_packages` (`npm install -g …`) ends, so `devy init`
/// can restore a removed step through the `global_packages` key that produced it.
pub(crate) const GLOBAL_PACKAGES_STEP: &str = "(global_packages install scripts)";

/// How the Nix backend runs a service under launchd or systemd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// Executable name inside `.devy/nix-profile/bin`.
    pub exec: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Runs once, before the first launch, while `marker` does not exist.
    pub init: Option<InitStep>,
    /// Working directory for the service and its init step. `nix_launch_for` defaults it
    /// to the data dir so tools with relative default paths write there.
    pub working_dir: Option<PathBuf>,
    /// nixpkgs attribute whose own `bin/` provides `exec` and the init command, instead
    /// of the merged profile `bin/`. Needed when another package in the profile wins a
    /// name conflict (mariadb's `bin/mysqld` is a symlink to `mariadbd`).
    pub exec_package: Option<String>,
    /// Directories copied, once, from the `exec_package`'s store path before init and
    /// launch, so servers that write into their config dir get a writable copy.
    pub seed_dirs: Vec<SeedDir>,
    /// Arguments appended only when a path exists in the `exec_package`'s store path,
    /// for settings the server rejects when the feature they configure isn't bundled.
    pub conditional_args: Vec<ConditionalArgs>,
    /// `(name, path)`: environment variables the nix backend sets to `path` inside the
    /// `exec_package`'s store path (the store path itself when `path` is empty).
    pub package_env: Vec<(String, String)>,
}

/// A package directory the nix backend copies into the project before first launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedDir {
    /// Path relative to the package's store root (e.g. `config`).
    pub from_package: String,
    /// Destination; copied only while it doesn't exist, so user edits are kept.
    pub to: PathBuf,
    /// Applied to the copy when it's first made, never on later starts.
    pub rewrites: Vec<SeedRewrite>,
}

/// A plain text replacement in one copied file; skipped when the file doesn't exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedRewrite {
    /// Path relative to the seeded directory (e.g. `jvm.options`).
    pub file: String,
    pub from: String,
    pub to: String,
}

/// Launch arguments that apply only when `package_path` exists in the package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionalArgs {
    /// Path relative to the package's store root (e.g. `plugins/opensearch-security`).
    pub package_path: String,
    pub args: Vec<String>,
}

impl LaunchSpec {
    pub fn new(exec: &str, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            exec: exec.to_string(),
            args: args.into_iter().collect(),
            env: Vec::new(),
            init: None,
            working_dir: None,
            exec_package: None,
            seed_dirs: Vec::new(),
            conditional_args: Vec::new(),
            package_env: Vec::new(),
        }
    }
}

/// How a service runs as a container under `service_manager: docker`. Built from the
/// same resolved `dep.extra` values as the module's `env_vars`, so the container's
/// settings and the exported variables agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerSpec {
    /// Image repository, e.g. `redis` or `docker.elastic.co/elasticsearch/elasticsearch`.
    pub image: String,
    /// Tag used when the dependency has no `version`.
    pub default_tag: String,
    /// Content digest (`sha256:<hex>`) of `default_tag` in `image`. When set, a dependency
    /// that sets neither `image` nor `version` runs `<image>@<digest>` instead of the
    /// mutable tag; `default_tag` stays the resolved version shown in status and the lock.
    pub default_digest: Option<&'static str>,
    /// Port the service listens on inside the container. Fixed per module; the resolved
    /// host port is published to it.
    pub container_port: u16,
    /// Further `(host, container)` ports to publish, e.g. MinIO's console.
    pub extra_ports: Vec<(u16, u16)>,
    /// Mount path of the service's named data volume; `None` for stateless services.
    pub data_path: Option<String>,
    pub env: Vec<(String, String)>,
    /// Credentials, passed through a mode-0600 `--env-file` instead of `-e` so they stay
    /// out of the `docker run` argv.
    pub secret_env: Vec<(String, String)>,
    /// Arguments after the image (the container's command).
    pub args: Vec<String>,
}

impl DockerSpec {
    pub fn new(image: &str, default_tag: &str, container_port: u16) -> Self {
        Self {
            image: image.to_string(),
            default_tag: default_tag.to_string(),
            default_digest: None,
            container_port,
            extra_ports: Vec::new(),
            data_path: None,
            env: Vec::new(),
            secret_env: Vec::new(),
            args: Vec::new(),
        }
    }

    /// Pins `default_tag` to `digest` (see [`DockerSpec::default_digest`]).
    fn pinned(mut self, digest: &'static str) -> Self {
        self.default_digest = Some(digest);
        self
    }

    fn data(mut self, path: &str) -> Self {
        self.data_path = Some(path.to_string());
        self
    }

    fn env(mut self, pairs: &[(&str, &str)]) -> Self {
        self.env
            .extend(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        self
    }

    fn secret_env(mut self, pairs: &[(&str, &str)]) -> Self {
        self.secret_env
            .extend(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        self
    }
}

/// An image repository and tag, e.g. `redis` and `7`, optionally pinned to a digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub repository: String,
    pub tag: String,
    /// The module's built-in digest for `tag` (`sha256:<hex>`), when it has one.
    pub digest: Option<String>,
}

impl ImageRef {
    /// `<repository>@<digest>` when pinned, otherwise `<repository>:<tag>`.
    pub fn reference(&self) -> String {
        match &self.digest {
            Some(digest) => format!("{}@{digest}", self.repository),
            None => format!("{}:{}", self.repository, self.tag),
        }
    }
}

/// Splits `image` into repository and tag. A `:` only starts a tag after the last `/`,
/// so a registry port (`registry:5000/redis`) isn't mistaken for one.
fn split_image_tag(image: &str) -> (&str, Option<&str>) {
    let name_start = image.rfind('/').map_or(0, |i| i + 1);
    match image[name_start..].rfind(':') {
        Some(i) => (&image[..name_start + i], Some(&image[name_start + i + 1..])),
        None => (image, None),
    }
}

/// The image a docker-managed `dep` runs. The repository is the `image` override, or
/// the module's image. The tag is a tag written in `image`, then `version`, then the
/// module's default tag. Only the module's own default (neither `image` nor `version`
/// set) uses the module's pinned digest.
pub(crate) fn docker_image(spec: &DockerSpec, dep: &Dependency) -> ImageRef {
    let (repository, image_tag) = match dep.image.as_deref() {
        Some(image) => split_image_tag(image),
        None => (spec.image.as_str(), None),
    };
    let tag = image_tag
        .or(dep.version.as_deref())
        .unwrap_or(&spec.default_tag);
    let digest = match (&dep.image, &dep.version) {
        (None, None) => spec.default_digest.map(String::from),
        _ => None,
    };
    ImageRef {
        repository: repository.to_string(),
        tag: tag.to_string(),
        digest,
    }
}

/// Warning for a `version` that a tag in `image` overrides.
pub(crate) fn docker_image_warning(dep: &Dependency) -> Option<String> {
    let (_, tag) = split_image_tag(dep.image.as_deref()?);
    let tag = tag?;
    let version = dep.version.as_deref()?;
    Some(format!(
        "image tag {tag} overrides version {version} — remove one of them"
    ))
}

/// Longest Unix socket path devy creates: macOS allows 103 bytes in `sun_path`, Linux
/// 107, and servers add suffixes like `.lock`.
const MAX_SOCKET_PATH: usize = 100;

/// Directory for a service's Unix sockets: `data_dir` when `<data_dir>/<socket_name>`
/// fits in `sun_path`, otherwise a short per-project directory inside devy's private
/// per-user directory (see `fs_safe::user_dir`). Deeply nested projects would otherwise
/// fail to start postgres or mysqld. The name is deterministic so it survives restarts;
/// a directory another user created there makes this fail instead of being used.
pub(crate) fn socket_dir(data_dir: &Path, socket_name: &str) -> Result<PathBuf> {
    if path_arg(data_dir).len() + 1 + socket_name.len() <= MAX_SOCKET_PATH {
        return Ok(data_dir.to_path_buf());
    }
    let user_dir = crate::fs_safe::user_dir()?;
    let dir = crate::fs_safe::private_dir(
        &user_dir,
        &format!("s-{:016x}", fnv1a(&path_arg(data_dir))),
        false,
    )?;
    // A long $XDG_RUNTIME_DIR/$TMPDIR can still exceed `sun_path`.
    if path_arg(&dir).len() + 1 + socket_name.len() > MAX_SOCKET_PATH {
        anyhow::bail!(
            "socket directory {} is too long for a Unix socket path — point XDG_RUNTIME_DIR or TMPDIR at a shorter directory",
            dir.display()
        );
    }
    Ok(dir)
}

/// One-time initialization of a service's data directory (e.g. `initdb`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitStep {
    /// Created by a successful init; init is skipped while it exists.
    pub marker: PathBuf,
    /// `cmd[0]` is an executable name inside `.devy/nix-profile/bin`.
    pub cmd: Vec<String>,
}

/// `<project_root>/.devy/data/<name>`: where nix-run services keep their state.
pub fn nix_data_dir(project_root: &Path, name: &str) -> PathBuf {
    project_root.join(".devy").join("data").join(name)
}

/// Prepares the nix launch spec for `dep`, creating its data dir. `None` on other backends.
pub(crate) fn nix_launch_for(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
    project_root: &Path,
) -> Result<Option<LaunchSpec>> {
    if pm.name() != "nix" {
        return Ok(None);
    }
    // `devy service start/restart` reach here without `up`'s managed-path check, and the
    // service would run on whatever state a repository committed under `.devy/data`.
    crate::fs_safe::check_managed_paths(project_root, &[])?;
    let data_dir = nix_data_dir(project_root, canonical_name(&dep.name));
    crate::fs_safe::ensure_devy_subdir(project_root, &data_dir)
        .with_context(|| format!("Failed to create {}", data_dir.display()))?;
    Ok(module.nix_launch(dep, &data_dir)?.map(|mut spec| {
        spec.working_dir.get_or_insert(data_dir);
        spec
    }))
}

/// Starts `dep` through `pm`, passing the nix launch spec when nix is the backend.
/// The shared body of every service module's `start`.
pub(crate) fn start_via_pm(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
    project_root: &Path,
) -> Result<()> {
    let launch = nix_launch_for(module, pm, dep, project_root)?;
    pm.start_service(&module.service_name(dep), launch.as_ref())
}

/// Launch spec shared by Elasticsearch and OpenSearch, which take the same `-E` settings.
/// Both write into their config dir (keystore, generated settings), so the nix backend
/// seeds a writable copy of the package's `config/` at `<data_dir>/config`, named by
/// `conf_env` (`ES_PATH_CONF` / `OPENSEARCH_PATH_CONF`). Creates `<data_dir>/logs`.
pub(crate) fn search_server_launch(
    exec: &str,
    attr: String,
    conf_env: &str,
    port: u16,
    data_dir: &Path,
) -> Result<LaunchSpec> {
    let config_dir = data_dir.join("config");
    let data = path_arg(&data_dir.join("data"));
    let logs_dir = data_dir.join("logs");
    let logs = path_arg(&logs_dir);
    // The JVM opens its GC log before the server creates `path.logs`.
    crate::fs_safe::ensure_dir_in(data_dir, &logs_dir)
        .with_context(|| format!("Failed to create {}", logs_dir.display()))?;
    // The package's start script runs from its store directory, so the stock
    // `jvm.options` paths relative to it (GC log, error file, heap dump) are read-only.
    let jvm_rewrite = |from: &str, to: String| SeedRewrite {
        file: "jvm.options".into(),
        from: from.into(),
        to,
    };
    Ok(LaunchSpec {
        env: vec![(conf_env.to_string(), path_arg(&config_dir))],
        exec_package: Some(attr),
        seed_dirs: vec![SeedDir {
            from_package: "config".into(),
            to: config_dir,
            rewrites: vec![
                jvm_rewrite("=logs/", format!("={logs}/")),
                jvm_rewrite(":logs/", format!(":{logs}/")),
                jvm_rewrite("-XX:HeapDumpPath=data", format!("-XX:HeapDumpPath={data}")),
            ],
        }],
        ..LaunchSpec::new(
            exec,
            [
                "-E".to_string(),
                "http.host=127.0.0.1".to_string(),
                "-E".to_string(),
                "transport.host=127.0.0.1".to_string(),
                "-E".to_string(),
                format!("http.port={port}"),
                "-E".to_string(),
                format!("path.data={data}"),
                "-E".to_string(),
                format!("path.logs={logs}"),
            ],
        )
    })
}

/// A path as a double-quoted string for nginx/HCL-style config files.
pub(crate) fn quoted_conf_path(path: &Path) -> String {
    let escaped = path_arg(path).replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// The nixpkgs attribute devy installs for `dep`: the module's versioned attribute when
/// `dep.version` maps to one, otherwise `unversioned`.
pub(crate) fn nix_install_attr(module: &dyn Module, dep: &Dependency, unversioned: &str) -> String {
    dep.version
        .as_deref()
        .and_then(|v| module.nix_versioned_attr(v))
        .unwrap_or_else(|| unversioned.to_string())
}

/// `pm_dep` for `name`, except that under nix the versioned attribute is installed when
/// `dep.version` maps to one, and unfree or insecure packages are allowed when the module
/// declares them.
pub(crate) fn pkg_dep(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
    name: &str,
) -> Dependency {
    if pm.name() == "nix" {
        Dependency {
            allow_unfree: module.nix_unfree(),
            allow_insecure: module.nix_insecure(),
            ..pm_dep(dep, &nix_install_attr(module, dep, name))
        }
    } else {
        pm_dep(dep, name)
    }
}

/// Installed check for `pkg_dep`. Nix installs attributes, not exact versions, so a
/// version pinned from devy.lock is also satisfied by the unversioned attribute when its
/// installed version maps to the same versioned attribute. The package an earlier
/// `devy up` installed (or a teammate's newer patch) then isn't reinstalled under its
/// versioned name.
pub(crate) fn pkg_installed(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
    name: &str,
) -> Result<bool> {
    let target = module
        .backend_package(pm, dep)
        .unwrap_or_else(|| pkg_dep(module, pm, dep, name));
    if pm.is_package_installed(&target)? {
        return Ok(true);
    }
    if dep.version_from_lock && target.name != name {
        let installed = pm.resolved_version(&pm_dep(dep, name))?;
        return Ok(installed
            .and_then(|v| module.nix_versioned_attr(&v))
            .is_some_and(|attr| attr == target.name));
    }
    Ok(false)
}

/// Whether `module`'s backend package for `dep` is installed.
pub(crate) fn backend_installed(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
) -> Result<bool> {
    pm.is_package_installed(&backend_package_of(module, pm, dep)?)
}

/// Installs `module`'s backend package for `dep`.
pub(crate) fn install_backend(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
) -> Result<()> {
    pm.install_package(&backend_package_of(module, pm, dep)?)
}

fn backend_package_of(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
) -> Result<Dependency> {
    module.backend_package(pm, dep).with_context(|| {
        format!(
            "{} is not installed through {} — this is a bug in devy",
            dep.name,
            pm.name()
        )
    })
}

/// Resolved version for `pkg_dep`, recorded in devy.lock. Under nix this is the version of
/// the attribute devy installs, falling back to the unversioned `name`; other backends
/// query the `devy.yml` name.
pub(crate) fn pkg_resolved_version(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
    name: &str,
) -> Result<Option<String>> {
    if pm.name() != "nix" {
        return pm.resolved_version(dep);
    }
    let target = pkg_dep(module, pm, dep, name);
    match pm.resolved_version(&target)? {
        Some(v) => Ok(Some(v)),
        None if target.name != name => pm.resolved_version(&pm_dep(dep, name)),
        None => Ok(None),
    }
}

/// Warning for a `devy.yml` version the nix backend can't honor. Versions pinned from
/// devy.lock, and modules that install outside the package manager, never warn.
pub(crate) fn nix_version_warning(dep: &Dependency, pm: &dyn PackageManager) -> Option<String> {
    // Docker-managed services use `version` as an image tag.
    if pm.name() != "nix" || dep.version_from_lock || dep.docker {
        return None;
    }
    let version = dep.version.as_deref()?;
    let module = get(&dep.name);
    if module.source().is_some() || module.nix_versioned_attr(version).is_some() {
        return None;
    }
    Some(format!(
        "version {version} is not supported by the nix backend — installing the nixpkgs default"
    ))
}

/// Path argument as an owned string for a launch spec.
pub(crate) fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub struct ServiceConfig {
    pub health_check_max_attempts: u32,
    pub health_check_sleep_ms: u64,
    pub shutdown_max_attempts: u32,
    pub shutdown_sleep_ms: u64,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            health_check_max_attempts: 10,
            health_check_sleep_ms: 500,
            shutdown_max_attempts: 10,
            shutdown_sleep_ms: 500,
        }
    }
}

pub trait Module: Sync {
    /// Whether this module manages a background service.
    fn is_service(&self) -> bool {
        false
    }

    /// The default TCP port this service listens on, if any.
    /// Used by `check_port_conflicts` to detect conflicts even when `port` is not
    /// explicitly set in devy.yml.
    fn default_port(&self) -> Option<u16> {
        None
    }

    /// The `dep.extra` key used to configure this service's port (e.g. `"port"`,
    /// `"smtp_port"`). `resolve_service_ports` writes a randomly-assigned port
    /// into `dep.extra` under this key so every downstream consumer (health checks,
    /// config-file writers, env vars) sees the correct value automatically.
    ///
    /// The default returns `Some("port")` for any service that has a `default_port`,
    /// and `None` for non-services or services whose port cannot be configured.
    /// Override when the module uses a key other than `"port"` (e.g. mailhog uses
    /// `"smtp_port"`).
    fn port_key(&self) -> Option<&'static str> {
        if self.is_service() && self.default_port().is_some() {
            Some("port")
        } else {
            None
        }
    }

    /// Whether `pm` can make this service listen on a port devy chooses. When false, port
    /// resolution falls back to `default_port` so exported env vars stay truthful.
    ///
    /// The default is true for every service under nix, where devy launches the process
    /// itself. brew, apt and winget run one machine-wide instance of each service, so a
    /// per-project port never applies there (see [`Module::explicit_port_via_config`]).
    fn port_applicable(&self, pm: &dyn PackageManager) -> bool {
        self.is_service() && pm.name() == "nix"
    }

    /// Whether `pm` applies an explicit port from devy.yml by writing it into the
    /// service's config file, although the port is not [`Module::port_applicable`]. Then
    /// no "cannot make … listen" warning is shown for it. The database modules override it
    /// for the backends whose config directory the server reads.
    fn explicit_port_via_config(&self, _pm: &dyn PackageManager) -> bool {
        false
    }

    /// Extra advice appended to the warning for an explicit port `pm` cannot apply (e.g.
    /// where to set it by hand).
    fn unapplied_port_hint(&self, _pm: &dyn PackageManager) -> Option<&'static str> {
        None
    }

    /// The install source recorded in devy.lock (e.g. "homebrew", "rustup").
    /// Return `None` to derive the source from the active package manager name.
    fn source(&self) -> Option<&'static str> {
        None
    }

    /// How `install` really gets the dependency onto the machine, for the executable
    /// entry listing (AI init/doctor review), given `backend`, the package manager as
    /// that listing names it (`nix`, `brew`, `sudo apt-get`, `winget`). Unlike [`Module::source`] (a stable lock key), it shows
    /// the route through the package manager, including `sudo`. The default: the module's
    /// own installer when it has one (rustup and the bun and deno installers always
    /// bypass the package manager), otherwise `backend`.
    fn install_route(&self, backend: &str) -> String {
        self.source().unwrap_or(backend).to_string()
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool>;
    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()>;

    /// The package `install` passes to `pm.install_package` for `dep`, or `None` when
    /// the dependency is not installed through the package manager (rustup, the deno and
    /// bun installers, rbenv). `install` and `is_installed` go through it, so the
    /// package found on disk (`PackageManager::package_bin_dir`) is the one installed.
    fn backend_package(&self, _pm: &dyn PackageManager, _dep: &Dependency) -> Option<Dependency> {
        None
    }

    /// The name used when managing this service via the package manager (start/stop/status).
    /// Override when the PM service name differs from the dependency name in devy.yml.
    fn service_name<'a>(&self, dep: &'a Dependency) -> Cow<'a, str> {
        Cow::Borrowed(&dep.name)
    }

    /// How the Nix backend launches this service: the binary from the project profile,
    /// its arguments and environment, and any one-time initialization. `data_dir` is
    /// `<project_root>/.devy/data/<canonical-name>`, already created. Modules that need a
    /// generated config file write it here before returning.
    ///
    /// Returns `None` if Nix-backed service management is not supported for this service.
    fn nix_launch(&self, _dep: &Dependency, _data_dir: &Path) -> Result<Option<LaunchSpec>> {
        Ok(None)
    }

    /// How this service runs as a container under `service_manager: docker`: its image,
    /// container port, data volume path, environment and arguments. `dep` has its port
    /// already resolved; that is the host port, not the container's.
    ///
    /// Returns `None` for modules that can't run in a container (every non-service).
    fn docker_spec(&self, _dep: &Dependency) -> Result<Option<DockerSpec>> {
        Ok(None)
    }

    /// Warnings about running `dep` as a container (e.g. a setting docker ignores).
    fn docker_warnings(&self, _dep: &Dependency) -> Vec<String> {
        vec![]
    }

    /// Whether `post_setup` only writes the package manager's service config (e.g. a
    /// conf.d file setting the port). Docker-managed services skip such a `post_setup`.
    fn post_setup_writes_service_config(&self) -> bool {
        false
    }

    /// The versioned nixpkgs attribute for `version` (e.g. `"22.11.0"` → `"nodejs_22"`).
    /// Full versions map like their short form. Returns `None` when the module has no
    /// versioned attributes or the version isn't in its allowlist of attributes
    /// nixpkgs carries, so the unversioned attribute is installed instead.
    fn nix_versioned_attr(&self, _version: &str) -> Option<String> {
        None
    }

    /// The nixpkgs attribute name for this module (e.g. `"redis"`, `"nodejs"`).
    /// Used by `devy export` to generate correct `pkgs.<attr>` entries.
    /// Returns `None` for modules with no known nixpkgs equivalent.
    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        None
    }

    /// Whether this module's nixpkgs package is unfree. The nix backend then allows
    /// unfree packages for that one install, and `devy export` allowlists it.
    fn nix_unfree(&self) -> bool {
        false
    }

    /// Whether nixpkgs marks this module's package insecure. The nix backend then
    /// allows insecure packages for that one install, and `devy export` allowlists it.
    fn nix_insecure(&self) -> bool {
        false
    }

    fn is_running(&self, _pm: &dyn PackageManager, _dep: &Dependency) -> Result<bool> {
        Ok(false)
    }

    fn start(
        &self,
        _pm: &dyn PackageManager,
        _dep: &Dependency,
        _project_root: &Path,
    ) -> Result<()> {
        Ok(())
    }

    fn stop(&self, _pm: &dyn PackageManager, _dep: &Dependency) -> Result<()> {
        Ok(())
    }

    /// Where the package manager keeps this service's logs, looked up by its backend
    /// service name.
    fn log_source(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
        lines: u32,
        follow: bool,
    ) -> Result<LogSource> {
        pm.log_source(&self.service_name(dep), lines, follow)
    }

    /// Log files or directories this service writes itself under its nix `data_dir`
    /// (`<project_root>/.devy/data/<canonical-name>`), shown by `devy logs` as hints.
    fn extra_log_paths(&self, _data_dir: &Path) -> Vec<PathBuf> {
        vec![]
    }

    /// Probes the service directly to confirm it is accepting connections.
    /// Override in service modules; default always passes.
    fn health_check(&self, _dep: &Dependency) -> Result<()> {
        Ok(())
    }

    /// Timing and attempt configuration for health-check and shutdown polling.
    /// Override `service_config()` for slow-starting or slow-stopping services.
    fn service_config(&self) -> ServiceConfig {
        ServiceConfig::default()
    }

    /// Polls `health_check` until the service is ready or attempts are exhausted.
    fn wait_for_ready(&self, dep: &Dependency) -> Result<()> {
        let cfg = self.service_config();
        let max = cfg.health_check_max_attempts;
        if max == 0 {
            anyhow::bail!(
                "health_check_max_attempts returned 0 for '{}'; must be > 0",
                dep.name
            );
        }
        let sleep_ms = cfg.health_check_sleep_ms;
        let mut last_err = anyhow::anyhow!("{} health check produced no error", dep.name);
        for attempt in 1..=max {
            match self.health_check(dep) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = e;
                    if attempt < max {
                        if attempt % 10 == 0 {
                            output::step(&format!(
                                "Still waiting for {} ({}/{})",
                                dep.name, attempt, max
                            ));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                    }
                }
            }
        }
        Err(last_err)
            .with_context(|| format!("{} did not become healthy after {max} attempts", dep.name))
    }

    /// Polls `is_running` until the service has stopped or attempts are exhausted.
    fn wait_for_stopped(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        let cfg = self.service_config();
        let max = cfg.shutdown_max_attempts;
        if max == 0 {
            anyhow::bail!(
                "shutdown_max_attempts returned 0 for '{}'; must be > 0",
                dep.name
            );
        }
        let sleep_ms = cfg.shutdown_sleep_ms;
        for attempt in 1..=max {
            if !self.is_running(pm, dep)? {
                return Ok(());
            }
            if attempt < max {
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            }
        }
        anyhow::bail!(
            "{} did not stop after {} attempts — try stopping it manually or run devy logs {}",
            dep.name,
            max,
            dep.name
        )
    }

    /// Environment variables this module injects when active (e.g. SMTP_HOST, VAULT_ADDR).
    /// User-configured vars in devy.yml always take precedence over these defaults.
    fn env_vars(
        &self,
        _dep: &Dependency,
        _project_root: &std::path::Path,
    ) -> HashMap<String, String> {
        HashMap::new()
    }

    /// Environment variables that depend on the backend running the service, added after
    /// `env_vars`. Used for values devy chooses at launch time (MinIO's console port
    /// under nix), so the exported value matches what the service was started with.
    /// Only `PortMode::Assign` (`devy up`) may choose and record such a value; under
    /// `PortMode::ReadOnly` nothing is written.
    fn backend_env_vars(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        _project_root: &std::path::Path,
        _mode: crate::commands::ports::PortMode,
    ) -> HashMap<String, String> {
        HashMap::new()
    }

    /// PATH entries to prepend when this module is active.
    /// Emitted as shadowenv `env/prepend-to-pathlist` directives so they compose
    /// correctly with the user's existing PATH.
    fn path_prepends(&self, _dep: &Dependency, _project_root: &std::path::Path) -> Vec<String> {
        vec![]
    }

    /// PATH entries that depend on the backend that installed the dependency, added
    /// after `path_prepends` (Java's `$JAVA_HOME/bin` for the JDK brew or nix installed).
    fn backend_path_prepends(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        _project_root: &std::path::Path,
    ) -> Vec<String> {
        vec![]
    }

    /// The set of keys this module reads from `dep.extra`.
    ///
    /// `None` skips key checking (e.g. GenericModule accepts any key).
    /// `Some(&[])` warns on any extra key (the default — correct for modules with no config).
    /// `Some(&["port", ...])` declares a known-key allowlist.
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&[])
    }

    /// The commands `post_setup` runs because of files in the project (package-manager
    /// installs that run lifecycle scripts, repository-provided scripts, build scripts),
    /// each as `<command> (<what drives it>)`, for the executable entry listing (AI
    /// init/doctor review). Empty when this module runs none for the project at
    /// `project_root`.
    fn setup_steps(&self, _dep: &Dependency, _project_root: &Path) -> Vec<String> {
        Vec::new()
    }

    /// Called unconditionally after a dependency is installed or confirmed installed.
    /// Implementations should be idempotent.
    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        _project_root: &std::path::Path,
    ) -> Result<()> {
        Ok(())
    }

    /// Returns the exact version string currently installed.
    /// Default delegates to the package manager; override for non-brew sources.
    fn resolved_version(
        &self,
        pm: &dyn PackageManager,
        dep: &Dependency,
    ) -> Result<Option<String>> {
        pm.resolved_version(dep)
    }

    /// Returns informational warnings about the dependency's configuration.
    /// Called by `devy check` before any installation to surface issues early.
    /// Warnings are printed but do not count as blocking issues.
    fn config_warnings(&self, _dep: &Dependency) -> Vec<String> {
        vec![]
    }

    /// Warnings about how `pm` applies the dependency's configuration (e.g. a Homebrew
    /// `my.cnf` that doesn't include devy's settings). Called by `devy check`; must only
    /// read, never write.
    fn backend_config_warnings(&self, _dep: &Dependency, _pm: &dyn PackageManager) -> Vec<String> {
        vec![]
    }

    /// Blocking problems in the dependency's configuration that `up` would fail on (an
    /// invalid secondary port, a credential that can't be passed safely). `devy check`
    /// counts them as issues. Messages omit the dependency name.
    fn config_issues(&self, _dep: &Dependency) -> Vec<String> {
        vec![]
    }
}

// ── Module statics ─────────────────────────────────────────────────────────────
// Service modules
static MYSQL: mysql::MysqlModule = mysql::MysqlModule;
static REDIS: redis::RedisModule = redis::RedisModule;
static POSTGRES: postgres::PostgresModule = postgres::PostgresModule;
static MONGODB: mongodb::MongodbModule = mongodb::MongodbModule;
static NGINX: nginx::NginxModule = nginx::NginxModule;
static KAFKA: kafka::KafkaModule = kafka::KafkaModule;
static RABBITMQ: rabbitmq::RabbitmqModule = rabbitmq::RabbitmqModule;
static MEMCACHED: memcached::MemcachedModule = memcached::MemcachedModule;
static ELASTICSEARCH: elasticsearch::ElasticsearchModule = elasticsearch::ElasticsearchModule;
static OPENSEARCH: opensearch::OpenSearchModule = opensearch::OpenSearchModule;
static MEILISEARCH: meilisearch::MeilisearchModule = meilisearch::MeilisearchModule;
static MINIO: minio::MinioModule = minio::MinioModule;
static MAILHOG: mailhog::MailhogModule = mailhog::MailhogModule;
static MARIADB: mariadb::MariadbModule = mariadb::MariadbModule;
static VAULT: vault::VaultModule = vault::VaultModule;
// Language / runtime modules
static RUST: rust::RustModule = rust::RustModule;
static NODE: node::NodeModule = node::NodeModule;
static TYPESCRIPT: typescript::TypeScriptModule = typescript::TypeScriptModule;
static RUBY: ruby::RubyModule = ruby::RubyModule;
static PYTHON: python::PythonModule = python::PythonModule;
static JAVA: java::JavaModule = java::JavaModule;
static KOTLIN: kotlin::KotlinModule = kotlin::KotlinModule;
static ELIXIR: elixir::ElixirModule = elixir::ElixirModule;
static ERLANG: erlang::ErlangModule = erlang::ErlangModule;
static DENO: deno::DenoModule = deno::DenoModule;
static BUN: bun::BunModule = bun::BunModule;
static DOTNET: dotnet::DotnetModule = dotnet::DotnetModule;
static DART: dart::DartModule = dart::DartModule;
static ZIG: zig::ZigModule = zig::ZigModule;
static CRYSTAL: crystal::CrystalModule = crystal::CrystalModule;
static GCLOUD: gcloud::GcloudModule = gcloud::GcloudModule;
static GENERIC: generic::GenericModule = generic::GenericModule;
// Package modules (per-PM name tables)
static GO: PackageModule = PackageModule {
    default: "go",
    apt: "golang-go",
    winget: "GoLang.Go",
    nix: "go",
    nix_versioned: go_nix_versioned,
    nix_unfree: false,
};

/// `1.26` or `1.26.3` → `go_1_26`, for Go releases nixpkgs carries.
fn go_nix_versioned(version: &str) -> Option<String> {
    helpers::allowlisted_attr(version, 2, &["1.26"], |v| {
        format!("go_{}", v.replace('.', "_"))
    })
}
static SCALA: PackageModule = PackageModule {
    default: "scala",
    apt: "scala",
    winget: "EPFL.Scala",
    nix: "scala",
    nix_versioned: helpers::no_nix_versions,
    nix_unfree: false,
};
static PHP: PackageModule = PackageModule {
    default: "php",
    apt: "php",
    winget: "PHP.PHP",
    nix: "php",
    nix_versioned: helpers::no_nix_versions,
    nix_unfree: false,
};
static AWSCLI: PackageModule = PackageModule {
    default: "awscli",
    apt: "awscli",
    winget: "Amazon.AWSCLI",
    nix: "awscli2",
    nix_versioned: helpers::no_nix_versions,
    nix_unfree: false,
};
static GH: PackageModule = PackageModule {
    default: "gh",
    apt: "gh",
    winget: "GitHub.cli",
    nix: "gh",
    nix_versioned: helpers::no_nix_versions,
    nix_unfree: false,
};
static KUBECTL: PackageModule = PackageModule {
    default: "kubectl",
    apt: "kubectl",
    winget: "Kubernetes.kubectl",
    nix: "kubectl",
    nix_versioned: helpers::no_nix_versions,
    nix_unfree: false,
};
static HELM: PackageModule = PackageModule {
    default: "helm",
    apt: "helm",
    winget: "Helm.Helm",
    nix: "kubernetes-helm",
    nix_versioned: helpers::no_nix_versions,
    nix_unfree: false,
};
static TERRAFORM: PackageModule = PackageModule {
    default: "terraform",
    apt: "terraform",
    winget: "Hashicorp.Terraform",
    nix: "terraform",
    nix_versioned: helpers::no_nix_versions,
    nix_unfree: true,
};
static AZURE_CLI: PackageModule = PackageModule {
    default: "azure-cli",
    apt: "azure-cli",
    winget: "Microsoft.AzureCLI",
    nix: "azure-cli",
    nix_versioned: helpers::no_nix_versions,
    nix_unfree: false,
};
static SWIFT: PackageModule = PackageModule {
    default: "swift",
    apt: "swift",
    winget: "Swift.Toolchain",
    nix: "swift",
    nix_versioned: helpers::no_nix_versions,
    nix_unfree: false,
};

/// Canonical-name → module registry. One entry per canonical name.
/// Aliases (e.g. "postgres" → "postgresql") live in ALIASES below.
/// Add a module here; adding it anywhere else is not required.
pub(crate) static REGISTRY: &[(&str, &dyn Module)] = &[
    // Services
    ("mysql", &MYSQL),
    ("redis", &REDIS),
    ("postgresql", &POSTGRES),
    ("mongodb", &MONGODB),
    ("nginx", &NGINX),
    ("kafka", &KAFKA),
    ("rabbitmq", &RABBITMQ),
    ("memcached", &MEMCACHED),
    ("elasticsearch", &ELASTICSEARCH),
    ("opensearch", &OPENSEARCH),
    ("meilisearch", &MEILISEARCH),
    ("minio", &MINIO),
    ("mailhog", &MAILHOG),
    ("mariadb", &MARIADB),
    ("vault", &VAULT),
    // Languages / runtimes
    ("rust", &RUST),
    ("node", &NODE),
    ("typescript", &TYPESCRIPT),
    ("ruby", &RUBY),
    ("python", &PYTHON),
    ("go", &GO),
    ("java", &JAVA),
    ("kotlin", &KOTLIN),
    ("scala", &SCALA),
    ("php", &PHP),
    ("elixir", &ELIXIR),
    ("erlang", &ERLANG),
    ("deno", &DENO),
    ("bun", &BUN),
    ("dotnet", &DOTNET),
    ("dart", &DART),
    ("zig", &ZIG),
    ("crystal", &CRYSTAL),
    // CLI / infrastructure tools
    ("awscli", &AWSCLI),
    ("gh", &GH),
    ("gcloud", &GCLOUD),
    ("kubectl", &KUBECTL),
    ("helm", &HELM),
    ("terraform", &TERRAFORM),
    ("azure-cli", &AZURE_CLI),
    ("swift", &SWIFT),
];

/// Alias → canonical name. The canonical name must exist in REGISTRY.
static ALIASES: &[(&str, &str)] = &[
    // Service aliases
    ("postgres", "postgresql"),
    ("mongo", "mongodb"),
    ("elastic", "elasticsearch"),
    ("meili", "meilisearch"),
    ("hashicorp-vault", "vault"),
    // Language / runtime aliases
    ("rustup", "rust"),
    ("nodejs", "node"),
    ("javascript", "node"),
    ("js", "node"),
    ("ts", "typescript"),
    ("python3", "python"),
    ("golang", "go"),
    ("openjdk", "java"),
    ("dotnet-sdk", "dotnet"),
    ("csharp", "dotnet"),
    // CLI aliases
    ("aws", "awscli"),
    ("aws-cli", "awscli"),
    ("github-cli", "gh"),
    ("google-cloud-sdk", "gcloud"),
    ("kubernetes-cli", "kubectl"),
    ("az", "azure-cli"),
];

/// Resolves a dependency name to its module, falling back to a generic install.
static REGISTRY_MAP: std::sync::LazyLock<HashMap<&'static str, &'static dyn Module>> =
    std::sync::LazyLock::new(|| REGISTRY.iter().copied().collect());

static ALIASES_MAP: std::sync::LazyLock<HashMap<&'static str, &'static str>> =
    std::sync::LazyLock::new(|| ALIASES.iter().map(|&(a, t)| (a, t)).collect());

pub fn get(name: &str) -> &'static dyn Module {
    let canonical = ALIASES_MAP.get(name).copied().unwrap_or(name);
    REGISTRY_MAP.get(canonical).copied().unwrap_or(&GENERIC)
}

/// Whether `name` (or the alias it resolves to) has its own module rather than the
/// generic fallback.
pub fn is_registered(name: &str) -> bool {
    REGISTRY_MAP.contains_key(canonical_name(name))
}

/// Returns the canonical registry name for `name`, resolving aliases.
/// `"postgres"` → `"postgresql"`, `"js"` → `"node"`, unknown → unchanged.
pub fn canonical_name(name: &str) -> &str {
    ALIASES_MAP.get(name).copied().unwrap_or(name)
}

/// The project-relative virtualenv directories devy manages for `deps` (one per
/// `python` dependency), for the managed-path check.
pub(crate) fn managed_venvs(deps: &[Dependency]) -> Vec<PathBuf> {
    deps.iter()
        .filter(|d| !d.docker && canonical_name(&d.name) == "python")
        .map(python::venv_rel_path)
        .collect()
}

/// One registry entry as described to an AI model: everything needed to write a valid
/// `dependencies:` item and to reference the variables a service injects.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogEntry {
    pub name: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<&'static str>,
    pub service: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_port: Option<u16>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// `None` when the module accepts any extra key.
    pub extra_keys: Option<&'static [&'static str]>,
}

/// The module catalog, generated from `REGISTRY` and `ALIASES` so it cannot drift.
pub fn catalog() -> Vec<CatalogEntry> {
    REGISTRY
        .iter()
        .map(|&(name, module)| {
            let service = module.is_service();
            let env = if service {
                let prefix = name.replace('-', "_").to_ascii_uppercase();
                let mut env = vec![format!("{prefix}_HOST")];
                if module.default_port().is_some() || module.port_key().is_some() {
                    env.push(format!("{prefix}_PORT"));
                }
                env
            } else {
                vec![]
            };
            CatalogEntry {
                name,
                aliases: ALIASES
                    .iter()
                    .filter(|&&(_, canon)| canon == name)
                    .map(|&(alias, _)| alias)
                    .collect(),
                service,
                default_port: module.default_port(),
                env,
                extra_keys: module.known_extra_keys(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── logs ──────────────────────────────────────────────────────────────────

    #[test]
    fn log_source_uses_the_backend_service_name() {
        let pm = crate::package_manager::MockPackageManager {
            log_source_result: Some(LogSource::Files(vec![PathBuf::from("/tmp/x.log")])),
            ..Default::default()
        };
        let source = get("postgres")
            .log_source(&pm, &Dependency::simple("postgres"), 20, true)
            .unwrap();
        assert_eq!(source, LogSource::Files(vec![PathBuf::from("/tmp/x.log")]));
        assert_eq!(
            *pm.log_queries.borrow(),
            [("postgresql".to_string(), 20, true)]
        );
    }

    #[test]
    fn extra_log_paths_default_is_empty() {
        assert!(get("redis").extra_log_paths(Path::new("/d")).is_empty());
    }

    // ── catalog ───────────────────────────────────────────────────────────────

    #[test]
    fn catalog_lists_every_registry_name_and_alias() {
        let catalog = catalog();
        let json = serde_json::to_string(&catalog).unwrap();
        assert!(!json.contains('\n'), "catalog JSON must be compact");
        for (name, _) in REGISTRY {
            assert!(catalog.iter().any(|e| e.name == *name), "missing {name}");
        }
        for (alias, canon) in ALIASES {
            let entry = catalog.iter().find(|e| e.name == *canon).unwrap();
            assert!(entry.aliases.contains(alias), "missing alias {alias}");
        }
    }

    #[test]
    fn catalog_lists_injected_service_vars() {
        let catalog = catalog();
        let pg = catalog.iter().find(|e| e.name == "postgresql").unwrap();
        assert!(pg.service);
        assert_eq!(pg.default_port, Some(5432));
        assert!(pg.env.contains(&"POSTGRESQL_HOST".to_string()));
        assert!(pg.env.contains(&"POSTGRESQL_PORT".to_string()));
        let node = catalog.iter().find(|e| e.name == "node").unwrap();
        assert!(!node.service && node.env.is_empty());
    }

    // ── registry integrity ────────────────────────────────────────────────────

    #[test]
    fn registry_names_are_unique() {
        let mut names: Vec<&str> = REGISTRY.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        for w in names.windows(2) {
            assert_ne!(w[0], w[1], "REGISTRY contains duplicate name '{}'", w[0]);
        }
    }

    #[test]
    fn alias_targets_exist_in_registry() {
        let canonical_names: std::collections::HashSet<&str> =
            REGISTRY.iter().map(|(n, _)| *n).collect();
        for (alias, canon) in ALIASES {
            assert!(
                canonical_names.contains(canon),
                "ALIASES: '{}' → '{}' but '{}' is not in REGISTRY",
                alias,
                canon,
                canon
            );
        }
    }

    #[test]
    fn alias_names_do_not_shadow_registry_names() {
        let canonical_names: std::collections::HashSet<&str> =
            REGISTRY.iter().map(|(n, _)| *n).collect();
        for (alias, _) in ALIASES {
            assert!(
                !canonical_names.contains(alias),
                "ALIASES entry '{}' shadows a REGISTRY canonical name",
                alias
            );
        }
    }

    // ── nix launch definitions ───────────────────────────────────────────────

    fn with_extra(name: &str, pairs: &[(&str, crate::config::ExtraValue)]) -> Dependency {
        Dependency::with_extra(
            name,
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn num(n: u64) -> crate::config::ExtraValue {
        crate::config::ExtraValue::Number(n.into())
    }

    fn launch(name: &str, dep: &Dependency, data: &Path) -> LaunchSpec {
        get(name)
            .nix_launch(dep, data)
            .unwrap()
            .unwrap_or_else(|| panic!("{name} must have a nix launch definition"))
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // ── docker specs ─────────────────────────────────────────────────────────

    fn dspec(name: &str, dep: &Dependency) -> DockerSpec {
        get(name)
            .docker_spec(dep)
            .unwrap()
            .unwrap_or_else(|| panic!("{name} must have a docker spec"))
    }

    fn env_of<'a>(spec: &'a DockerSpec, key: &str) -> Option<&'a str> {
        spec.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn image_dep(name: &str, version: Option<&str>, image: Option<&str>) -> Dependency {
        Dependency {
            version: version.map(String::from),
            image: image.map(String::from),
            docker: true,
            ..Dependency::simple(name)
        }
    }

    fn image_of(name: &str, version: Option<&str>, image: Option<&str>) -> String {
        let dep = image_dep(name, version, image);
        docker_image(&dspec(name, &dep), &dep).reference()
    }

    #[test]
    fn docker_image_tag_resolution() {
        assert_eq!(image_of("redis", None, None), "redis:7");
        assert_eq!(image_of("redis", Some("7.2"), None), "redis:7.2");
        assert_eq!(
            image_of("redis", None, Some("registry.corp.example/mirror/redis")),
            "registry.corp.example/mirror/redis:7"
        );
        assert_eq!(
            image_of(
                "redis",
                Some("7.2"),
                Some("registry.corp.example/mirror/redis")
            ),
            "registry.corp.example/mirror/redis:7.2"
        );
        // A registry port is not a tag.
        assert_eq!(
            image_of("redis", None, Some("registry:5000/redis")),
            "registry:5000/redis:7"
        );
    }

    #[test]
    fn docker_image_tag_in_image_wins_over_version_with_warning() {
        let dep = image_dep(
            "redis",
            Some("7.2"),
            Some("registry.corp.example/mirror/redis:7"),
        );
        let image = docker_image(&dspec("redis", &dep), &dep);
        assert_eq!(image.repository, "registry.corp.example/mirror/redis");
        assert_eq!(image.tag, "7");
        assert_eq!(
            docker_image_warning(&dep).as_deref(),
            Some("image tag 7 overrides version 7.2 — remove one of them")
        );
        assert_eq!(
            docker_image_warning(&image_dep("redis", None, Some("redis:7"))),
            None
        );
        assert_eq!(
            docker_image_warning(&image_dep("redis", Some("7"), Some("registry:5000/redis"))),
            None
        );
    }

    #[test]
    fn postgres_docker_spec_trusts_local_connections() {
        let spec = dspec("postgres", &Dependency::simple("postgres"));
        assert_eq!(spec.image, "postgres");
        assert_eq!(spec.default_tag, "16");
        assert_eq!(spec.container_port, 5432);
        assert_eq!(spec.data_path.as_deref(), Some("/var/lib/postgresql/data"));
        assert_eq!(env_of(&spec, "POSTGRES_HOST_AUTH_METHOD"), Some("trust"));
        assert!(env_of(&spec, "POSTGRES_USER").is_some_and(|u| !u.is_empty()));
        assert!(env_of(&spec, "POSTGRES_PASSWORD").is_none());
        assert!(spec.args.is_empty());
    }

    #[test]
    fn mysql_family_docker_specs_allow_empty_password_and_sanitize_cli_args() {
        let dep = with_extra(
            "mysql",
            &[(
                "cli_args",
                crate::config::ExtraValue::String(
                    "--max-connections=50 bad=1 --sql_mode=ANSI".into(),
                ),
            )],
        );
        let mysql = dspec("mysql", &dep);
        assert_eq!(
            (mysql.image.as_str(), mysql.default_tag.as_str()),
            ("mysql", "8.0")
        );
        assert_eq!(mysql.container_port, 3306);
        assert_eq!(mysql.data_path.as_deref(), Some("/var/lib/mysql"));
        assert_eq!(env_of(&mysql, "MYSQL_ALLOW_EMPTY_PASSWORD"), Some("yes"));
        assert_eq!(mysql.args, s(&["--max-connections=50", "--sql-mode=ANSI"]));

        let dep = Dependency {
            name: "mariadb".into(),
            ..dep
        };
        let mariadb = dspec("mariadb", &dep);
        assert_eq!(
            (mariadb.image.as_str(), mariadb.default_tag.as_str()),
            ("mariadb", "11")
        );
        assert_eq!(mariadb.container_port, 3306);
        assert_eq!(mariadb.data_path.as_deref(), Some("/var/lib/mysql"));
        assert_eq!(
            env_of(&mariadb, "MARIADB_ALLOW_EMPTY_ROOT_PASSWORD"),
            Some("1")
        );
        assert_eq!(
            mariadb.args,
            s(&["--max-connections=50", "--sql-mode=ANSI"])
        );
    }

    #[test]
    fn simple_service_docker_specs() {
        // (module, image, tag, container port, data path)
        let cases: &[(&str, &str, &str, u16, Option<&str>)] = &[
            ("redis", "redis", "7", 6379, Some("/data")),
            ("mongodb", "mongo", "7", 27017, Some("/data/db")),
            ("rabbitmq", "rabbitmq", "3", 5672, Some("/var/lib/rabbitmq")),
            ("memcached", "memcached", "1", 11211, None),
            ("nginx", "nginx", "1.30.5", 80, None),
            ("mailhog", "mailhog/mailhog", "v1.0.1", 1025, None),
            (
                "meilisearch",
                "getmeili/meilisearch",
                "v1.8",
                7700,
                Some("/meili_data"),
            ),
        ];
        for &(name, image, tag, port, data) in cases {
            let spec = dspec(name, &Dependency::simple(name));
            assert_eq!(spec.image, image, "{name}");
            assert_eq!(spec.default_tag, tag, "{name}");
            assert_eq!(spec.container_port, port, "{name}");
            assert_eq!(spec.data_path.as_deref(), data, "{name}");
            assert!(spec.env.is_empty(), "{name}: {:?}", spec.env);
            assert!(
                spec.args.is_empty() && spec.extra_ports.is_empty(),
                "{name}"
            );
        }
    }

    /// Only rejects `latest`; tags like `redis:7` follow the spec's version table.
    #[test]
    fn no_docker_default_tag_is_latest() {
        for (name, module) in REGISTRY {
            if let Some(spec) = module.docker_spec(&Dependency::simple(name)).unwrap() {
                assert_ne!(spec.default_tag, "latest", "{name}");
                assert!(!spec.default_tag.is_empty(), "{name}");
            }
        }
    }

    #[test]
    fn meilisearch_docker_spec_passes_master_key() {
        let dep = with_extra(
            "meilisearch",
            &[(
                "master_key",
                crate::config::ExtraValue::String("s3cret".into()),
            )],
        );
        let spec = dspec("meilisearch", &dep);
        assert_eq!(env_of(&spec, "MEILI_MASTER_KEY"), None);
        assert_eq!(
            spec.secret_env,
            [("MEILI_MASTER_KEY".to_string(), "s3cret".to_string())]
        );
    }

    #[test]
    fn search_server_docker_specs_run_single_node_without_security() {
        let es = dspec("elasticsearch", &Dependency::simple("elasticsearch"));
        assert_eq!(es.image, "docker.elastic.co/elasticsearch/elasticsearch");
        assert_eq!(es.default_tag, "8.13.4");
        assert_eq!(es.container_port, 9200);
        assert_eq!(
            es.data_path.as_deref(),
            Some("/usr/share/elasticsearch/data")
        );
        assert_eq!(env_of(&es, "discovery.type"), Some("single-node"));
        assert_eq!(env_of(&es, "xpack.security.enabled"), Some("false"));
        assert_eq!(env_of(&es, "ES_JAVA_OPTS"), Some("-Xms512m -Xmx512m"));

        let os = dspec("opensearch", &Dependency::simple("opensearch"));
        assert_eq!(os.image, "opensearchproject/opensearch");
        assert_eq!(os.default_tag, "2");
        assert_eq!(os.container_port, 9200);
        assert_eq!(os.data_path.as_deref(), Some("/usr/share/opensearch/data"));
        assert_eq!(env_of(&os, "discovery.type"), Some("single-node"));
        assert_eq!(env_of(&os, "DISABLE_SECURITY_PLUGIN"), Some("true"));
    }

    #[test]
    fn minio_docker_spec_console_and_credentials() {
        let plain = dspec("minio", &Dependency::simple("minio"));
        assert_eq!(
            (
                plain.image.as_str(),
                plain.default_tag.as_str(),
                plain.default_digest
            ),
            (
                "pgsty/silo",
                "RELEASE.2026-09-16T00-00-00Z",
                Some("sha256:635197cb9f36d01bee221d34d1c7d7960f6a95c48b0b6c01d99cd13bdae51a46")
            )
        );
        assert_eq!(plain.container_port, 9000);
        assert_eq!(plain.data_path.as_deref(), Some("/data"));
        assert_eq!(
            plain.args,
            s(&["server", "/data", "--console-address", ":9001"])
        );
        assert!(plain.extra_ports.is_empty());
        assert!(plain.env.is_empty());
        assert!(plain.secret_env.is_empty());

        let dep = with_extra(
            "minio",
            &[
                ("console_port", num(9101)),
                ("access_key", crate::config::ExtraValue::String("me".into())),
                ("secret_key", crate::config::ExtraValue::String("pw".into())),
            ],
        );
        let spec = dspec("minio", &dep);
        assert_eq!(spec.extra_ports, vec![(9101, 9001)]);
        assert!(spec.env.is_empty(), "credentials must not be plain -e env");
        assert_eq!(
            spec.secret_env,
            [
                ("MINIO_ROOT_USER".to_string(), "me".to_string()),
                ("MINIO_ROOT_PASSWORD".to_string(), "pw".to_string()),
            ]
        );
    }

    #[test]
    fn vault_docker_spec_dev_mode_and_server_config() {
        let dev = dspec(
            "vault",
            &with_extra(
                "vault",
                &[("dev_mode", crate::config::ExtraValue::Bool(true))],
            ),
        );
        assert_eq!(
            (dev.image.as_str(), dev.default_tag.as_str()),
            ("hashicorp/vault", "1.16")
        );
        assert_eq!(dev.container_port, 8200);
        assert_eq!(dev.args, s(&["server", "-dev"]));
        assert_eq!(env_of(&dev, "VAULT_DEV_ROOT_TOKEN_ID"), Some("root"));
        assert!(env_of(&dev, "VAULT_LOCAL_CONFIG").is_none());

        let server = dspec("vault", &with_extra("vault", &[("port", num(51200))]));
        assert_eq!(server.args, s(&["server"]));
        assert_eq!(server.data_path.as_deref(), Some("/vault/file"));
        let config: serde_json::Value =
            serde_json::from_str(env_of(&server, "VAULT_LOCAL_CONFIG").unwrap()).unwrap();
        assert_eq!(config["storage"]["file"]["path"], "/vault/file");
        assert_eq!(config["listener"]["tcp"]["tls_disable"], true);
        assert_eq!(config["disable_mlock"], true);
        assert_eq!(config["api_addr"], "http://127.0.0.1:51200");
        assert!(env_of(&server, "VAULT_DEV_ROOT_TOKEN_ID").is_none());
    }

    #[test]
    fn kafka_docker_spec_kraft_with_host_port_listener() {
        let spec = dspec("kafka", &with_extra("kafka", &[("port", num(51000))]));
        assert_eq!(
            (spec.image.as_str(), spec.default_tag.as_str()),
            ("apache/kafka", "3.7.0")
        );
        assert_eq!(spec.container_port, 9092);
        assert_eq!(spec.data_path.as_deref(), Some("/var/lib/kafka/data"));
        assert_eq!(
            env_of(&spec, "KAFKA_PROCESS_ROLES"),
            Some("broker,controller")
        );
        assert_eq!(
            env_of(&spec, "KAFKA_ADVERTISED_LISTENERS"),
            Some("PLAINTEXT://127.0.0.1:51000")
        );
        assert_eq!(
            env_of(&spec, "KAFKA_LISTENERS"),
            Some("PLAINTEXT://:9092,CONTROLLER://:9093")
        );
        assert_eq!(
            env_of(&spec, "KAFKA_CONTROLLER_QUORUM_VOTERS"),
            Some("1@localhost:9093")
        );
        assert_eq!(
            env_of(&spec, "KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR"),
            Some("1")
        );
        assert_eq!(env_of(&spec, "KAFKA_LOG_DIRS"), Some("/var/lib/kafka/data"));
    }

    #[test]
    fn kafka_docker_warns_unless_kraft() {
        let warning = "zookeeper mode is not supported with docker — running Kafka in KRaft mode";
        assert_eq!(
            get("kafka").docker_warnings(&Dependency::simple("kafka")),
            vec![warning]
        );
        let off = with_extra(
            "kafka",
            &[("kraft", crate::config::ExtraValue::Bool(false))],
        );
        assert_eq!(get("kafka").docker_warnings(&off), vec![warning]);
        let on = with_extra("kafka", &[("kraft", crate::config::ExtraValue::Bool(true))]);
        assert!(get("kafka").docker_warnings(&on).is_empty());
    }

    #[test]
    fn every_builtin_service_has_a_docker_spec_and_nothing_else_does() {
        for (name, module) in REGISTRY {
            let spec = module.docker_spec(&Dependency::simple(name)).unwrap();
            assert_eq!(
                spec.is_some(),
                module.is_service(),
                "{name}: docker spec iff service"
            );
        }
        assert!(
            get("someunknownservice")
                .docker_spec(&Dependency::simple("someunknownservice"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn only_conf_writing_post_setups_are_skipped_under_docker() {
        let mut skipped: Vec<&str> = REGISTRY
            .iter()
            .filter(|(_, m)| m.post_setup_writes_service_config())
            .map(|(n, _)| *n)
            .collect();
        skipped.sort_unstable();
        assert_eq!(
            skipped,
            vec!["kafka", "mariadb", "mysql", "postgresql", "rabbitmq"]
        );
    }

    #[test]
    fn every_builtin_service_has_a_nix_launch_and_generic_does_not() {
        for (name, module) in REGISTRY {
            if !module.is_service() {
                continue;
            }
            let dir = crate::test_support::tmp_dir();
            let dep = Dependency::simple(name);
            assert!(
                module.nix_launch(&dep, &dir).unwrap().is_some(),
                "{name} must define a nix launch"
            );
        }
        let dir = crate::test_support::tmp_dir();
        assert!(
            get("someunknownservice")
                .nix_launch(&Dependency::simple("someunknownservice"), &dir)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn start_via_pm_passes_launch_and_creates_data_dir_under_nix() {
        use crate::package_manager::MockPackageManager;
        let root = crate::test_support::tmp_dir();
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let dep = with_extra("postgres", &[("port", num(51000))]);
        get("postgres").start(&pm, &dep, &root).unwrap();
        let data = root.join(".devy").join("data").join("postgresql");
        assert!(
            data.is_dir(),
            "data dir must be created under the canonical name"
        );
        let launches = pm.started_launches.borrow();
        let spec = launches[0]
            .as_ref()
            .expect("nix must receive a launch spec");
        assert!(spec.args.contains(&"51000".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn socket_dir_uses_data_dir_when_short_enough() {
        let d = Path::new("/p/.devy/data/postgresql");
        assert_eq!(socket_dir(d, ".s.PGSQL.51000").unwrap(), d);
    }

    #[cfg(unix)]
    #[test]
    fn socket_dir_falls_back_to_private_user_dir_for_deep_projects() {
        use std::os::unix::fs::PermissionsExt;
        let deep = PathBuf::from(format!("/{}/.devy/data/postgresql", "x".repeat(90)));
        let dir = socket_dir(&deep, ".s.PGSQL.51000").unwrap();
        assert_eq!(
            dir.parent(),
            Some(crate::fs_safe::user_dir_path().as_path())
        );
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(dir.join(".s.PGSQL.65535.lock").as_os_str().len() <= MAX_SOCKET_PATH);
        assert_eq!(
            dir,
            socket_dir(&deep, ".s.PGSQL.51000").unwrap(),
            "must be stable"
        );
    }

    /// Scenario "Pre-created socket directory": a socket directory owned by another
    /// user makes the service start fail with the ownership error.
    #[cfg(unix)]
    #[test]
    fn foreign_socket_dir_fails_service_start() {
        use crate::package_manager::MockPackageManager;
        let tmp = crate::test_support::tmp_dir();
        let root = tmp.join("p".repeat(100));
        std::fs::create_dir(&root).unwrap();
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let dep = Dependency::simple("postgresql");
        // The directory exists and is ours, but presented as another user's.
        crate::fs_safe::user_dir().unwrap();
        let err = crate::fs_safe::with_fake_owner(crate::fs_safe::current_uid() + 1, || {
            start_via_pm(get("postgresql"), &pm, &dep, &root)
        })
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("is not owned by the current user"),
            "{err:#}"
        );
    }

    #[test]
    fn nix_launch_for_defaults_working_dir_to_data_dir() {
        use crate::package_manager::MockPackageManager;
        let root = crate::test_support::tmp_dir();
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let spec = nix_launch_for(get("redis"), &pm, &Dependency::simple("redis"), &root)
            .unwrap()
            .unwrap();
        assert_eq!(spec.working_dir, Some(nix_data_dir(&root, "redis")));
    }

    #[test]
    fn start_via_pm_passes_no_launch_on_other_backends() {
        use crate::package_manager::MockPackageManager;
        let root = crate::test_support::tmp_dir();
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        get("redis")
            .start(&pm, &Dependency::simple("redis"), &root)
            .unwrap();
        assert_eq!(*pm.started_launches.borrow(), vec![None]);
        assert!(!root.join(".devy").exists(), "no data dir outside nix");
    }

    // The nix backend only exists on macOS and Linux; expected paths use `/`.
    #[cfg(unix)]
    #[test]
    fn postgres_launch_initdb_once_and_listens_locally() {
        let d = Path::new("/p/.devy/data/postgresql");
        let spec = launch(
            "postgresql",
            &with_extra("postgresql", &[("port", num(51000))]),
            d,
        );
        assert_eq!(spec.exec, "postgres");
        assert_eq!(
            spec.args,
            s(&[
                "-D",
                "/p/.devy/data/postgresql",
                "-p",
                "51000",
                "-k",
                "/p/.devy/data/postgresql",
                "-c",
                "listen_addresses=127.0.0.1",
            ])
        );
        let init = spec.init.unwrap();
        assert_eq!(init.marker, d.join("PG_VERSION"));
        assert_eq!(init.cmd, s(&["initdb", "-D", "/p/.devy/data/postgresql"]));
    }

    // The nix backend only exists on macOS and Linux; expected paths use `/`.
    #[cfg(unix)]
    #[test]
    fn mysql_launch_with_sanitized_cli_args() {
        let d = Path::new("/p/.devy/data/mysql");
        let dep = with_extra(
            "mysql",
            &[
                ("port", num(51001)),
                (
                    "cli_args",
                    crate::config::ExtraValue::String(
                        "--innodb-buffer-pool-size=256M bogus --x;y=1".into(),
                    ),
                ),
            ],
        );
        let spec = launch("mysql", &dep, d);
        assert_eq!(spec.exec, "mysqld");
        assert_eq!(
            spec.args,
            s(&[
                "--no-defaults",
                "--datadir=/p/.devy/data/mysql",
                "--innodb-buffer-pool-size=256M",
                "--port=51001",
                "--bind-address=127.0.0.1",
                "--socket=/p/.devy/data/mysql/mysql.sock",
                "--mysqlx=OFF",
            ])
        );
        assert_eq!(spec.exec_package.as_deref(), Some("mysql84"));
        let init = spec.init.unwrap();
        assert_eq!(init.marker, d.join("mysql"));
        assert_eq!(
            init.cmd,
            s(&[
                "mysqld",
                "--no-defaults",
                "--initialize-insecure",
                "--datadir=/p/.devy/data/mysql",
            ])
        );
    }

    // The nix backend only exists on macOS and Linux; expected paths use `/`.
    #[cfg(unix)]
    #[test]
    fn mariadb_launch_uses_mariadbd_and_install_db() {
        let d = Path::new("/p/.devy/data/mariadb");
        let spec = launch(
            "mariadb",
            &with_extra("mariadb", &[("port", num(51002))]),
            d,
        );
        assert_eq!(spec.exec, "mariadbd");
        assert_eq!(
            spec.exec_package, None,
            "mariadb wins conflicts; the profile bin is fine"
        );
        assert_eq!(spec.args[0], "--no-defaults");
        assert!(!spec.args.contains(&"--mysqlx=OFF".to_string()));
        assert!(spec.args.contains(&"--port=51002".to_string()));
        assert!(spec.args.contains(&"--bind-address=127.0.0.1".to_string()));
        let init = spec.init.unwrap();
        assert_eq!(init.marker, d.join("mysql"));
        assert_eq!(
            init.cmd,
            s(&[
                "mariadb-install-db",
                "--no-defaults",
                "--datadir=/p/.devy/data/mariadb",
                "--auth-root-authentication-method=normal",
            ])
        );
    }

    // The nix backend only exists on macOS and Linux.
    #[cfg(unix)]
    #[test]
    fn mysql_family_launch_skips_dangerous_options() {
        for name in ["mysql", "mariadb"] {
            let dep = with_extra(
                name,
                &[
                    ("port", num(51001)),
                    (
                        "cli_args",
                        crate::config::ExtraValue::String(
                            "--bind-address=0.0.0.0 --skip-grant-tables=1".into(),
                        ),
                    ),
                ],
            );
            let mut spec = None;
            let msgs = crate::output::with_warn_messages(|| {
                spec = Some(launch(name, &dep, Path::new("/p/.devy/data/db")));
            });
            let args = spec.unwrap().args;
            assert_eq!(
                msgs,
                [
                    "skipped cli_args token --bind-address=0.0.0.0: not an allowed server option",
                    "skipped cli_args token --skip-grant-tables=1: not an allowed server option",
                ],
                "{name}"
            );
            assert!(!args.iter().any(|a| a.contains("0.0.0.0")), "{name}");
            assert!(!args.iter().any(|a| a.contains("grant")), "{name}");
            assert_eq!(
                args.iter()
                    .filter(|a| a.starts_with("--bind-address"))
                    .count(),
                1,
                "{name}"
            );
        }
    }

    // The nix backend only exists on macOS and Linux.
    #[cfg(unix)]
    #[test]
    fn mysql_family_launch_forced_bind_wins() {
        for name in ["mysql", "mariadb"] {
            let dep = with_extra(
                name,
                &[(
                    "cli_args",
                    crate::config::ExtraValue::String(
                        "--max-connections=5 --sql_mode=ANSI --skip-name-resolve=1".into(),
                    ),
                )],
            );
            let args = launch(name, &dep, Path::new("/p/.devy/data/db")).args;
            let bind = args
                .iter()
                .position(|a| a == "--bind-address=127.0.0.1")
                .expect("forced bind present");
            for token in [
                "--max-connections=5",
                "--sql-mode=ANSI",
                "--skip-name-resolve=1",
            ] {
                let at = args
                    .iter()
                    .position(|a| a == token)
                    .unwrap_or_else(|| panic!("{name}: {token} missing from {args:?}"));
                assert!(at < bind, "{name}: {token} must precede the forced bind");
            }
            assert_eq!(args[0], "--no-defaults", "{name}");
        }
    }

    #[test]
    fn redis_memcached_launch_args() {
        let d = Path::new("/p/.devy/data/redis");
        let spec = launch("redis", &with_extra("redis", &[("port", num(51000))]), d);
        assert_eq!(spec.exec, "redis-server");
        assert_eq!(
            spec.args,
            s(&[
                "--port",
                "51000",
                "--bind",
                "127.0.0.1",
                "--dir",
                "/p/.devy/data/redis"
            ])
        );
        assert!(spec.init.is_none());

        let spec = launch(
            "memcached",
            &with_extra("memcached", &[("port", num(51003))]),
            d,
        );
        assert_eq!(spec.exec, "memcached");
        assert_eq!(spec.args, s(&["-p", "51003", "-l", "127.0.0.1"]));
    }

    // The nix backend only exists on macOS and Linux; expected paths use `/`.
    #[cfg(unix)]
    #[test]
    fn rabbitmq_launch_env() {
        let dir = crate::test_support::tmp_dir();
        let d: &Path = &dir;
        let spec = launch(
            "rabbitmq",
            &with_extra("rabbitmq", &[("port", num(51004))]),
            d,
        );
        assert_eq!(spec.exec, "rabbitmq-server");
        assert!(spec.args.is_empty());
        let env: HashMap<_, _> = spec.env.into_iter().collect();
        assert_eq!(env["RABBITMQ_NODE_PORT"], "51004");
        assert_eq!(env["RABBITMQ_NODE_IP_ADDRESS"], "127.0.0.1");
        assert_eq!(env["ERL_EPMD_ADDRESS"], "127.0.0.1");
        assert_eq!(
            env["RABBITMQ_SERVER_ADDITIONAL_ERL_ARGS"],
            "-kernel inet_dist_use_interface {127,0,0,1}"
        );
        assert_eq!(env["RABBITMQ_MNESIA_BASE"], path_arg(&d.join("mnesia")));
        assert_eq!(env["RABBITMQ_LOG_BASE"], path_arg(&d.join("log")));
        let dist: u16 = env["RABBITMQ_DIST_PORT"].parse().expect("valid dist port");
        assert_ne!(dist, 51004);
        assert!(env["RABBITMQ_NODENAME"].starts_with("devy-"));
        assert!(env["RABBITMQ_NODENAME"].ends_with("@localhost"));
        // The dist port is reused on the next start.
        let again = launch(
            "rabbitmq",
            &with_extra("rabbitmq", &[("port", num(51004))]),
            d,
        );
        let again: HashMap<_, _> = again.env.into_iter().collect();
        assert_eq!(again["RABBITMQ_DIST_PORT"], dist.to_string());
    }

    // The nix backend only exists on macOS and Linux; expected paths use `/`.
    #[cfg(unix)]
    #[test]
    fn search_servers_launch_with_settings() {
        for (name, conf_env, extra) in [
            (
                "elasticsearch",
                "ES_PATH_CONF",
                &["-E", "xpack.ml.enabled=false"][..],
            ),
            ("opensearch", "OPENSEARCH_PATH_CONF", &[][..]),
        ] {
            let dir = crate::test_support::tmp_dir();
            let d = path_arg(&dir);
            let spec = launch(name, &with_extra(name, &[("port", num(51005))]), &dir);
            assert_eq!(spec.exec, name);
            let mut args = s(&[
                "-E",
                "http.host=127.0.0.1",
                "-E",
                "transport.host=127.0.0.1",
                "-E",
                "http.port=51005",
                "-E",
                &format!("path.data={d}/data"),
                "-E",
                &format!("path.logs={d}/logs"),
            ]);
            args.extend(s(extra));
            assert_eq!(spec.args, args, "{name}");
            assert_eq!(spec.exec_package.as_deref(), Some(name));
            let rewrite = |from: &str, to: String| SeedRewrite {
                file: "jvm.options".into(),
                from: from.into(),
                to,
            };
            assert_eq!(
                spec.seed_dirs,
                [SeedDir {
                    from_package: "config".into(),
                    to: dir.join("config"),
                    rewrites: vec![
                        rewrite("=logs/", format!("={d}/logs/")),
                        rewrite(":logs/", format!(":{d}/logs/")),
                        rewrite(
                            "-XX:HeapDumpPath=data",
                            format!("-XX:HeapDumpPath={d}/data")
                        ),
                    ],
                }]
            );
            assert_eq!(spec.env, [(conf_env.into(), format!("{d}/config"))]);
            assert!(dir.join("logs").is_dir(), "{name} must create the logs dir");
        }
        let dir = crate::test_support::tmp_dir();
        let es = launch("elasticsearch", &Dependency::simple("elasticsearch"), &dir);
        assert!(es.conditional_args.is_empty());
        assert_eq!(es.package_env, [("ES_HOME".into(), String::new())]);
        let os = launch("opensearch", &Dependency::simple("opensearch"), &dir);
        assert!(os.package_env.is_empty());
        assert_eq!(
            os.conditional_args,
            [ConditionalArgs {
                package_path: "plugins/opensearch-security".into(),
                args: s(&["-E", "plugins.security.disabled=true"]),
            }]
        );
    }

    #[test]
    fn meilisearch_launch_with_and_without_master_key() {
        let d = Path::new("/p/d");
        let spec = launch(
            "meilisearch",
            &with_extra("meilisearch", &[("port", num(51006))]),
            d,
        );
        assert_eq!(spec.exec, "meilisearch");
        assert_eq!(
            spec.args,
            s(&["--http-addr", "127.0.0.1:51006", "--db-path", "/p/d"])
        );
        let dep = with_extra(
            "meilisearch",
            &[
                ("port", num(51006)),
                (
                    "master_key",
                    crate::config::ExtraValue::String("k3y".into()),
                ),
            ],
        );
        let spec = launch("meilisearch", &dep, d);
        assert_eq!(
            spec.args,
            s(&["--http-addr", "127.0.0.1:51006", "--db-path", "/p/d"])
        );
        assert!(
            !spec.args.iter().any(|a| a.contains("k3y")),
            "the master key must not be in argv"
        );
        assert_eq!(spec.env, [("MEILI_MASTER_KEY".into(), "k3y".into())]);
    }

    #[test]
    fn minio_launch_console_and_credentials() {
        let dir = crate::test_support::tmp_dir();
        let d: &Path = &dir;
        let spec = launch("minio", &with_extra("minio", &[("port", num(51007))]), d);
        assert_eq!(spec.exec, "minio");
        assert_eq!(
            spec.args[..4],
            s(&["server", &path_arg(d), "--address", "127.0.0.1:51007"])
        );
        // Without console_port, a free port is chosen, still on loopback.
        assert_eq!(spec.args[4], "--console-address");
        assert!(spec.args[5].starts_with("127.0.0.1:"), "{:?}", spec.args);
        assert_eq!(spec.args.len(), 6);
        assert!(spec.env.is_empty());
        let dep = with_extra(
            "minio",
            &[
                ("port", num(51007)),
                ("console_port", num(9001)),
                ("access_key", crate::config::ExtraValue::String("u".into())),
                ("secret_key", crate::config::ExtraValue::String("p".into())),
            ],
        );
        let spec = launch("minio", &dep, d);
        assert_eq!(spec.args[4..], s(&["--console-address", "127.0.0.1:9001"]));
        assert!(!spec.args.iter().any(|a| a == "u" || a == "p"));
        let env: HashMap<_, _> = spec.env.into_iter().collect();
        assert_eq!(env["MINIO_ROOT_USER"], "u");
        assert_eq!(env["MINIO_ROOT_PASSWORD"], "p");
    }

    #[test]
    fn mailhog_launch_binds_smtp_port() {
        let spec = launch(
            "mailhog",
            &with_extra("mailhog", &[("smtp_port", num(51008))]),
            Path::new("/p/d"),
        );
        assert_eq!(spec.exec, "MailHog");
        assert_eq!(
            spec.args,
            s(&[
                "-smtp-bind-addr",
                "127.0.0.1:51008",
                "-ui-bind-addr",
                "127.0.0.1:8025",
                "-api-bind-addr",
                "127.0.0.1:8025",
            ])
        );
    }

    #[test]
    fn mongodb_launch_runs_mongod() {
        let spec = launch(
            "mongo",
            &with_extra("mongo", &[("port", num(51000))]),
            Path::new("/p/.devy/data/mongodb"),
        );
        assert_eq!(spec.exec, "mongod");
        assert_eq!(
            spec.args,
            s(&[
                "--port",
                "51000",
                "--bind_ip",
                "127.0.0.1",
                "--dbpath",
                "/p/.devy/data/mongodb",
                "--unixSocketPrefix",
                "/p/.devy/data/mongodb",
            ])
        );
    }

    // The nix backend only exists on macOS and Linux; expected paths use `/`.
    #[cfg(unix)]
    #[test]
    fn nginx_launch_writes_foreground_config() {
        let dir = crate::test_support::tmp_dir();
        let spec = launch("nginx", &with_extra("nginx", &[("port", num(51009))]), &dir);
        let conf = dir.join("nginx.conf");
        assert_eq!(spec.exec, "nginx");
        assert_eq!(
            spec.args,
            vec![
                "-p".to_string(),
                path_arg(&dir),
                "-c".to_string(),
                path_arg(&conf)
            ]
        );
        let text = std::fs::read_to_string(&conf).unwrap();
        assert!(text.contains("daemon off;"));
        assert!(text.contains("listen 127.0.0.1:51009;"));
        assert!(text.contains(&format!("pid \"{}\";", dir.join("nginx.pid").display())));
        assert!(text.contains("client_body_temp_path"));
        assert_eq!(text.matches('{').count(), text.matches('}').count());
    }

    #[test]
    fn vault_launch_dev_mode() {
        let dir = crate::test_support::tmp_dir();
        let dep = with_extra(
            "vault",
            &[
                ("port", num(51010)),
                ("dev_mode", crate::config::ExtraValue::Bool(true)),
            ],
        );
        let spec = launch("vault", &dep, &dir);
        assert_eq!(spec.exec, "vault");
        assert_eq!(
            spec.args,
            s(&[
                "server",
                "-dev",
                "-dev-listen-address=127.0.0.1:51010",
                "-dev-root-token-id=root",
            ])
        );
        assert!(!dir.join("vault.hcl").exists());
    }

    // The nix backend only exists on macOS and Linux; expected paths use `/`.
    #[cfg(unix)]
    #[test]
    fn vault_launch_writes_config_without_dev_mode() {
        let dir = crate::test_support::tmp_dir();
        let spec = launch("vault", &with_extra("vault", &[("port", num(51010))]), &dir);
        let conf = dir.join("vault.hcl");
        assert_eq!(
            spec.args,
            vec!["server".to_string(), format!("-config={}", conf.display())]
        );
        let text = std::fs::read_to_string(&conf).unwrap();
        assert!(text.contains("address     = \"127.0.0.1:51010\""));
        assert!(text.contains("tls_disable = true"));
        assert!(text.contains(&format!("path = \"{}\"", dir.join("storage").display())));
        assert!(text.contains("disable_mlock = true"));
    }

    #[test]
    fn quoted_conf_path_escapes_quotes_and_backslashes() {
        assert_eq!(
            quoted_conf_path(Path::new("/a b/\"c\\d")),
            "\"/a b/\\\"c\\\\d\""
        );
    }

    // ── versioned nix attributes ──────────────────────────────────────────────

    #[test]
    fn nix_versioned_attr_table() {
        let cases: &[(&str, &str, Option<&str>)] = &[
            // node / typescript: major, full, unsupported, nonsense
            ("node", "22", Some("nodejs_22")),
            ("node", "22.11.0", Some("nodejs_22")),
            ("node", "24", Some("nodejs_24")),
            ("node", "20", None),
            ("node", "lts", None),
            ("typescript", "24.1.0", Some("nodejs_24")),
            // python: major.minor
            ("python", "3.12", Some("python312")),
            ("python", "3.12.4", Some("python312")),
            ("python", "3.14", Some("python314")),
            ("python", "3", None),
            ("python", "3.9", None),
            // postgresql
            ("postgresql", "16", Some("postgresql_16")),
            ("postgresql", "16.4", Some("postgresql_16")),
            ("postgresql", "13", None),
            // mysql
            ("mysql", "8.4", Some("mysql84")),
            ("mysql", "8.4.3", Some("mysql84")),
            ("mysql", "8.0", None),
            // java
            ("java", "21", Some("jdk21")),
            ("java", "21.0.2", Some("jdk21")),
            ("java", "8", Some("jdk8")),
            ("java", "23", None),
            // dotnet
            ("dotnet", "8", Some("dotnet-sdk_8")),
            ("dotnet", "8.0.100", Some("dotnet-sdk_8")),
            ("dotnet", "5", None),
            // go
            ("go", "1.26", Some("go_1_26")),
            ("go", "1.26.3", Some("go_1_26")),
            ("go", "1.22", None),
            // modules without versioned attributes
            ("redis", "7", None),
            ("jq", "1.6", None),
        ];
        for (module, version, want) in cases {
            assert_eq!(
                get(module).nix_versioned_attr(version).as_deref(),
                *want,
                "{module} {version}"
            );
        }
    }

    #[test]
    fn unversioned_nix_defaults() {
        let dep = Dependency::simple;
        assert_eq!(get("java").nix_attr(&dep("java")).as_deref(), Some("jdk21"));
        assert_eq!(
            get("dotnet").nix_attr(&dep("dotnet")).as_deref(),
            Some("dotnet-sdk_8")
        );
        assert_eq!(
            get("mysql").nix_attr(&dep("mysql")).as_deref(),
            Some("mysql84")
        );
        assert_eq!(get("go").nix_attr(&dep("go")).as_deref(), Some("go"));
    }

    fn versioned(name: &str, version: &str, from_lock: bool) -> Dependency {
        Dependency {
            version: Some(version.into()),
            version_from_lock: from_lock,
            allow_unfree: false,
            allow_insecure: false,
            ..Dependency::simple(name)
        }
    }

    #[test]
    fn backend_package_is_what_install_installs() {
        use crate::package_manager::{MockPackageManager, brew_formula_name};
        for (name, version, formula) in [
            ("node", "22", "node@22"),
            ("postgres", "16", "postgresql@16"),
            ("mysql", "8.4", "mysql@8.4"),
            ("java", "17", "openjdk@17"),
        ] {
            let pm = MockPackageManager {
                name: "brew",
                ..Default::default()
            };
            let dep = versioned(name, version, false);
            let module = get(name);
            module.install(&pm, &dep).unwrap();
            let installed = pm.installed_deps.borrow();
            let [installed] = installed.as_slice() else {
                panic!("{name}: {installed:?}");
            };
            let pkg = module.backend_package(&pm, &dep).unwrap();
            assert_eq!(format!("{pkg:?}"), format!("{installed:?}"), "{name}");
            assert_eq!(brew_formula_name(&pkg), formula, "{name}");
        }
    }

    // ── unfree nix packages ───────────────────────────────────────────────────

    #[test]
    fn exactly_four_modules_are_nix_unfree() {
        let mut unfree: Vec<&str> = REGISTRY
            .iter()
            .filter(|(_, m)| m.nix_unfree())
            .map(|(n, _)| *n)
            .collect();
        unfree.sort_unstable();
        assert_eq!(unfree, ["elasticsearch", "mongodb", "terraform", "vault"]);
        assert!(
            !get("jq").nix_unfree(),
            "the generic module is never unfree"
        );
    }

    #[test]
    fn nix_install_dep_allows_unfree_only_for_unfree_modules() {
        use crate::package_manager::MockPackageManager;
        let nix = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        for name in [
            "mongodb",
            "redis",
            "vault",
            "terraform",
            "elasticsearch",
            "node",
        ] {
            get(name).install(&nix, &Dependency::simple(name)).unwrap();
        }
        assert_eq!(
            *nix.unfree_packages.borrow(),
            vec!["mongodb-ce", "vault", "terraform", "elasticsearch"]
        );

        let brew = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        get("vault")
            .install(&brew, &Dependency::simple("vault"))
            .unwrap();
        assert!(brew.unfree_packages.borrow().is_empty(), "nix only");
    }

    #[test]
    fn only_elasticsearch_is_nix_insecure() {
        let insecure: Vec<&str> = REGISTRY
            .iter()
            .filter(|(_, m)| m.nix_insecure())
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(insecure, ["elasticsearch"]);
        assert!(!get("jq").nix_insecure());
    }

    #[test]
    fn nix_install_dep_allows_insecure_only_for_insecure_modules() {
        use crate::package_manager::MockPackageManager;
        let nix = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        for name in ["elasticsearch", "mongodb", "redis", "opensearch"] {
            get(name).install(&nix, &Dependency::simple(name)).unwrap();
        }
        assert_eq!(*nix.insecure_packages.borrow(), vec!["elasticsearch"]);

        let brew = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        get("elasticsearch")
            .install(&brew, &Dependency::simple("elasticsearch"))
            .unwrap();
        assert!(brew.insecure_packages.borrow().is_empty(), "nix only");
    }

    #[test]
    fn nix_install_receives_versioned_attr() {
        use crate::package_manager::MockPackageManager;
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        get("node")
            .install(&pm, &versioned("node", "22", false))
            .unwrap();
        get("python")
            .install(&pm, &versioned("python", "3.12", false))
            .unwrap();
        get("go")
            .install(&pm, &versioned("go", "1.26.3", false))
            .unwrap();
        get("node")
            .install(&pm, &versioned("node", "19", false))
            .unwrap();
        assert_eq!(
            *pm.installed_packages.borrow(),
            vec!["nodejs_22", "python312", "go_1_26", "nodejs"]
        );
    }

    #[test]
    fn other_backends_ignore_nix_versioned_attr() {
        use crate::package_manager::MockPackageManager;
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        get("node")
            .install(&pm, &versioned("node", "22", false))
            .unwrap();
        assert_eq!(*pm.installed_packages.borrow(), vec!["node"]);
    }

    #[test]
    fn lock_pinned_version_satisfied_by_unversioned_attr_at_that_version() {
        use crate::package_manager::MockPackageManager;
        // The unversioned attr ("postgresql") is installed at 18.6; the versioned one isn't.
        let pm = MockPackageManager {
            name: "nix",
            installed_pkg: Some("postgresql"),
            version: Some("18.6".into()),
            ..Default::default()
        };
        assert!(
            get("postgresql")
                .is_installed(&pm, &versioned("postgresql", "18.6", true))
                .unwrap(),
            "a lock-pinned version already installed unversioned must count as installed"
        );
        assert!(
            !get("postgresql")
                .is_installed(&pm, &versioned("postgresql", "18.6", false))
                .unwrap(),
            "an explicit version needs the versioned attribute"
        );
        assert!(
            !get("postgresql")
                .is_installed(&pm, &versioned("postgresql", "17.2", true))
                .unwrap(),
            "a different locked version is not satisfied"
        );
    }

    #[test]
    fn lock_pinned_version_satisfied_at_attribute_granularity() {
        use crate::package_manager::MockPackageManager;
        // A teammate on newer nixpkgs locked 24.21.0; this profile has nodejs 24.20.0.
        let pm = MockPackageManager {
            name: "nix",
            installed_pkg: Some("nodejs"),
            version: Some("24.20.0".into()),
            ..Default::default()
        };
        let node = get("node");
        assert!(
            node.is_installed(&pm, &versioned("node", "24.21.0", true))
                .unwrap(),
            "same versioned attribute (nodejs_24) counts as installed"
        );
        assert!(
            !node
                .is_installed(&pm, &versioned("node", "22.1.0", true))
                .unwrap(),
            "a lock pinned to nodejs_22 is not satisfied by nodejs 24"
        );
        assert!(
            !node
                .is_installed(&pm, &versioned("node", "24.21.0", false))
                .unwrap(),
            "an explicit version still needs the versioned attribute"
        );
    }

    #[test]
    fn nix_resolved_version_queries_install_attr_then_unversioned() {
        use crate::package_manager::MockPackageManager;
        let nix = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let node = get("node");
        assert_eq!(
            node.resolved_version(&nix, &versioned("node", "22", false))
                .unwrap(),
            None
        );
        assert_eq!(*nix.version_queries.borrow(), vec!["nodejs_22", "nodejs"]);

        let nix = MockPackageManager {
            name: "nix",
            installed_pkg: Some("nodejs"),
            version: Some("24.20.0".into()),
            ..Default::default()
        };
        assert_eq!(
            node.resolved_version(&nix, &Dependency::simple("node"))
                .unwrap(),
            Some("24.20.0".into())
        );
        assert_eq!(*nix.version_queries.borrow(), vec!["nodejs"]);

        let brew = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        node.resolved_version(&brew, &versioned("node", "22", false))
            .unwrap();
        assert_eq!(*brew.version_queries.borrow(), vec!["node"]);

        let brew = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        get("mongodb")
            .resolved_version(&brew, &Dependency::simple("mongodb"))
            .unwrap();
        assert_eq!(*brew.version_queries.borrow(), vec!["mongodb"]);
    }

    #[test]
    fn nix_resolved_version_uses_mapped_attrs() {
        use crate::package_manager::MockPackageManager;
        for (name, attr) in [
            ("python", "python3"),
            ("java", "jdk21"),
            ("dotnet", "dotnet-sdk_8"),
            ("mysql", "mysql84"),
            ("typescript", "nodejs"),
            ("mongodb", "mongodb-ce"),
        ] {
            let nix = MockPackageManager {
                name: "nix",
                installed_pkg: Some(attr),
                version: Some("1.2.3".into()),
                ..Default::default()
            };
            assert_eq!(
                get(name)
                    .resolved_version(&nix, &Dependency::simple(name))
                    .unwrap(),
                Some("1.2.3".into()),
                "{name} should query {attr}"
            );
        }
    }

    #[test]
    fn nix_version_warning_rules() {
        use crate::package_manager::MockPackageManager;
        let nix = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let brew = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert!(nix_version_warning(&versioned("jq", "1.6", false), &nix).is_some());
        assert!(nix_version_warning(&versioned("jq", "1.6", true), &nix).is_none());
        assert!(nix_version_warning(&versioned("jq", "1.6", false), &brew).is_none());
        assert!(nix_version_warning(&versioned("node", "22", false), &nix).is_none());
        assert!(nix_version_warning(&versioned("node", "20", false), &nix).is_some());
        // rust installs through rustup, which honors the version itself.
        assert!(nix_version_warning(&versioned("rust", "1.80", false), &nix).is_none());
        assert!(nix_version_warning(&Dependency::simple("jq"), &nix).is_none());
    }

    // ── port_applicable ───────────────────────────────────────────────────────

    #[test]
    fn port_applicable_per_backend() {
        use crate::package_manager::MockPackageManager;
        let with = |name: &'static str, config_dir: Option<&str>| MockPackageManager {
            name,
            config_dir: config_dir.map(std::path::PathBuf::from),
            ..Default::default()
        };
        let nix = with("nix", None);
        let brew = with("brew", Some("/opt/homebrew/etc"));
        let apt = with("apt", Some("/etc/postgresql/16/main/conf.d"));
        let apt_no_pg = with("apt", None);
        let winget = with("winget", None);

        assert!(get("redis").port_applicable(&nix));
        assert!(!get("redis").port_applicable(&brew));
        assert!(!get("redis").port_applicable(&apt));
        assert!(!get("redis").port_applicable(&winget));

        // brew and apt run one machine-wide database server: no per-project port.
        for db in ["postgresql", "mysql", "mariadb"] {
            assert!(get(db).port_applicable(&nix), "{db}");
            for pm in [&brew, &apt, &apt_no_pg, &winget] {
                assert!(!get(db).port_applicable(pm), "{db} under {}", pm.name);
            }
        }
        assert!(
            !get("node").port_applicable(&nix),
            "non-services never apply ports"
        );
    }

    // ── extra_port ────────────────────────────────────────────────────────────

    #[test]
    fn extra_port_returns_default_when_key_absent() {
        let dep = Dependency::simple("redis");
        assert_eq!(extra_port(&dep, "port", 6379).unwrap(), 6379);
    }

    #[test]
    fn extra_port_returns_custom_value() {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(6380u64.into()),
        );
        let dep = Dependency::with_extra("redis", extra);
        assert_eq!(extra_port(&dep, "port", 6379).unwrap(), 6380);
    }

    #[test]
    fn extra_port_bails_on_overflow() {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(99999u64.into()),
        );
        let dep = Dependency::with_extra("redis", extra);
        assert!(extra_port(&dep, "port", 6379).is_err());
    }

    #[test]
    fn extra_port_bails_on_zero() {
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(0u64.into()),
        );
        let dep = Dependency::with_extra("redis", extra);
        let err = extra_port(&dep, "port", 6379).unwrap_err();
        assert!(
            err.to_string().contains("out of range"),
            "error must say 'out of range' for port 0"
        );
    }

    #[test]
    fn extra_port_uses_provided_key_name() {
        let mut extra = HashMap::new();
        extra.insert(
            "smtp_port".into(),
            crate::config::ExtraValue::Number(1025u64.into()),
        );
        let dep = Dependency::with_extra("mailhog", extra);
        assert_eq!(extra_port(&dep, "smtp_port", 1025).unwrap(), 1025);
        assert_eq!(extra_port(&dep, "port", 80).unwrap(), 80); // different key → default
    }

    // ── get ───────────────────────────────────────────────────────────────────

    #[test]
    fn get_mysql_is_service() {
        assert!(get("mysql").is_service());
    }

    #[test]
    fn get_redis_is_service() {
        assert!(get("redis").is_service());
    }

    #[test]
    fn get_postgres_is_service() {
        assert!(get("postgresql").is_service());
        assert!(get("postgres").is_service());
    }

    #[test]
    fn get_mongodb_is_service() {
        assert!(get("mongodb").is_service());
        assert!(get("mongo").is_service());
    }

    #[test]
    fn get_nginx_is_service() {
        assert!(get("nginx").is_service());
    }

    #[test]
    fn get_rabbitmq_is_service() {
        assert!(get("rabbitmq").is_service());
    }

    #[test]
    fn get_kafka_is_service() {
        assert!(get("kafka").is_service());
    }

    #[test]
    fn get_memcached_is_service() {
        assert!(get("memcached").is_service());
    }

    #[test]
    fn get_elasticsearch_is_service() {
        assert!(get("elasticsearch").is_service());
        assert!(get("elastic").is_service());
    }

    #[test]
    fn get_opensearch_is_service() {
        assert!(get("opensearch").is_service());
    }

    #[test]
    fn get_meilisearch_is_service() {
        assert!(get("meilisearch").is_service());
        assert!(get("meili").is_service());
    }

    #[test]
    fn get_minio_is_service() {
        assert!(get("minio").is_service());
    }

    #[test]
    fn get_mailhog_is_service() {
        assert!(get("mailhog").is_service());
    }

    #[test]
    fn get_mariadb_is_service() {
        assert!(get("mariadb").is_service());
    }

    #[test]
    fn get_vault_is_service() {
        assert!(get("vault").is_service());
        assert!(get("hashicorp-vault").is_service());
    }

    #[test]
    fn get_erlang_is_not_a_service() {
        assert!(!get("erlang").is_service());
    }

    #[test]
    fn get_elixir_is_not_a_service() {
        assert!(!get("elixir").is_service());
    }

    #[test]
    fn get_deno_source_is_deno_installer() {
        assert_eq!(get("deno").source(), Some("deno-installer"));
    }

    #[test]
    fn get_bun_source_is_bun_installer() {
        assert_eq!(get("bun").source(), Some("bun-installer"));
    }

    #[test]
    fn get_dotnet_aliases_resolve() {
        for name in &["dotnet", "dotnet-sdk", "csharp"] {
            assert!(!get(name).is_service(), "{} should not be a service", name);
        }
    }

    #[test]
    fn get_dart_is_not_a_service() {
        assert!(!get("dart").is_service());
    }

    #[test]
    fn get_zig_is_not_a_service() {
        assert!(!get("zig").is_service());
    }

    #[test]
    fn get_crystal_is_not_a_service() {
        assert!(!get("crystal").is_service());
    }

    #[test]
    fn get_awscli_aliases_resolve() {
        for name in &["awscli", "aws", "aws-cli"] {
            assert!(!get(name).is_service(), "{} should not be a service", name);
        }
    }

    #[test]
    fn get_gh_aliases_resolve() {
        for name in &["gh", "github-cli"] {
            assert!(!get(name).is_service(), "{} should not be a service", name);
        }
    }

    #[test]
    fn get_gcloud_source_is_gcloud_installer() {
        assert_eq!(get("gcloud").source(), Some("gcloud-installer"));
        assert_eq!(get("google-cloud-sdk").source(), Some("gcloud-installer"));
    }

    #[test]
    fn get_kubectl_aliases_resolve() {
        for name in &["kubectl", "kubernetes-cli"] {
            assert!(!get(name).is_service(), "{} should not be a service", name);
        }
    }

    #[test]
    fn get_helm_is_not_a_service() {
        assert!(!get("helm").is_service());
    }

    #[test]
    fn get_terraform_is_not_a_service() {
        assert!(!get("terraform").is_service());
    }

    #[test]
    fn get_azure_cli_aliases_resolve() {
        for name in &["azure-cli", "az"] {
            assert!(!get(name).is_service(), "{} should not be a service", name);
        }
    }

    #[test]
    fn get_rust_source_is_rustup() {
        assert_eq!(get("rust").source(), Some("rustup"));
        assert_eq!(get("rustup").source(), Some("rustup"));
    }

    #[test]
    fn get_node_aliases_resolve() {
        for name in &["node", "nodejs", "javascript", "js"] {
            let m = get(name);
            assert!(!m.is_service(), "{} should not be a service", name);
        }
    }

    #[test]
    fn get_language_aliases_are_not_services() {
        for name in &[
            "python",
            "python3",
            "java",
            "openjdk",
            "go",
            "golang",
            "ruby",
            "typescript",
            "ts",
            "kotlin",
            "scala",
            "php",
            "elixir",
        ] {
            assert!(!get(name).is_service(), "{} should not be a service", name);
        }
    }

    #[test]
    fn get_unknown_falls_back_to_generic() {
        let m = get("somerandompkg");
        assert!(!m.is_service());
        assert_eq!(m.source(), None);
    }

    // ── get() match arm identity tests ───────────────────────────────────────
    // These use an apt PM that reports "nodejs" as installed but not "node",
    // distinguishing NodeModule (which maps to "nodejs" on apt) from GenericModule
    // (which passes through the raw dep name "node").

    #[test]
    fn get_node_routes_to_node_module_on_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            installed_pkg: Some("nodejs"),
            ..Default::default()
        };
        let dep = Dependency::simple("node");
        assert!(get("node").is_installed(&pm, &dep).unwrap());
        assert!(get("nodejs").is_installed(&pm, &dep).unwrap());
        assert!(get("javascript").is_installed(&pm, &dep).unwrap());
        assert!(get("js").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_typescript_routes_to_typescript_module_on_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            installed_pkg: Some("nodejs"),
            ..Default::default()
        };
        let dep = Dependency::simple("typescript");
        assert!(get("typescript").is_installed(&pm, &dep).unwrap());
        assert!(get("ts").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_ruby_routes_to_ruby_module() {
        // RubyModule.source() returns Some("rbenv"), which GenericModule does not.
        assert_eq!(get("ruby").source(), Some("rbenv"));
    }

    #[test]
    fn get_python_routes_to_package_module_on_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            installed_pkg: Some("python3"),
            ..Default::default()
        };
        let dep = Dependency::simple("python");
        assert!(get("python").is_installed(&pm, &dep).unwrap());
        assert!(get("python3").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_go_routes_to_package_module_on_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            installed_pkg: Some("golang-go"),
            ..Default::default()
        };
        let dep = Dependency::simple("go");
        assert!(get("go").is_installed(&pm, &dep).unwrap());
        assert!(get("golang").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_java_routes_to_package_module_on_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            installed_pkg: Some("default-jdk"),
            ..Default::default()
        };
        let dep = Dependency::simple("java");
        assert!(get("java").is_installed(&pm, &dep).unwrap());
        assert!(get("openjdk").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_kotlin_routes_to_kotlin_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("JetBrains.Kotlin"),
            ..Default::default()
        };
        let dep = Dependency::simple("kotlin");
        assert!(get("kotlin").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_scala_routes_to_package_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("EPFL.Scala"),
            ..Default::default()
        };
        let dep = Dependency::simple("scala");
        assert!(get("scala").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_php_routes_to_package_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("PHP.PHP"),
            ..Default::default()
        };
        let dep = Dependency::simple("php");
        assert!(get("php").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_elixir_routes_to_elixir_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Erlang-Solutions.Elixir"),
            ..Default::default()
        };
        let dep = Dependency::simple("elixir");
        assert!(get("elixir").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_erlang_routes_to_erlang_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Erlang-Solutions.Erlang"),
            ..Default::default()
        };
        let dep = Dependency::simple("erlang");
        assert!(get("erlang").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_dotnet_routes_to_dotnet_module_on_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            installed_pkg: Some("dotnet-sdk-8.0"),
            ..Default::default()
        };
        let dep = Dependency::simple("dotnet");
        assert!(get("dotnet").is_installed(&pm, &dep).unwrap());
        assert!(get("dotnet-sdk").is_installed(&pm, &dep).unwrap());
        assert!(get("csharp").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_dart_routes_to_dart_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Dart.Dart"),
            ..Default::default()
        };
        let dep = Dependency::simple("dart");
        assert!(get("dart").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_zig_routes_to_zig_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("zig-lang.zig"),
            ..Default::default()
        };
        let dep = Dependency::simple("zig");
        assert!(get("zig").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_crystal_routes_to_crystal_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Manas.Crystal"),
            ..Default::default()
        };
        let dep = Dependency::simple("crystal");
        assert!(get("crystal").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_awscli_routes_to_package_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Amazon.AWSCLI"),
            ..Default::default()
        };
        let dep = Dependency::simple("awscli");
        assert!(get("awscli").is_installed(&pm, &dep).unwrap());
        assert!(get("aws").is_installed(&pm, &dep).unwrap());
        assert!(get("aws-cli").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_gh_routes_to_package_module_on_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            installed_pkg: Some("gh"),
            ..Default::default()
        };
        let dep = Dependency::simple("gh");
        assert!(get("gh").is_installed(&pm, &dep).unwrap());
        assert!(get("github-cli").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_kubectl_routes_to_package_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Kubernetes.kubectl"),
            ..Default::default()
        };
        let dep = Dependency::simple("kubectl");
        assert!(get("kubectl").is_installed(&pm, &dep).unwrap());
        assert!(get("kubernetes-cli").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_helm_routes_to_package_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Helm.Helm"),
            ..Default::default()
        };
        let dep = Dependency::simple("helm");
        assert!(get("helm").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_terraform_routes_to_package_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Hashicorp.Terraform"),
            ..Default::default()
        };
        let dep = Dependency::simple("terraform");
        assert!(get("terraform").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_azure_cli_routes_to_package_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Microsoft.AzureCLI"),
            ..Default::default()
        };
        let dep = Dependency::simple("azure-cli");
        assert!(get("azure-cli").is_installed(&pm, &dep).unwrap());
        assert!(get("az").is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn get_swift_routes_to_package_module_on_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("Swift.Toolchain"),
            ..Default::default()
        };
        let dep = Dependency::simple("swift");
        assert!(get("swift").is_installed(&pm, &dep).unwrap());
    }

    // ── extra_list ────────────────────────────────────────────────────────────

    #[test]
    fn extra_list_missing_key_returns_empty() {
        let dep = Dependency::simple("node");
        assert!(extra_list(&dep, "global_packages").unwrap().is_empty());
    }

    #[test]
    fn extra_list_sequence_returns_strings() {
        let mut extra = HashMap::new();
        extra.insert(
            "global_packages".to_string(),
            crate::config::ExtraValue::Sequence(vec![
                crate::config::ExtraValue::String("typescript".into()),
                crate::config::ExtraValue::String("eslint".into()),
            ]),
        );
        let dep = Dependency {
            name: "node".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
        };
        let pkgs = extra_list(&dep, "global_packages").unwrap();
        assert_eq!(pkgs, vec!["typescript", "eslint"]);
    }

    #[test]
    fn extra_list_non_sequence_value_returns_empty() {
        let mut extra = HashMap::new();
        extra.insert(
            "global_packages".to_string(),
            crate::config::ExtraValue::String("ts".into()),
        );
        let dep = Dependency {
            name: "node".into(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            extra,
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
        };
        assert!(extra_list(&dep, "global_packages").unwrap().is_empty());
    }

    // ── pm_dep ────────────────────────────────────────────────────────────────

    #[test]
    fn pm_dep_replaces_name_preserves_other_fields() {
        let dep = Dependency {
            name: "ruby".into(),
            version: Some("3.2".into()),
            tap: Some("homebrew/core".into()),
            after_install: None,
            shell: None,
            extra: HashMap::new(),
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
        };
        let remapped = pm_dep(&dep, "ruby@3.2");
        assert_eq!(remapped.name, "ruby@3.2");
        assert_eq!(remapped.version, Some("3.2".into()));
        assert_eq!(remapped.tap, Some("homebrew/core".into()));
    }

    // ── Module trait defaults ─────────────────────────────────────────────────

    struct DefaultModule;
    impl Module for DefaultModule {
        fn is_installed(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn install(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn default_service_name_returns_dep_name() {
        let dep = Dependency::simple("myservice");
        assert_eq!(DefaultModule.service_name(&dep).as_ref(), "myservice");
    }

    #[test]
    fn default_service_name_different_names() {
        assert_eq!(
            DefaultModule
                .service_name(&Dependency::simple("redis"))
                .as_ref(),
            "redis"
        );
        assert_eq!(
            DefaultModule
                .service_name(&Dependency::simple("mysql"))
                .as_ref(),
            "mysql"
        );
    }

    #[test]
    fn default_is_running_returns_false() {
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("test");
        assert!(!DefaultModule.is_running(&pm, &dep).unwrap());
    }

    #[test]
    fn default_env_vars_returns_empty_map() {
        let dep = Dependency::simple("test");
        assert!(
            DefaultModule
                .env_vars(&dep, std::path::Path::new("/tmp"))
                .is_empty()
        );
    }

    #[test]
    fn default_resolved_version_delegates_to_pm() {
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("test");
        assert!(DefaultModule.resolved_version(&pm, &dep).unwrap().is_none());
    }

    #[test]
    fn default_resolved_version_delegates_to_pm_returns_some_when_pm_has_version() {
        let pm = crate::package_manager::MockPackageManager {
            version: Some("1.2.3".into()),
            ..Default::default()
        };
        let dep = Dependency::simple("test");
        assert_eq!(
            DefaultModule.resolved_version(&pm, &dep).unwrap(),
            Some("1.2.3".into())
        );
    }

    // ── wait_for_ready ────────────────────────────────────────────────────────

    struct HealthyModule;
    impl Module for HealthyModule {
        fn is_installed(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn install(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<()> {
            Ok(())
        }
        fn health_check(&self, _: &Dependency) -> Result<()> {
            Ok(())
        }
        fn service_config(&self) -> ServiceConfig {
            ServiceConfig {
                health_check_sleep_ms: 0,
                ..Default::default()
            }
        }
    }

    #[test]
    fn wait_for_ready_succeeds_immediately_when_healthy() {
        let dep = Dependency::simple("testservice");
        HealthyModule.wait_for_ready(&dep).unwrap();
    }

    struct ZeroAttemptsModule;
    impl Module for ZeroAttemptsModule {
        fn is_installed(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn install(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<()> {
            Ok(())
        }
        fn service_config(&self) -> ServiceConfig {
            ServiceConfig {
                health_check_max_attempts: 0,
                ..Default::default()
            }
        }
    }

    #[test]
    fn wait_for_ready_returns_err_when_max_attempts_is_zero() {
        let dep = Dependency::simple("zeroservice");
        let result = ZeroAttemptsModule.wait_for_ready(&dep);
        assert!(
            result.is_err(),
            "must return Err (not panic) when max_attempts is 0"
        );
        assert!(result.unwrap_err().to_string().contains("zeroservice"));
    }

    struct SickModule;
    impl Module for SickModule {
        fn is_installed(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn install(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<()> {
            Ok(())
        }
        fn health_check(&self, _: &Dependency) -> Result<()> {
            anyhow::bail!("not healthy")
        }
        fn service_config(&self) -> ServiceConfig {
            ServiceConfig {
                health_check_sleep_ms: 0,
                ..Default::default()
            }
        }
    }

    #[test]
    fn wait_for_ready_fails_with_context_after_max_attempts() {
        let dep = Dependency::simple("testservice");
        let err = SickModule.wait_for_ready(&dep).unwrap_err();
        assert!(err.to_string().contains("testservice"));
        assert!(err.to_string().contains("10 attempts"));
    }

    struct EventuallyHealthyModule {
        calls: std::sync::atomic::AtomicU32,
        fail_for: u32,
    }
    impl Module for EventuallyHealthyModule {
        fn is_installed(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn install(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<()> {
            Ok(())
        }
        fn health_check(&self, _: &Dependency) -> Result<()> {
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n < self.fail_for {
                anyhow::bail!("not ready yet")
            } else {
                Ok(())
            }
        }
        fn service_config(&self) -> ServiceConfig {
            ServiceConfig {
                health_check_sleep_ms: 0,
                ..Default::default()
            }
        }
    }

    #[test]
    fn wait_for_ready_retries_until_healthy() {
        let m = EventuallyHealthyModule {
            calls: std::sync::atomic::AtomicU32::new(0),
            fail_for: 2,
        };
        let dep = Dependency::simple("eventually");
        m.wait_for_ready(&dep).unwrap();
        assert!(
            m.calls.load(std::sync::atomic::Ordering::Relaxed) >= 3,
            "Expected at least 3 health_check calls"
        );
    }

    // ── wait_for_stopped ─────────────────────────────────────────────────────

    struct AlreadyStoppedModule;
    impl Module for AlreadyStoppedModule {
        fn is_installed(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn install(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<()> {
            Ok(())
        }
        fn is_running(
            &self,
            _pm: &dyn crate::package_manager::PackageManager,
            _dep: &Dependency,
        ) -> Result<bool> {
            Ok(false)
        }
        fn service_config(&self) -> ServiceConfig {
            ServiceConfig {
                shutdown_sleep_ms: 0,
                ..Default::default()
            }
        }
    }

    #[test]
    fn wait_for_stopped_returns_ok_when_already_stopped() {
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("stoppedservice");
        AlreadyStoppedModule.wait_for_stopped(&pm, &dep).unwrap();
    }

    struct NeverStopsModule;
    impl Module for NeverStopsModule {
        fn is_installed(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn install(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<()> {
            Ok(())
        }
        fn is_running(
            &self,
            _pm: &dyn crate::package_manager::PackageManager,
            _dep: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn service_config(&self) -> ServiceConfig {
            ServiceConfig {
                shutdown_sleep_ms: 0,
                ..Default::default()
            }
        }
    }

    #[test]
    fn wait_for_stopped_fails_after_max_attempts() {
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("stubborn");
        let err = NeverStopsModule.wait_for_stopped(&pm, &dep).unwrap_err();
        assert!(
            err.to_string().contains("stubborn"),
            "error must name the service"
        );
        assert!(
            err.to_string().contains("10 attempts"),
            "error must mention attempt count"
        );
    }

    struct EventuallyStopsModule {
        calls: std::sync::atomic::AtomicU32,
        stop_after: u32,
    }
    impl Module for EventuallyStopsModule {
        fn is_installed(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn install(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<()> {
            Ok(())
        }
        fn is_running(
            &self,
            _pm: &dyn crate::package_manager::PackageManager,
            _dep: &Dependency,
        ) -> Result<bool> {
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(n < self.stop_after)
        }
        fn service_config(&self) -> ServiceConfig {
            ServiceConfig {
                shutdown_sleep_ms: 0,
                ..Default::default()
            }
        }
    }

    #[test]
    fn wait_for_stopped_retries_until_stopped() {
        let pm = crate::package_manager::MockPackageManager::default();
        let m = EventuallyStopsModule {
            calls: std::sync::atomic::AtomicU32::new(0),
            stop_after: 2,
        };
        let dep = Dependency::simple("slowstopper");
        m.wait_for_stopped(&pm, &dep).unwrap();
        assert!(m.calls.load(std::sync::atomic::Ordering::Relaxed) >= 3);
    }

    struct ZeroShutdownModule;
    impl Module for ZeroShutdownModule {
        fn is_installed(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<bool> {
            Ok(true)
        }
        fn install(
            &self,
            _: &dyn crate::package_manager::PackageManager,
            _: &Dependency,
        ) -> Result<()> {
            Ok(())
        }
        fn service_config(&self) -> ServiceConfig {
            ServiceConfig {
                shutdown_max_attempts: 0,
                ..Default::default()
            }
        }
    }

    #[test]
    fn wait_for_stopped_returns_err_when_max_attempts_is_zero() {
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("zeroservice");
        let result = ZeroShutdownModule.wait_for_stopped(&pm, &dep);
        assert!(
            result.is_err(),
            "must return Err (not silent bail) when shutdown_max_attempts is 0"
        );
        assert!(result.unwrap_err().to_string().contains("zeroservice"));
    }

    // ── node_pkg ──────────────────────────────────────────────────────────────

    #[test]
    fn node_pkg_apt() {
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(node_pkg(&pm), "nodejs");
    }

    #[test]
    fn node_pkg_winget() {
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(node_pkg(&pm), "OpenJS.NodeJS");
    }

    #[test]
    fn node_pkg_brew_default() {
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(node_pkg(&pm), "node");
    }

    // ── PackageModule ─────────────────────────────────────────────────────────

    #[test]
    fn package_module_name_for_apt() {
        let m = PackageModule {
            default: "go",
            apt: "golang-go",
            winget: "GoLang.Go",
            nix: "go",
            nix_versioned: helpers::no_nix_versions,
            nix_unfree: false,
        };
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            ..Default::default()
        };
        assert_eq!(m.name_for(&pm), "golang-go");
    }

    #[test]
    fn package_module_name_for_winget() {
        let m = PackageModule {
            default: "go",
            apt: "golang-go",
            winget: "GoLang.Go",
            nix: "go",
            nix_versioned: helpers::no_nix_versions,
            nix_unfree: false,
        };
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            ..Default::default()
        };
        assert_eq!(m.name_for(&pm), "GoLang.Go");
    }

    #[test]
    fn package_module_name_for_default() {
        let m = PackageModule {
            default: "go",
            apt: "golang-go",
            winget: "GoLang.Go",
            nix: "go",
            nix_versioned: helpers::no_nix_versions,
            nix_unfree: false,
        };
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        assert_eq!(m.name_for(&pm), "go");
    }

    #[test]
    fn package_module_name_for_nix() {
        let m = PackageModule {
            default: "go",
            apt: "golang-go",
            winget: "GoLang.Go",
            nix: "go",
            nix_versioned: helpers::no_nix_versions,
            nix_unfree: false,
        };
        let pm = crate::package_manager::MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        assert_eq!(m.name_for(&pm), "go");
    }

    #[test]
    fn package_module_is_installed_true() {
        let m = PackageModule {
            default: "go",
            apt: "golang-go",
            winget: "GoLang.Go",
            nix: "go",
            nix_versioned: helpers::no_nix_versions,
            nix_unfree: false,
        };
        let pm = crate::package_manager::MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let dep = Dependency::simple("go");
        assert!(m.is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn package_module_is_installed_false() {
        let m = PackageModule {
            default: "go",
            apt: "golang-go",
            winget: "GoLang.Go",
            nix: "go",
            nix_versioned: helpers::no_nix_versions,
            nix_unfree: false,
        };
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("go");
        assert!(!m.is_installed(&pm, &dep).unwrap());
    }

    #[test]
    fn package_module_install_propagates_pm_error() {
        let m = PackageModule {
            default: "go",
            apt: "golang-go",
            winget: "GoLang.Go",
            nix: "go",
            nix_versioned: helpers::no_nix_versions,
            nix_unfree: false,
        };
        let pm = crate::package_manager::MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("go");
        assert!(m.install(&pm, &dep).is_err());
    }

    // ── write_mysql_config ────────────────────────────────────────────────────

    fn write_and_read(port: u16, cli_args: Option<&str>) -> String {
        let dir = crate::test_support::tmp_dir();
        write_mysql_config("apt", "mysql", &dir, port, cli_args);
        std::fs::read_to_string(dir.join("devy.cnf")).unwrap()
        // dir is dropped here, cleaning up automatically
    }

    /// Like `write_and_read` but also returns the warnings emitted.
    fn write_and_read_with_messages(port: u16, cli_args: Option<&str>) -> (String, Vec<String>) {
        let mut content = String::new();
        let msgs = crate::output::with_warn_messages(|| {
            content = write_and_read(port, cli_args);
        });
        (content, msgs)
    }

    /// Like `write_and_read` but also returns the number of warnings emitted.
    fn write_and_read_with_warnings(port: u16, cli_args: Option<&str>) -> (String, usize) {
        let (content, msgs) = write_and_read_with_messages(port, cli_args);
        (content, msgs.len())
    }

    #[test]
    fn write_mysql_config_includes_port() {
        let content = write_and_read(3307, None);
        assert!(content.contains("port = 3307"));
    }

    #[test]
    fn write_mysql_config_accepts_valid_double_dash_arg() {
        let content = write_and_read(3306, Some("--innodb-buffer-pool-size=256M"));
        assert!(content.contains("innodb-buffer-pool-size = 256M"));
    }

    #[test]
    fn write_mysql_config_valid_and_invalid_args() {
        let (content, msgs) =
            write_and_read_with_messages(3306, Some("--innodb-buffer-pool-size=256M bogus"));
        assert!(content.contains("innodb-buffer-pool-size = 256M"));
        assert!(!content.contains("bogus"));
        assert_eq!(
            msgs,
            ["skipped cli_args token bogus: not an allowed server option"]
        );
    }

    #[test]
    fn write_mysql_config_dangerous_options_skipped() {
        let (content, msgs) = write_and_read_with_messages(
            3306,
            Some("--bind-address=0.0.0.0 --skip-grant-tables=1"),
        );
        assert!(!content.contains("0.0.0.0"));
        assert!(content.contains("\nbind-address = 127.0.0.1\n"));
        assert!(!content.contains("skip-grant-tables"));
        assert_eq!(
            msgs,
            [
                "skipped cli_args token --bind-address=0.0.0.0: not an allowed server option",
                "skipped cli_args token --skip-grant-tables=1: not an allowed server option",
            ]
        );
    }

    #[test]
    fn write_mysql_config_skips_unlisted_path_and_plugin_options() {
        for token in [
            "--init-file=/tmp/x.sql",
            "--init_file=/tmp/x.sql",
            "--plugin-load=x.so",
            "--plugin-dir=/tmp",
            "--secure-file-priv=",
            "--datadir=/tmp",
            "--socket=/tmp/s",
            "--user=root",
            "--general-log-file=/etc/passwd",
            "--port=1",
        ] {
            let (content, warns) = write_and_read_with_warnings(3306, Some(token));
            assert_eq!(
                content,
                "# devy-managed\n[mysqld]\nport = 3306\n\
                 bind-address = 127.0.0.1\nloose-mysqlx-bind-address = 127.0.0.1\n",
                "{token}"
            );
            assert_eq!(warns, 1, "{token}");
        }
    }

    #[test]
    fn write_mysql_config_normalizes_underscores() {
        let content = write_and_read(3306, Some("--sql_mode=ANSI --max_connections=5"));
        assert!(content.contains("sql-mode = ANSI"));
        assert!(content.contains("max-connections = 5"));
    }

    #[test]
    fn write_mysql_config_rejects_bare_key_value_without_double_dash() {
        let (content, warns) = write_and_read_with_warnings(3306, Some("max_connections=1"));
        assert!(!content.contains("max_connections"));
        assert!(warns > 0, "must warn when -- prefix is missing");
    }

    #[test]
    fn write_mysql_config_rejects_key_with_special_chars() {
        let (content, warns) = write_and_read_with_warnings(3306, Some("--bad;key=val"));
        assert!(!content.contains("bad;key"));
        assert!(warns > 0, "must warn when key contains unsafe characters");
    }

    #[test]
    fn write_mysql_config_skips_args_without_equals() {
        let (content, warns) = write_and_read_with_warnings(3306, Some("--skip-name-resolve"));
        assert!(!content.contains("skip-name-resolve"));
        assert!(warns > 0, "must warn when no = is present in arg");
    }

    #[test]
    fn write_mysql_config_treats_newline_in_args_as_separator() {
        // \n is whitespace — split_whitespace splits the arg list on it.
        let (content, warns) =
            write_and_read_with_warnings(3306, Some("--max-connections=10\nline2"));
        assert!(content.contains("max-connections = 10"));
        assert!(!content.contains("line2"));
        assert_eq!(warns, 1, "must warn for the bare 'line2' token");
    }

    #[test]
    fn write_mysql_config_rejects_value_with_null_byte() {
        let (content, msgs) = write_and_read_with_messages(3306, Some("--sql-mode=val\x00ue"));
        assert!(!content.contains("sql-mode"));
        assert_eq!(
            msgs,
            ["skipped cli_args token --sql-mode=val\\0ue: not an allowed server option"],
            "the NUL is escaped in the warning"
        );
    }

    #[test]
    fn write_mysql_config_carriage_return_splits_token() {
        // \r is ASCII whitespace; split_whitespace splits "--sql-mode=val\rue" into
        // "--sql-mode=val" (accepted) and "ue" (no -- prefix, dropped with a warning).
        let (content, warns) = write_and_read_with_warnings(3306, Some("--sql-mode=val\rue"));
        assert!(content.contains("sql-mode = val\n"));
        assert!(!content.contains("ue"), "portion after \\r is dropped");
        assert_eq!(warns, 1, "must warn for the bare 'ue' token");
    }

    #[test]
    fn write_mysql_config_accepts_value_without_newline_or_null() {
        let content = write_and_read(3306, Some("--max-connections=200"));
        assert!(content.contains("max-connections = 200"));
    }

    #[test]
    fn mysql_allowlist_entries_are_normalized() {
        for key in helpers::MYSQL_ALLOWED_ARGS {
            assert!(
                key.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
                "{key}"
            );
        }
    }

    #[test]
    fn write_mysql_config_brew_keeps_existing_my_cnf() {
        let dir = crate::test_support::tmp_dir();
        let original = "[mysqld]\nbind-address = 127.0.0.1\n";
        std::fs::write(dir.join("my.cnf"), original).unwrap();
        let msgs = crate::output::with_warn_messages(|| {
            write_mysql_config("brew", "mysql", &dir, 3307, Some("--max-connections=9"));
        });
        assert_eq!(
            std::fs::read_to_string(dir.join("my.cnf")).unwrap(),
            original,
            "a user's my.cnf must never be overwritten"
        );
        let devy = std::fs::read_to_string(dir.join("my.cnf.d").join("devy.cnf")).unwrap();
        assert_eq!(
            devy,
            "[mysqld]\nport = 3307\nmax-connections = 9\n\
             bind-address = 127.0.0.1\nloose-mysqlx-bind-address = 127.0.0.1\n"
        );
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(
            msgs[0].starts_with("mysql: "),
            "names the service: {msgs:?}"
        );
        assert!(msgs[0].contains("!includedir"), "{msgs:?}");
        assert!(msgs[0].contains("left it unchanged"), "{msgs:?}");
    }

    /// An unreadable `my.cnf` (here a directory) is warned about only while devy's
    /// settings need the include; at the defaults `up` stays quiet.
    #[test]
    fn write_mysql_config_brew_unreadable_my_cnf_warns_only_when_customized() {
        let dir = crate::test_support::tmp_dir();
        std::fs::create_dir(dir.join("my.cnf")).unwrap();
        let msgs = crate::output::with_warn_messages(|| {
            write_mysql_config("brew", "mariadb", &dir, 3306, None);
        });
        assert!(msgs.is_empty(), "{msgs:?}");
        let msgs = crate::output::with_warn_messages(|| {
            write_mysql_config("brew", "mariadb", &dir, 3307, None);
        });
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(
            msgs[0].starts_with("mariadb: could not read") && msgs[0].contains("!includedir"),
            "{msgs:?}"
        );
        assert!(dir.join("my.cnf").is_dir(), "left unchanged");
    }

    #[test]
    fn write_mysql_config_brew_silent_when_my_cnf_includes_dir() {
        let dir = crate::test_support::tmp_dir();
        let include = dir.join("my.cnf.d");
        let original = format!("[mysqld]\n\n!includedir {}/\n", include.display());
        std::fs::write(dir.join("my.cnf"), &original).unwrap();
        let warns = crate::output::with_warn_capture(|| {
            write_mysql_config("brew", "mysql", &dir, 3307, None);
        });
        assert_eq!(warns, 0);
        assert_eq!(
            std::fs::read_to_string(dir.join("my.cnf")).unwrap(),
            original
        );
        assert!(include.join("devy.cnf").is_file());
    }

    #[test]
    fn write_mysql_config_brew_creates_missing_my_cnf_with_include() {
        let dir = crate::test_support::tmp_dir();
        let warns = crate::output::with_warn_capture(|| {
            write_mysql_config("brew", "mysql", &dir, 3307, None);
        });
        assert_eq!(warns, 0);
        assert_eq!(
            std::fs::read_to_string(dir.join("my.cnf")).unwrap(),
            format!("!includedir {}\n", dir.join("my.cnf.d").display())
        );
    }

    #[test]
    fn write_mysql_config_binds_loopback_after_user_args() {
        for pm in ["apt", "brew"] {
            let dir = crate::test_support::tmp_dir();
            write_mysql_config(pm, "mysql", &dir, 3307, Some("--max-connections=9"));
            let path = if pm == "brew" {
                dir.join("my.cnf.d").join("devy.cnf")
            } else {
                dir.join("devy.cnf")
            };
            let text = std::fs::read_to_string(path).unwrap();
            let user = text.find("max-connections").unwrap();
            let bind = text.find("bind-address = 127.0.0.1").unwrap();
            assert!(bind > user, "{pm}: {text}");
            assert!(
                text.contains("loose-mysqlx-bind-address = 127.0.0.1"),
                "{pm}: {text}"
            );
        }
    }

    #[test]
    fn write_mysql_config_brew_warns_on_every_run_while_customized() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("my.cnf"), "[mysqld]\n").unwrap();
        let count = |port| {
            crate::output::with_warn_capture(|| {
                write_mysql_config("brew", "mysql", &dir, port, None);
            })
        };
        assert_eq!(count(3307), 1);
        assert_eq!(count(3307), 1, "the port is still not applied");
        assert_eq!(count(3306), 0, "nothing to apply at the defaults");
    }

    #[test]
    fn mysql_family_post_setup_keeps_brew_loopback_bind_at_defaults() {
        let dir = crate::test_support::tmp_dir();
        let pm = crate::package_manager::MockPackageManager {
            name: "brew",
            config_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        let devy_cnf = dir.join("my.cnf.d").join("devy.cnf");
        helpers::mysql_family_post_setup(&pm, "mysql", 3307, None).unwrap();
        helpers::mysql_family_post_setup(&pm, "mysql", 3306, None).unwrap();
        let text = std::fs::read_to_string(&devy_cnf).unwrap();
        assert!(text.contains("port = 3306"), "{text}");
        assert!(text.contains("bind-address = 127.0.0.1"), "{text}");
        assert!(
            std::fs::read_to_string(dir.join("my.cnf"))
                .unwrap()
                .contains("!includedir")
        );
    }

    #[test]
    fn write_mysql_config_brew_defaults_do_not_warn_about_stock_my_cnf() {
        let dir = crate::test_support::tmp_dir();
        let stock = "[mysqld]\nbind-address = 127.0.0.1\n";
        std::fs::write(dir.join("my.cnf"), stock).unwrap();
        let warns = crate::output::with_warn_capture(|| {
            write_mysql_config("brew", "mysql", &dir, 3306, None);
        });
        assert_eq!(warns, 0);
        assert_eq!(std::fs::read_to_string(dir.join("my.cnf")).unwrap(), stock);
    }

    #[test]
    fn write_mysql_config_brew_recreates_missing_my_cnf_even_when_unchanged() {
        let dir = crate::test_support::tmp_dir();
        write_mysql_config("brew", "mysql", &dir, 3307, None);
        std::fs::remove_file(dir.join("my.cnf")).unwrap();
        write_mysql_config("brew", "mysql", &dir, 3307, None);
        assert!(
            std::fs::read_to_string(dir.join("my.cnf"))
                .unwrap()
                .contains("!includedir")
        );
    }

    #[test]
    fn mysql_family_post_setup_leaves_apt_file_alone_at_defaults() {
        let dir = crate::test_support::tmp_dir();
        let user_cnf = "[mysqld]\n# the user's own settings\nmax_connections = 7\n";
        std::fs::write(dir.join("my.cnf"), user_cnf).unwrap();
        let pm = crate::package_manager::MockPackageManager {
            name: "apt",
            config_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        helpers::mysql_family_post_setup(&pm, "mysql", 3306, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("my.cnf")).unwrap(),
            user_cnf,
            "at defaults the apt conf.d file must not be rewritten"
        );
        assert_eq!(
            std::fs::read_dir(&*dir).unwrap().count(),
            1,
            "nothing else is written"
        );
    }

    // ── run_cmd ───────────────────────────────────────────────────────────────

    #[test]
    fn run_cmd_succeeds_on_true() {
        assert!(run_cmd("true", &[]).is_ok());
    }

    #[test]
    fn run_cmd_fails_on_false() {
        assert!(run_cmd("false", &[]).is_err());
    }

    #[test]
    fn run_cmd_error_message_contains_program_name() {
        let err = run_cmd("false", &["--arg"]).unwrap_err();
        assert!(err.to_string().contains("false"));
    }

    // ── get: match arm identity for "gh"/"github-cli" ─────────────────────────

    #[test]
    fn get_gh_routes_to_package_module_not_generic_on_winget() {
        // PackageModule maps "gh"/"github-cli" to "GitHub.cli" on winget.
        // GenericModule would pass the raw dep name "gh" through — would not match "GitHub.cli".
        let pm = crate::package_manager::MockPackageManager {
            name: "winget",
            installed_pkg: Some("GitHub.cli"),
            ..Default::default()
        };
        let dep = Dependency::simple("gh");
        assert!(get("gh").is_installed(&pm, &dep).unwrap());
        assert!(get("github-cli").is_installed(&pm, &dep).unwrap());
    }
}
