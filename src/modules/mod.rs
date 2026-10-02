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
use crate::package_manager::PackageManager;
use helpers::{
    PackageModule, extra_port, extra_strs, node_pkg, pm_dep, run_cmd, tcp_ping, write_mysql_config,
};

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
}

impl LaunchSpec {
    pub fn new(exec: &str, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            exec: exec.to_string(),
            args: args.into_iter().collect(),
            env: Vec::new(),
            init: None,
            working_dir: None,
        }
    }
}

/// Longest Unix socket path devy creates: macOS allows 103 bytes in `sun_path`, Linux
/// 107, and servers add suffixes like `.lock`.
const MAX_SOCKET_PATH: usize = 100;

/// Directory for a service's Unix sockets: `data_dir` when `<data_dir>/<socket_name>`
/// fits in `sun_path`, otherwise a short per-project directory under `/tmp`.
/// Deeply nested projects would otherwise fail to start postgres or mysqld.
pub(crate) fn socket_dir(data_dir: &Path, socket_name: &str) -> Result<PathBuf> {
    if path_arg(data_dir).len() + 1 + socket_name.len() <= MAX_SOCKET_PATH {
        return Ok(data_dir.to_path_buf());
    }
    let dir = PathBuf::from(format!("/tmp/devy-{:016x}", fnv1a(&path_arg(data_dir))));
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir)
}

/// 64-bit FNV-1a: a stable hash for short, per-project names (unlike `DefaultHasher`,
/// it doesn't change between Rust releases).
pub(crate) fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
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
    let data_dir = nix_data_dir(project_root, canonical_name(&dep.name));
    std::fs::create_dir_all(&data_dir)
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
pub(crate) fn search_server_launch(exec: &str, port: u16, data_dir: &Path) -> LaunchSpec {
    LaunchSpec::new(
        exec,
        [
            "-E".to_string(),
            "http.host=127.0.0.1".to_string(),
            "-E".to_string(),
            format!("http.port={port}"),
            "-E".to_string(),
            format!("path.data={}", path_arg(&data_dir.join("data"))),
            "-E".to_string(),
            format!("path.logs={}", path_arg(&data_dir.join("logs"))),
        ],
    )
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
/// `dep.version` maps to one.
pub(crate) fn pkg_dep(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
    name: &str,
) -> Dependency {
    if pm.name() == "nix" {
        pm_dep(dep, &nix_install_attr(module, dep, name))
    } else {
        pm_dep(dep, name)
    }
}

/// Installed check for `pkg_dep`. A version pinned from devy.lock is also satisfied by
/// the unversioned attribute already installed at exactly that version, so the package
/// an earlier `devy up` installed isn't reinstalled under its versioned name.
pub(crate) fn pkg_installed(
    module: &dyn Module,
    pm: &dyn PackageManager,
    dep: &Dependency,
    name: &str,
) -> Result<bool> {
    let target = pkg_dep(module, pm, dep, name);
    if pm.is_package_installed(&target)? {
        return Ok(true);
    }
    if dep.version_from_lock && target.name != name {
        let installed = pm.resolved_version(&pm_dep(dep, name))?;
        return Ok(installed.is_some() && installed == dep.version);
    }
    Ok(false)
}

/// Warning for a `devy.yml` version the nix backend can't honor. Versions pinned from
/// devy.lock, and modules that install outside the package manager, never warn.
pub(crate) fn nix_version_warning(dep: &Dependency, pm: &dyn PackageManager) -> Option<String> {
    if pm.name() != "nix" || dep.version_from_lock {
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
    /// itself. Database modules override it to also accept a service config dir.
    fn port_applicable(&self, pm: &dyn PackageManager) -> bool {
        self.is_service() && pm.name() == "nix"
    }

    /// The install source recorded in devy.lock (e.g. "homebrew", "rustup").
    /// Return `None` to derive the source from the active package manager name.
    fn source(&self) -> Option<&'static str> {
        None
    }

    fn is_installed(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<bool>;
    fn install(&self, pm: &dyn PackageManager, dep: &Dependency) -> Result<()>;

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
            "{} did not stop after {} attempts — try stopping it manually or check its logs",
            dep.name,
            max
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

    /// PATH entries to prepend when this module is active.
    /// Emitted as shadowenv `env/prepend-to-pathlist` directives so they compose
    /// correctly with the user's existing PATH.
    fn path_prepends(&self, _dep: &Dependency, _project_root: &std::path::Path) -> Vec<String> {
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
};
static PHP: PackageModule = PackageModule {
    default: "php",
    apt: "php",
    winget: "PHP.PHP",
    nix: "php",
    nix_versioned: helpers::no_nix_versions,
};
static AWSCLI: PackageModule = PackageModule {
    default: "awscli",
    apt: "awscli",
    winget: "Amazon.AWSCLI",
    nix: "awscli2",
    nix_versioned: helpers::no_nix_versions,
};
static GH: PackageModule = PackageModule {
    default: "gh",
    apt: "gh",
    winget: "GitHub.cli",
    nix: "gh",
    nix_versioned: helpers::no_nix_versions,
};
static KUBECTL: PackageModule = PackageModule {
    default: "kubectl",
    apt: "kubectl",
    winget: "Kubernetes.kubectl",
    nix: "kubectl",
    nix_versioned: helpers::no_nix_versions,
};
static HELM: PackageModule = PackageModule {
    default: "helm",
    apt: "helm",
    winget: "Helm.Helm",
    nix: "kubernetes-helm",
    nix_versioned: helpers::no_nix_versions,
};
static TERRAFORM: PackageModule = PackageModule {
    default: "terraform",
    apt: "terraform",
    winget: "Hashicorp.Terraform",
    nix: "terraform",
    nix_versioned: helpers::no_nix_versions,
};
static AZURE_CLI: PackageModule = PackageModule {
    default: "azure-cli",
    apt: "azure-cli",
    winget: "Microsoft.AzureCLI",
    nix: "azure-cli",
    nix_versioned: helpers::no_nix_versions,
};
static SWIFT: PackageModule = PackageModule {
    default: "swift",
    apt: "swift",
    winget: "Swift.Toolchain",
    nix: "swift",
    nix_versioned: helpers::no_nix_versions,
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

/// Returns the canonical registry name for `name`, resolving aliases.
/// `"postgres"` → `"postgresql"`, `"js"` → `"node"`, unknown → unchanged.
pub fn canonical_name(name: &str) -> &str {
    ALIASES_MAP.get(name).copied().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
    fn socket_dir_falls_back_to_short_tmp_dir_for_deep_projects() {
        let deep = PathBuf::from(format!("/{}/.devy/data/postgresql", "x".repeat(90)));
        let dir = socket_dir(&deep, ".s.PGSQL.51000").unwrap();
        assert!(
            path_arg(&dir).starts_with("/tmp/devy-"),
            "{}",
            dir.display()
        );
        assert!(dir.join(".s.PGSQL.65535.lock").as_os_str().len() <= MAX_SOCKET_PATH);
        assert_eq!(
            dir,
            socket_dir(&deep, ".s.PGSQL.51000").unwrap(),
            "must be stable"
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
                "--port=51001",
                "--bind-address=127.0.0.1",
                "--socket=/p/.devy/data/mysql/mysql.sock",
                "--mysqlx=OFF",
                "--innodb-buffer-pool-size=256M",
            ])
        );
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
        for name in ["elasticsearch", "opensearch"] {
            let d = Path::new("/p/d");
            let spec = launch(name, &with_extra(name, &[("port", num(51005))]), d);
            assert_eq!(spec.exec, name);
            assert_eq!(
                spec.args,
                s(&[
                    "-E",
                    "http.host=127.0.0.1",
                    "-E",
                    "http.port=51005",
                    "-E",
                    "path.data=/p/d/data",
                    "-E",
                    "path.logs=/p/d/logs",
                ])
            );
        }
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
        assert_eq!(spec.args[4..], s(&["--master-key", "k3y"]));
    }

    #[test]
    fn minio_launch_console_and_credentials() {
        let d = Path::new("/p/d");
        let spec = launch("minio", &with_extra("minio", &[("port", num(51007))]), d);
        assert_eq!(spec.exec, "minio");
        assert_eq!(
            spec.args,
            s(&["server", "/p/d", "--address", "127.0.0.1:51007"])
        );
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
        assert_eq!(spec.args[4..], s(&["--console-address", ":9001"]));
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
        assert_eq!(spec.args, s(&["-smtp-bind-addr", "127.0.0.1:51008"]));
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
            ..Dependency::simple(name)
        }
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

        assert!(get("postgresql").port_applicable(&nix));
        assert!(get("postgresql").port_applicable(&brew));
        assert!(get("postgresql").port_applicable(&apt));
        assert!(!get("postgresql").port_applicable(&apt_no_pg));
        assert!(!get("postgresql").port_applicable(&winget));

        assert!(get("mysql").port_applicable(&brew));
        assert!(get("mariadb").port_applicable(&apt));
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

    // ── extra_strs ────────────────────────────────────────────────────────────

    #[test]
    fn extra_strs_missing_key_returns_empty() {
        let dep = Dependency::simple("node");
        assert!(extra_strs(&dep, "global_packages").is_empty());
    }

    #[test]
    fn extra_strs_sequence_returns_strings() {
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
        };
        let pkgs = extra_strs(&dep, "global_packages");
        assert_eq!(pkgs, vec!["typescript", "eslint"]);
    }

    #[test]
    fn extra_strs_non_sequence_value_returns_empty() {
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
        };
        assert!(extra_strs(&dep, "global_packages").is_empty());
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
        write_mysql_config(&dir, port, cli_args).unwrap();
        std::fs::read_to_string(dir.join("my.cnf")).unwrap()
        // dir is dropped here, cleaning up automatically
    }

    /// Like `write_and_read` but also returns the number of warnings emitted.
    fn write_and_read_with_warnings(port: u16, cli_args: Option<&str>) -> (String, usize) {
        let mut content = String::new();
        let warn_count = crate::output::with_warn_capture(|| {
            content = write_and_read(port, cli_args);
        });
        (content, warn_count)
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
    fn write_mysql_config_rejects_bare_key_value_without_double_dash() {
        let (content, warns) = write_and_read_with_warnings(3306, Some("skip_grant_tables=1"));
        assert!(!content.contains("skip_grant_tables"));
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
        let (content, warns) = write_and_read_with_warnings(3306, Some("--no-value-here"));
        assert!(!content.contains("no-value-here"));
        assert!(warns > 0, "must warn when no = is present in arg");
    }

    #[test]
    fn write_mysql_config_treats_newline_in_args_as_separator() {
        // \n is whitespace — split_whitespace splits the arg list on it.
        // "--key=value" is valid and written; "line2" has no -- prefix and warns.
        let (content, warns) = write_and_read_with_warnings(3306, Some("--key=value\nline2"));
        assert!(
            content.contains("key = value"),
            "Expected valid arg to be written"
        );
        assert!(
            !content.contains("line2"),
            "Bare token after newline must be skipped"
        );
        assert!(warns > 0, "must warn for the bare 'line2' token");
    }

    #[test]
    fn write_mysql_config_rejects_value_with_null_byte() {
        let (content, warns) = write_and_read_with_warnings(3306, Some("--key=val\x00ue"));
        assert!(!content.contains("val"));
        assert!(warns > 0, "must warn when value contains null byte");
    }

    #[test]
    fn write_mysql_config_carriage_return_splits_token() {
        // \r is ASCII whitespace; split_whitespace splits "--key=val\rue" into
        // "--key=val" (accepted) and "ue" (no -- prefix, dropped with a warning).
        let (content, warns) = write_and_read_with_warnings(3306, Some("--key=val\rue"));
        assert!(
            content.contains("key = val"),
            "portion before \\r is accepted"
        );
        assert!(!content.contains("ue"), "portion after \\r is dropped");
        assert!(warns > 0, "must warn for the bare 'ue' token");
    }

    #[test]
    fn write_mysql_config_accepts_value_without_newline_or_null() {
        let content = write_and_read(3306, Some("--max-connections=200"));
        assert!(content.contains("max-connections = 200"));
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
