use anyhow::{Context, Result};
use serde::Deserialize;
use serde_norway as yaml;
use std::collections::HashMap;
use std::path::Path;

use crate::validate;

pub type ExtraValue = yaml::Value;

/// Package manager selection from `devy.yml`.
/// Serde rejects unknown values at parse time, catching typos early.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PackageManagerChoice {
    #[default]
    Auto,
    Nix,
    Brew,
    Apt,
}

/// What runs service dependencies: the package manager's own service backend, or
/// per-project containers. Set at the top level and optionally per dependency.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceManagerChoice {
    #[default]
    Package,
    Docker,
}

/// The Docker-compatible CLI used for docker-managed services.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContainerCli {
    #[default]
    Docker,
    Podman,
}

impl ContainerCli {
    /// The executable name.
    pub fn binary(self) -> &'static str {
        match self {
            ContainerCli::Docker => "docker",
            ContainerCli::Podman => "podman",
        }
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RawCommand {
    Simple(String),
    Configured {
        cmd: String,
        cwd: Option<String>,
        shell: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct DevyCommand {
    pub cmd: String,
    pub cwd: Option<String>,
    pub shell: String,
}

pub(crate) fn default_shell() -> String {
    if cfg!(target_os = "windows") {
        "cmd".into()
    } else {
        "sh".into()
    }
}

impl From<RawCommand> for DevyCommand {
    fn from(raw: RawCommand) -> Self {
        match raw {
            RawCommand::Simple(cmd) => DevyCommand {
                cmd,
                cwd: None,
                shell: default_shell(),
            },
            RawCommand::Configured { cmd, cwd, shell } => DevyCommand {
                cmd,
                cwd,
                shell: shell.unwrap_or_else(default_shell),
            },
        }
    }
}

// ── Hooks ─────────────────────────────────────────────────────────────────────

/// A hook value — either a single command or a list of commands run in order.
///
/// Accepts any of:
///   before_up: "echo hi"
///   before_up: { cmd: "echo hi", shell: bash }
///   before_up: ["echo one", "echo two"]
///   before_up: ["echo one", { cmd: "echo two", shell: bash }]
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum HookAction {
    Single(RawCommand),
    List(Vec<RawCommand>),
}

impl HookAction {
    pub fn commands(&self) -> &[RawCommand] {
        match self {
            HookAction::Single(cmd) => std::slice::from_ref(cmd),
            HookAction::List(cmds) => cmds.as_slice(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HooksConfig {
    pub before_up: Option<HookAction>,
    pub after_up: Option<HookAction>,
    pub before_down: Option<HookAction>,
    pub after_down: Option<HookAction>,
}

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevyConfig {
    pub name: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<RawDependency>,
    #[serde(default)]
    pub environment: HashMap<String, String>,
    #[serde(default)]
    pub commands: HashMap<String, RawCommand>,
    #[serde(default)]
    pub hooks: HooksConfig,
    /// Optional package manager override. Omit or set to "auto" to let devy detect
    /// the platform default. Unknown values are rejected at parse time.
    #[serde(default)]
    pub package_manager: PackageManagerChoice,
    /// Default backend for service dependencies; each dependency may override it.
    #[serde(default)]
    pub service_manager: ServiceManagerChoice,
    /// CLI used for docker-managed services. Unused when none are docker-managed.
    #[serde(default)]
    pub container_cli: ContainerCli,
}

/// Supports two forms in YAML:
///   - python
///   - mysql:
///     version: "8.1"
///     port: 3307
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RawDependency {
    Simple(String),
    Configured(HashMap<String, Option<DepConfig>>),
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct DepConfig {
    pub version: Option<String>,
    pub tap: Option<String>,
    /// Shell command run immediately after the dependency is freshly installed.
    /// Treat as arbitrary code execution — do not commit values from untrusted sources.
    /// Runs once; will not re-run on subsequent `devy up` calls unless the package has
    /// been fully removed from the system.
    pub after_install: Option<String>,
    /// Shell interpreter used to run `after_install`. Defaults to `sh` (or `cmd` on Windows).
    /// Must be a bare shell name from the allowed list: sh, bash, zsh, fish, cmd, powershell.
    /// Paths (e.g. `/usr/bin/bash`) are not accepted.
    pub shell: Option<String>,
    /// Overrides the top-level `service_manager` for this (service) dependency.
    pub service_manager: Option<ServiceManagerChoice>,
    /// Container image repository (optionally with a tag) for a docker-managed service,
    /// replacing the module's default image, e.g. for a registry mirror.
    pub image: Option<String>,
    /// Module-specific keys (e.g. port, cli_args) are captured here.
    #[serde(flatten)]
    pub extra: HashMap<String, ExtraValue>,
}

/// Normalized, flat representation used throughout the rest of the codebase.
#[derive(Debug, Clone)]
pub struct Dependency {
    pub name: String,
    pub version: Option<String>,
    pub tap: Option<String>,
    pub after_install: Option<String>,
    /// Shell interpreter for `after_install`. `None` means use the platform default.
    pub shell: Option<String>,
    /// Per-dependency image override for docker-managed services.
    pub image: Option<String>,
    pub extra: HashMap<String, ExtraValue>,
    /// True when `version` was pinned from devy.lock rather than written in devy.yml.
    /// Never read from or written to any file.
    pub version_from_lock: bool,
    /// True when the nix backend must allow unfree packages for this install. Set by
    /// `pkg_dep` from `Module::nix_unfree`; never read from or written to any file.
    pub allow_unfree: bool,
    /// True when the nix backend must allow insecure packages for this install. Set by
    /// `pkg_dep` from `Module::nix_insecure`; never read from or written to any file.
    pub allow_insecure: bool,
    /// True when this service runs as a container rather than through the package
    /// manager. Set by `normalized_dependencies`; never read from or written to any file.
    pub docker: bool,
}

impl Dependency {
    pub fn simple(name: &str) -> Self {
        Dependency {
            name: name.to_string(),
            version: None,
            tap: None,
            after_install: None,
            shell: None,
            image: None,
            extra: HashMap::new(),
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
            docker: false,
        }
    }

    #[cfg(test)]
    pub fn with_extra(name: &str, extra: HashMap<String, ExtraValue>) -> Self {
        Self {
            extra,
            ..Self::simple(name)
        }
    }

    pub fn versioned_name(&self) -> String {
        match &self.version {
            Some(v) => format!("{}@{}", self.name, v),
            None => self.name.clone(),
        }
    }
}

impl DevyConfig {
    pub fn load_default() -> Result<Self> {
        let start = std::env::current_dir().context("Failed to get current directory")?;
        Self::load(&Self::locate_config(&start)?)
    }

    /// Load config from the nearest `devy.yml` and return both the config and its
    /// parent directory (the project root). Use this instead of the 7-line inline
    /// pattern that was scattered across command modules.
    pub fn load_with_root() -> Result<(Self, std::path::PathBuf)> {
        let (config_path, project_root) = Self::locate_root()?;
        let config = Self::load(&config_path)?;
        Ok((config, project_root))
    }

    /// The nearest `devy.yml` from the current directory (see [`Self::locate_config`])
    /// and its parent directory, the project root, without reading the file.
    pub fn locate_root() -> Result<(std::path::PathBuf, std::path::PathBuf)> {
        let start = std::env::current_dir().context("Failed to get current directory")?;
        let config_path = Self::locate_config(&start)?;
        let project_root = config_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("devy.yml has no parent directory"))?
            .to_path_buf();
        Ok((config_path, project_root))
    }

    /// Walks from `start` up to the nearest `.git` root or `$HOME`,
    /// whichever comes first, looking for `devy.yml`.
    ///
    /// Stopping at the git root prevents a malicious or unrelated `devy.yml` planted
    /// in a parent directory from being picked up and having its hooks executed. For the
    /// same reason the walk stops at a directory owned by another user (for example a
    /// shared `/tmp`) without looking inside it, and a `devy.yml` owned by another user
    /// is ignored.
    ///
    /// When nothing is found the error names a `devy.yml` skipped because another user
    /// owns it (or its directory), so `sudo devy ...` or a root container over a
    /// user-owned checkout is not misreported as "not inside a devy project".
    pub(crate) fn locate_config(start: &std::path::Path) -> Result<std::path::PathBuf> {
        match Self::discover(start) {
            Discovery::Found(path) => {
                // Executable lookups from here on ignore PATH entries inside the project.
                if let Some(root) = path.parent() {
                    crate::fs_safe::set_project_root(root);
                }
                Ok(path)
            }
            Discovery::NotFound => {
                anyhow::bail!("devy.yml not found — are you inside a devy project?")
            }
            Discovery::Foreign(path) => anyhow::bail!(
                "devy.yml not found — ignoring {} because it is owned by another user",
                path.display()
            ),
            Discovery::Stopped(dir) => anyhow::bail!(
                "devy.yml not found — stopped looking at {} because it is owned by another user",
                dir.display()
            ),
        }
    }

    fn discover(start: &std::path::Path) -> Discovery {
        Self::discover_with_home(start, std::env::var("HOME").ok())
    }

    /// `discover` with the raw `$HOME` value injected.
    fn discover_with_home(start: &std::path::Path, home: Option<String>) -> Discovery {
        let home = home
            .map(std::path::PathBuf::from)
            .map(|h| h.canonicalize().unwrap_or(h));
        Self::find_config_with(start, home.as_deref(), owned_by_current_user)
    }

    /// `locate_config`'s walk with the home directory and the ownership check injected.
    fn find_config_with(
        start: &std::path::Path,
        home: Option<&std::path::Path>,
        is_owned: impl Fn(&std::path::Path) -> bool,
    ) -> Discovery {
        let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        let mut dir = start.clone();
        loop {
            let candidate = dir.join("devy.yml");
            if !is_owned(&dir) {
                // Only an existence check: nothing inside a foreign directory is read.
                // Stopping at a foreign ancestor with no devy.yml (`/opt`, `/tmp`) is the
                // ordinary not-found case; only a foreign start directory (`sudo devy`, a
                // root container over a user's checkout) is worth naming.
                return if candidate.exists() {
                    Discovery::Foreign(candidate)
                } else if dir == start {
                    Discovery::Stopped(dir)
                } else {
                    Discovery::NotFound
                };
            }
            if candidate.exists() {
                return if is_owned(&candidate) {
                    Discovery::Found(candidate)
                } else {
                    Discovery::Foreign(candidate)
                };
            }
            if dir.join(".git").exists() || home == Some(dir.as_path()) {
                return Discovery::NotFound;
            }
            match dir.parent() {
                Some(parent) => dir = parent.to_path_buf(),
                None => return Discovery::NotFound,
            }
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content = crate::yaml_safe::read_capped(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let label = path.display().to_string();
        let config: Self = crate::yaml_safe::from_str_strict(&content, &label)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        config
            .validate()
            .with_context(|| path.display().to_string())?;
        Ok(config)
    }

    /// Checks every value that reaches a process argument, a path or an environment
    /// variable name against the rules in `crate::validate`. Called by `load`, so every
    /// command rejects a hostile `devy.yml` before acting on it.
    pub fn validate(&self) -> Result<()> {
        for (i, raw) in self.dependencies.iter().enumerate() {
            match raw {
                RawDependency::Simple(name) => validate_dependency(i, name, None)?,
                RawDependency::Configured(map) => {
                    for (name, cfg) in map {
                        validate_dependency(i, name, cfg.as_ref())?;
                    }
                }
            }
        }
        let mut names: Vec<&String> = self.commands.keys().collect();
        names.sort();
        for name in names {
            validate::require(
                validate::command_name(name),
                "commands",
                "command name",
                name,
            )?;
            validate_cwd(&format!("commands.{name}.cwd"), &self.commands[name])?;
        }
        let hooks = [
            ("before_up", &self.hooks.before_up),
            ("after_up", &self.hooks.after_up),
            ("before_down", &self.hooks.before_down),
            ("after_down", &self.hooks.after_down),
        ];
        for (label, hook) in hooks {
            for (i, cmd) in hook.iter().flat_map(|h| h.commands()).enumerate() {
                validate_cwd(&format!("hooks.{label}[{i}].cwd"), cmd)?;
            }
        }
        let mut keys: Vec<&String> = self.environment.keys().collect();
        keys.sort();
        for key in keys {
            validate::require(validate::env_key(key), "environment", "key", key)?;
            if let Some(reason) = validate::reserved_env_key(key) {
                anyhow::bail!(
                    "{} ({reason}); remove it from `environment` in devy.yml",
                    validate::invalid("environment", "key", key)
                );
            }
        }
        // Every loaded config normalizes, so what reads its dependencies
        // (`config_diff::summary` among them) never sees an error after `load`.
        self.normalized_dependencies()?;
        Ok(())
    }

    pub fn normalized_dependencies(&self) -> Result<Vec<Dependency>> {
        let mut result = Vec::new();
        for raw in &self.dependencies {
            match raw {
                RawDependency::Simple(name) => {
                    validate_dependency(result.len(), name, None)?;
                    result.push(self.service_backend(Dependency::simple(name), None)?)
                }
                RawDependency::Configured(map) => {
                    if map.len() > 1 {
                        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
                        anyhow::bail!(
                            "dependency entry has multiple keys ({}); \
                             each dependency must be its own list item",
                            keys.join(", ")
                        );
                    }
                    for (name, cfg) in map {
                        validate_dependency(result.len(), name, cfg.as_ref())?;
                        let cfg = cfg.clone().unwrap_or_default();
                        let dep = Dependency {
                            name: name.clone(),
                            version: cfg.version,
                            tap: cfg.tap,
                            after_install: cfg.after_install,
                            shell: cfg.shell,
                            image: cfg.image,
                            extra: cfg.extra,
                            version_from_lock: false,
                            allow_unfree: false,
                            allow_insecure: false,
                            docker: false,
                        };
                        result.push(self.service_backend(dep, cfg.service_manager)?);
                    }
                }
            }
        }
        Ok(result)
    }

    /// Whether there are dependencies and every one is a docker-managed service, so
    /// nothing is installed through the package manager. False for an invalid config.
    pub fn docker_only(&self) -> bool {
        self.normalized_dependencies()
            .is_ok_and(|deps| !deps.is_empty() && deps.iter().all(|d| d.docker))
    }

    /// Validates the per-dependency `service_manager`/`image` settings and marks `dep`
    /// docker-managed when it is a built-in service and its own `service_manager`, or
    /// failing that the top-level one, is `docker`. Non-services always use the package
    /// manager.
    fn service_backend(
        &self,
        mut dep: Dependency,
        own: Option<ServiceManagerChoice>,
    ) -> Result<Dependency> {
        let is_service = crate::modules::get(&dep.name).is_service();
        if !is_service && (own.is_some() || dep.image.is_some()) {
            anyhow::bail!(
                "{}: service_manager and image apply only to built-in services",
                dep.name
            );
        }
        if let Some(image) = dep.image.as_deref() {
            validate_image(&dep.name, image)?;
        }
        dep.docker =
            is_service && own.unwrap_or(self.service_manager) == ServiceManagerChoice::Docker;
        Ok(dep)
    }
}

/// Validates one dependency entry: its name, its version and the module `extra` values
/// that reach tool arguments or paths. `index` is its position in `dependencies`.
fn validate_dependency(index: usize, name: &str, cfg: Option<&DepConfig>) -> Result<()> {
    validate::require(
        validate::dep_name(name),
        &format!("dependencies[{index}]"),
        "dependency name",
        name,
    )?;
    let Some(cfg) = cfg else {
        return Ok(());
    };
    if let Some(version) = cfg.version.as_deref() {
        validate::require(
            validate::version(version),
            &format!("dependencies.{name}.version"),
            "version",
            version,
        )?;
    }
    crate::modules::helpers::validate_extra(name, &cfg.extra)
}

/// Validates the `cwd` of a command or hook entry: relative and inside the project root.
fn validate_cwd(location: &str, cmd: &RawCommand) -> Result<()> {
    if let RawCommand::Configured { cwd: Some(cwd), .. } = cmd {
        validate::require(validate::rel_path_inside(cwd), location, "cwd", cwd)?;
    }
    Ok(())
}

/// Outcome of the `devy.yml` walk in `DevyConfig::find_config_with`.
#[derive(Debug, PartialEq, Eq)]
enum Discovery {
    Found(std::path::PathBuf),
    NotFound,
    /// The walk ended at this `devy.yml` because another user owns it or its directory.
    Foreign(std::path::PathBuf),
    /// The walk started in this directory, which holds no `devy.yml`, and another user
    /// owns it.
    Stopped(std::path::PathBuf),
}

/// Whether `path` is owned by the user running devy. Unix compares the owner uid with
/// the real uid; other platforms have no comparable cheap check and treat every path as
/// owned. A path that cannot be inspected counts as not owned.
#[cfg(unix)]
fn owned_by_current_user(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).is_ok_and(|m| m.uid() == crate::fs_safe::current_uid())
}

#[cfg(not(unix))]
fn owned_by_current_user(_path: &Path) -> bool {
    true
}

/// Rejects an `image` that isn't a plain `[registry/]repository[:tag]`. It is passed to
/// the container CLI as an argument, where a value starting with `-` would read as a flag.
fn validate_image(dep: &str, image: &str) -> Result<()> {
    let valid = image.starts_with(|c: char| c.is_ascii_alphanumeric())
        && image
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':'));
    if !valid {
        anyhow::bail!("{dep}: invalid image '{image}' — expected [registry/]repository[:tag]");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> crate::test_support::TempDir {
        crate::test_support::tmp_dir()
    }

    // ── DevyCommand::from ─────────────────────────────────────────────────────

    #[test]
    fn from_simple_defaults_to_platform_shell() {
        let raw = RawCommand::Simple("echo hi".into());
        let cmd = DevyCommand::from(raw);
        assert_eq!(cmd.cmd, "echo hi");
        assert_eq!(cmd.shell, default_shell());
        assert!(cmd.cwd.is_none());
    }

    #[test]
    fn from_configured_uses_custom_shell_and_cwd() {
        let raw = RawCommand::Configured {
            cmd: "make build".into(),
            cwd: Some("/tmp".into()),
            shell: Some("bash".into()),
        };
        let cmd = DevyCommand::from(raw);
        assert_eq!(cmd.cmd, "make build");
        assert_eq!(cmd.shell, "bash");
        assert_eq!(cmd.cwd, Some("/tmp".into()));
    }

    #[test]
    fn from_configured_shell_none_defaults_to_platform_shell() {
        let raw = RawCommand::Configured {
            cmd: "echo".into(),
            cwd: None,
            shell: None,
        };
        let cmd = DevyCommand::from(raw);
        assert_eq!(cmd.shell, default_shell());
        assert!(cmd.cwd.is_none());
    }

    // ── Dependency::versioned_name ────────────────────────────────────────────

    #[test]
    fn versioned_name_no_version() {
        let dep = Dependency::simple("node");
        assert_eq!(dep.versioned_name(), "node");
    }

    #[test]
    fn versioned_name_with_version() {
        let dep = Dependency {
            name: "node".into(),
            version: Some("20".into()),
            tap: None,
            after_install: None,
            shell: None,
            extra: HashMap::new(),
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
        };
        assert_eq!(dep.versioned_name(), "node@20");
    }

    // ── DevyConfig::normalized_dependencies ──────────────────────────────────

    #[test]
    fn normalized_deps_simple_included() {
        let yaml = "dependencies:\n  - node\n  - python\n";
        let config: DevyConfig = yaml::from_str(yaml).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"node"));
        assert!(names.contains(&"python"));
    }

    #[test]
    fn normalized_deps_version_preserved() {
        let yaml = "dependencies:\n  - node:\n      version: \"20\"\n";
        let config: DevyConfig = yaml::from_str(yaml).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        assert_eq!(deps[0].version, Some("20".into()));
    }

    #[test]
    fn normalized_deps_after_install_preserved() {
        let yaml =
            "dependencies:\n  - mysql:\n      after_install: \"mysql_secure_installation\"\n";
        let config: DevyConfig = yaml::from_str(yaml).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        assert_eq!(
            deps[0].after_install.as_deref(),
            Some("mysql_secure_installation")
        );
    }

    #[test]
    fn normalized_deps_shell_preserved() {
        let yaml =
            "dependencies:\n  - mysql:\n      after_install: \"echo done\"\n      shell: bash\n";
        let config: DevyConfig = yaml::from_str(yaml).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        assert_eq!(deps[0].shell.as_deref(), Some("bash"));
        // shell must not appear in extra once promoted to a first-class field.
        assert!(
            !deps[0].extra.contains_key("shell"),
            "shell must not appear in dep.extra"
        );
    }

    #[test]
    fn normalized_deps_shell_absent_is_none() {
        let yaml = "dependencies:\n  - node\n";
        let config: DevyConfig = yaml::from_str(yaml).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        assert!(deps[0].shell.is_none());
    }

    #[test]
    fn normalized_deps_after_install_absent_is_none() {
        let yaml = "dependencies:\n  - node\n";
        let config: DevyConfig = yaml::from_str(yaml).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        assert!(deps[0].after_install.is_none());
    }

    #[test]
    fn normalized_deps_multi_key_configured_returns_err() {
        let mut map = std::collections::HashMap::new();
        map.insert("mysql".to_string(), None);
        map.insert("redis".to_string(), None);
        let config = DevyConfig {
            name: None,
            dependencies: vec![RawDependency::Configured(map)],
            environment: HashMap::new(),
            commands: HashMap::new(),
            hooks: Default::default(),
            package_manager: Default::default(),
            service_manager: Default::default(),
            container_cli: Default::default(),
        };
        assert!(
            config.normalized_dependencies().is_err(),
            "multi-key Configured entry must return Err"
        );
    }

    #[test]
    fn normalized_deps_single_key_configured_succeeds() {
        let mut map = std::collections::HashMap::new();
        map.insert("mysql".to_string(), None);
        let config = DevyConfig {
            name: None,
            dependencies: vec![RawDependency::Configured(map)],
            environment: HashMap::new(),
            commands: HashMap::new(),
            hooks: Default::default(),
            package_manager: Default::default(),
            service_manager: Default::default(),
            container_cli: Default::default(),
        };
        let deps = config.normalized_dependencies().unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "mysql");
    }

    // ── service_manager / container_cli ───────────────────────────────────────

    fn parse(yaml: &str) -> Result<DevyConfig> {
        Ok(yaml::from_str(yaml)?)
    }

    #[test]
    fn service_manager_and_container_cli_default() {
        let config = parse("dependencies: []\n").unwrap();
        assert_eq!(config.service_manager, ServiceManagerChoice::Package);
        assert_eq!(config.container_cli, ContainerCli::Docker);
    }

    #[test]
    fn service_manager_and_container_cli_valid_values() {
        let config = parse("service_manager: docker\ncontainer_cli: podman\n").unwrap();
        assert_eq!(config.service_manager, ServiceManagerChoice::Docker);
        assert_eq!(config.container_cli, ContainerCli::Podman);
        assert_eq!(config.container_cli.binary(), "podman");
        let config = parse("service_manager: package\ncontainer_cli: docker\n").unwrap();
        assert_eq!(config.service_manager, ServiceManagerChoice::Package);
        assert_eq!(config.container_cli.binary(), "docker");
    }

    #[test]
    fn invalid_service_manager_or_container_cli_is_a_parse_error() {
        assert!(parse("service_manager: kubernetes\n").is_err());
        assert!(parse("service_manager: Docker\n").is_err());
        assert!(parse("container_cli: nerdctl\n").is_err());
        assert!(parse("dependencies:\n  - redis: { service_manager: kubernetes }\n").is_err());
    }

    #[test]
    fn per_dependency_service_manager_and_image_are_typed() {
        let config =
            parse("dependencies:\n  - redis: { service_manager: docker, image: mirror/redis }\n")
                .unwrap();
        let deps = config.normalized_dependencies().unwrap();
        assert_eq!(deps[0].image.as_deref(), Some("mirror/redis"));
        assert!(deps[0].docker);
        assert!(
            deps[0].extra.is_empty(),
            "service_manager and image must not land in extra: {:?}",
            deps[0].extra
        );
    }

    fn docker_flags(yaml: &str) -> Vec<(String, bool)> {
        parse(yaml)
            .unwrap()
            .normalized_dependencies()
            .unwrap()
            .into_iter()
            .map(|d| (d.name, d.docker))
            .collect()
    }

    #[test]
    fn top_level_docker_applies_to_services_only() {
        assert_eq!(
            docker_flags("service_manager: docker\ndependencies:\n  - redis\n  - jq\n"),
            vec![("redis".into(), true), ("jq".into(), false)]
        );
    }

    #[test]
    fn per_dependency_docker_opt_in() {
        assert_eq!(
            docker_flags("dependencies:\n  - postgres: { service_manager: docker }\n  - redis\n"),
            vec![("postgres".into(), true), ("redis".into(), false)]
        );
    }

    #[test]
    fn per_dependency_package_opt_out() {
        assert_eq!(
            docker_flags(
                "service_manager: docker\ndependencies:\n  - redis: { service_manager: package }\n  - mysql\n"
            ),
            vec![("redis".into(), false), ("mysql".into(), true)]
        );
    }

    fn validation_error(yaml: &str) -> String {
        parse(yaml)
            .unwrap()
            .normalized_dependencies()
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn service_manager_rejected_on_non_service() {
        assert_eq!(
            validation_error("dependencies:\n  - node: { service_manager: docker }\n"),
            "node: service_manager and image apply only to built-in services"
        );
        assert_eq!(
            validation_error("dependencies:\n  - node: { image: node }\n"),
            "node: service_manager and image apply only to built-in services"
        );
    }

    #[test]
    fn service_manager_rejected_on_generic_dependency() {
        assert_eq!(
            validation_error("dependencies:\n  - foo: { service_manager: docker }\n"),
            "foo: service_manager and image apply only to built-in services"
        );
    }

    #[test]
    fn top_level_docker_does_not_reject_non_services() {
        let yaml = "service_manager: docker\ndependencies:\n  - node\n  - foo\n";
        assert!(parse(yaml).unwrap().normalized_dependencies().is_ok());
    }

    #[test]
    fn image_must_be_a_plain_reference() {
        for bad in ["--privileged", "redis latest", "redis@sha256:abc", "/redis"] {
            let yaml = format!("dependencies:\n  - redis: {{ image: \"{bad}\" }}\n");
            assert!(
                validation_error(&yaml).contains("invalid image"),
                "{bad} must be rejected"
            );
        }
        let yaml = "dependencies:\n  - redis: { image: \"registry.corp.example:5000/mirror/redis:7.2\" }\n";
        assert!(parse(yaml).unwrap().normalized_dependencies().is_ok());
    }

    /// The first ```yaml block after `heading` in README.md. Line endings are normalized,
    /// since Windows checkouts may convert the README to CRLF.
    fn readme_yaml(heading: &str) -> String {
        let readme = include_str!("../README.md").replace("\r\n", "\n");
        let section = &readme[readme.find(heading).expect("heading in README")..];
        let start = section.find("```yaml\n").expect("yaml block") + "```yaml\n".len();
        let len = section[start..].find("```").expect("closed block");
        section[start..start + len].to_string()
    }

    #[test]
    fn readme_reference_example_parses() {
        let config = parse(&readme_yaml("## devy.yml reference")).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        let postgres = deps.iter().find(|d| d.name == "postgres").unwrap();
        assert!(postgres.docker);
        assert_eq!(
            postgres.image.as_deref(),
            Some("registry.corp.example/mirror/postgres")
        );
        assert!(!deps.iter().find(|d| d.name == "redis").unwrap().docker);
    }

    #[test]
    fn readme_docker_section_example_parses() {
        let config = parse(&readme_yaml("## Running services with Docker or Podman")).unwrap();
        let docker: Vec<(String, bool)> = config
            .normalized_dependencies()
            .unwrap()
            .into_iter()
            .map(|d| (d.name, d.docker))
            .collect();
        assert_eq!(
            docker,
            vec![
                ("node".into(), false),
                ("postgresql".into(), true),
                ("redis".into(), false)
            ]
        );
    }

    #[test]
    fn readme_nix_free_example_parses() {
        let config = parse(&readme_yaml("### Without Nix")).unwrap();
        assert_eq!(config.package_manager, PackageManagerChoice::Brew);
        assert_eq!(config.service_manager, ServiceManagerChoice::Docker);
        let docker: Vec<(String, bool)> = config
            .normalized_dependencies()
            .unwrap()
            .into_iter()
            .map(|d| (d.name, d.docker))
            .collect();
        assert_eq!(
            docker,
            vec![
                ("node".into(), false),
                ("postgresql".into(), true),
                ("redis".into(), true)
            ]
        );
    }

    // ── DevyConfig::load ──────────────────────────────────────────────────────

    #[test]
    fn load_valid_yaml_ok() {
        let dir = tmp_dir();
        let path = dir.join("devy.yml");
        std::fs::write(&path, "name: test\n").unwrap();
        assert!(DevyConfig::load(&path).is_ok());
    }

    #[test]
    fn load_missing_file_err() {
        let path = std::path::Path::new("/nonexistent/devy_test_missing.yml");
        assert!(DevyConfig::load(path).is_err());
    }

    #[test]
    fn load_invalid_yaml_err() {
        let dir = tmp_dir();
        let path = dir.join("devy.yml");
        std::fs::write(&path, "dependencies: [unclosed\n").unwrap();
        assert!(DevyConfig::load(&path).is_err());
    }

    /// `DevyConfig::load` on `doc`, failing the test if it takes long enough to suggest
    /// alias expansion ran.
    fn load_quickly(doc: &str) -> Result<DevyConfig> {
        let dir = tmp_dir();
        let path = dir.join("devy.yml");
        std::fs::write(&path, doc).unwrap();
        let started = std::time::Instant::now();
        let result = DevyConfig::load(&path);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "load took {:?}",
            started.elapsed()
        );
        result
    }

    fn assert_alias_rejected(result: Result<impl std::fmt::Debug>) {
        let msg = format!("{:#}", result.expect_err("aliases must be rejected"));
        assert!(
            msg.contains("devy.yml")
                && msg.contains("anchors and aliases")
                && msg.contains("not supported"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn load_rejects_billion_laughs_in_accepted_field() {
        // Under `dependencies:`, which accepts lists, so only the alias check can stop it.
        let doc = crate::test_support::billion_laughs("dependencies");
        assert_alias_rejected(load_quickly(&doc));
    }

    #[test]
    fn load_rejects_flat_alias_amplification() {
        let doc = crate::test_support::flat_quadratic("dependencies", 2000, 2000);
        assert_alias_rejected(load_quickly(&doc));
    }

    #[test]
    fn load_rejects_a_single_anchor() {
        assert_alias_rejected(load_quickly("name: &n app\n"));
    }

    #[test]
    fn load_accepts_star_and_ampersand_in_strings_and_comments() {
        let doc = "# deps: *all &x\nname: \"*app &x\"\nenvironment:\n  GLOB: '*.rs'\n  NOTE: |\n    *literal &x\ndependencies:\n  - node # *not an alias\n";
        let config = load_quickly(doc).unwrap();
        assert_eq!(config.name.as_deref(), Some("*app &x"));
    }

    #[test]
    fn load_rejects_oversized_file() {
        let doc = format!(
            "name: \"{}\"\n",
            "a".repeat(crate::yaml_safe::MAX_YAML_BYTES)
        );
        let msg = format!("{:#}", load_quickly(&doc).unwrap_err());
        assert!(msg.contains("larger than"), "{msg}");
    }

    #[test]
    fn load_unknown_top_level_key_returns_err() {
        let dir = tmp_dir();
        let path = dir.join("devy.yml");
        std::fs::write(&path, "dependecies:\n  - node\n").unwrap();
        assert!(
            DevyConfig::load(&path).is_err(),
            "unknown top-level key must be rejected"
        );
    }

    #[test]
    fn load_unknown_hook_key_returns_err() {
        let dir = tmp_dir();
        let path = dir.join("devy.yml");
        std::fs::write(&path, "hooks:\n  before_Up: \"echo hi\"\n").unwrap();
        assert!(
            DevyConfig::load(&path).is_err(),
            "typo'd hook name must be rejected, not silently ignored"
        );
    }

    // ── DevyConfig::locate_config ─────────────────────────────────────────────

    #[test]
    fn locate_config_finds_file_at_git_root_from_subdirectory() {
        // Layout: root/.git, root/devy.yml, root/a/b/ (start)
        let root = tmp_dir();
        let sub = root.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("devy.yml"), "name: test\n").unwrap();

        let found = DevyConfig::locate_config(&sub).ok();
        let expected = root.canonicalize().unwrap().join("devy.yml");
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn locate_config_stops_at_git_root_when_no_devy_yml() {
        // Layout: root/.git (no devy.yml), root/a/ (start)
        let root = tmp_dir();
        let sub = root.join("a");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let found = DevyConfig::locate_config(&sub).ok();
        assert!(found.is_none());
    }

    #[test]
    fn locate_config_finds_devy_yml_in_current_dir() {
        // devy.yml in the start dir itself — no walking needed.
        let root = tmp_dir();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("devy.yml"), "name: test\n").unwrap();

        let found = DevyConfig::locate_config(&root).ok();
        let expected = root.canonicalize().unwrap().join("devy.yml");
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn locate_config_returns_none_when_no_git_and_no_devy_yml_up_to_home() {
        // Layout: root/ (used as HOME bound), root/inner/ (start). No .git, no devy.yml.
        // Pass root as HOME so the walk is bounded without relying on the real HOME.
        let root = tmp_dir();
        let sub = root.join("inner");
        std::fs::create_dir_all(&sub).unwrap();

        // HOME is injected rather than set in the environment, which every test
        // thread shares (a trusted HOME there changes `installers::user_home()`).
        let home = root.to_str().unwrap().to_string();
        let found = DevyConfig::discover_with_home(&sub, Some(home));

        assert_eq!(
            found,
            Discovery::NotFound,
            "locate_config must return None when no .git and no devy.yml exist up to HOME"
        );
    }

    #[test]
    fn locate_config_home_guard_fires_when_home_dir_missing() {
        // HOME points to a path that does not exist on disk; the guard must still fire.
        // Layout: fake_home/ (doesn't exist on disk), fake_home/inner/ (start).
        // The walk goes: inner → fake_home. At fake_home the HOME guard must stop it
        // before walking further. fake_home is set as HOME but never created on disk.
        let root = tmp_dir();
        let fake_home = root.join("nonexistent_home");
        let sub = fake_home.join("inner");
        // Only create `sub` (and its parents up to root); fake_home itself is NOT created.
        std::fs::create_dir_all(&sub).unwrap();

        // Walk: sub → fake_home. The HOME guard must fire at fake_home and return None.
        // Without the fix the guard never fires (raw vs. canonical mismatch) and the
        // walk continues up through root and eventually to `/`. HOME is injected
        // rather than set in the shared process environment.
        let home = fake_home.to_str().unwrap().to_string();
        let found = DevyConfig::discover_with_home(&sub, Some(home));

        assert_eq!(
            found,
            Discovery::NotFound,
            "locate_config must stop at HOME even when HOME directory does not exist"
        );
    }

    // ── value validation ──────────────────────────────────────────────────────

    fn invalid(yaml: &str) -> String {
        let config: DevyConfig = yaml::from_str(yaml).unwrap();
        config.validate().unwrap_err().to_string()
    }

    fn load_err(yaml: &str) -> String {
        let dir = tmp_dir();
        let path = dir.join("devy.yml");
        std::fs::write(&path, yaml).unwrap();
        format!("{:#}", DevyConfig::load(&path).unwrap_err())
    }

    #[test]
    fn option_like_dependency_name_is_rejected_on_load() {
        let err = load_err("dependencies:\n  - \"-oDPkg::Pre-Invoke::=id\"\n");
        assert!(
            err.ends_with("dependencies[0]: invalid dependency name \"-oDPkg::Pre-Invoke::=id\""),
            "{err}"
        );
    }

    #[test]
    fn hostile_dependency_names_are_rejected() {
        for name in ["./evil.deb", "evil.deb", "evilorg/tap/formula", "a b"] {
            let err = invalid(&format!("dependencies:\n  - node\n  - {name:?}\n"));
            assert_eq!(
                err,
                format!("dependencies[1]: invalid dependency name {name:?}")
            );
            let err = invalid(&format!(
                "dependencies:\n  - {name:?}:\n      version: \"1\"\n"
            ));
            assert!(err.contains("invalid dependency name"), "{err}");
        }
    }

    #[test]
    fn apt_arch_suffix_is_accepted() {
        let config: DevyConfig = yaml::from_str("dependencies:\n  - libssl-dev:arm64\n").unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn shell_metacharacters_in_version_are_rejected() {
        let err = invalid("dependencies:\n  - deno:\n      version: \"1.0;id\"\n");
        assert_eq!(err, "dependencies.deno.version: invalid version \"1.0;id\"");
    }

    #[test]
    fn normalized_dependencies_validates_names_and_versions() {
        let config: DevyConfig =
            yaml::from_str("dependencies:\n  - deno:\n      version: \"../x\"\n").unwrap();
        assert!(config.normalized_dependencies().is_err());
        let config: DevyConfig = yaml::from_str("dependencies:\n  - \"./evil.deb\"\n").unwrap();
        assert!(config.normalized_dependencies().is_err());
    }

    #[test]
    fn command_name_with_substitution_is_rejected() {
        let err = invalid("commands:\n  \"$(id>/tmp/p)\": echo hi\n");
        assert_eq!(err, "commands: invalid command name \"$(id>/tmp/p)\"");
    }

    #[test]
    fn command_cwd_escaping_the_project_is_rejected() {
        let err = invalid("commands:\n  dev: { cmd: \"npm run dev\", cwd: ../../ }\n");
        assert_eq!(err, "commands.dev.cwd: invalid cwd \"../../\"");
        let err = invalid("commands:\n  dev: { cmd: \"npm run dev\", cwd: /tmp/shared }\n");
        assert_eq!(err, "commands.dev.cwd: invalid cwd \"/tmp/shared\"");
    }

    #[test]
    fn hook_cwd_escaping_the_project_is_rejected() {
        let err = invalid(
            "hooks:\n  before_up:\n    - echo one\n    - { cmd: \"echo two\", cwd: ../.. }\n",
        );
        assert_eq!(err, "hooks.before_up[1].cwd: invalid cwd \"../..\"");
        let err = invalid("hooks:\n  after_down: { cmd: \"x\", cwd: /tmp }\n");
        assert_eq!(err, "hooks.after_down[0].cwd: invalid cwd \"/tmp\"");
    }

    #[test]
    fn invalid_environment_key_is_rejected() {
        let err = invalid("environment:\n  \"A-B\": x\n");
        assert_eq!(err, "environment: invalid key \"A-B\"");
    }

    #[test]
    fn shell_reserved_environment_key_is_rejected() {
        let err = invalid("environment:\n  FOO: bar\n  PWD: /tmp\n");
        assert_eq!(
            err,
            "environment: invalid key \"PWD\" (the shell keeps it for the working directory); remove it from `environment` in devy.yml"
        );
        for key in [
            "IFS",
            "PROMPT_COMMAND",
            "STARSHIP_PROMPT_COMMAND",
            "PS1",
            "XDG_STATE_HOME",
            "__shadowenv_data",
            "EXECIGNORE",
            "FUNCNEST",
            "MAILPATH",
            "HISTFILE",
            "chpwd_functions",
            "fish_key_bindings",
            "__fish_config_dir",
        ] {
            let err = invalid(&format!("environment:\n  {key}: x\n"));
            assert!(
                err.starts_with(&format!("environment: invalid key \"{key}\" (")),
                "{err}"
            );
        }
    }

    #[test]
    fn hostile_module_list_entry_is_rejected_on_load() {
        let err = load_err("dependencies:\n  - rust:\n      targets: [\"./x\"]\n");
        assert!(
            err.ends_with("dependencies.rust.targets: invalid list entry \"./x\""),
            "{err}"
        );
    }

    #[test]
    fn hostile_venv_path_is_rejected_on_load() {
        let err = load_err("dependencies:\n  - python:\n      venv_path: ../../x\n");
        assert!(err.ends_with("invalid path \"../../x\""), "{err}");
    }

    #[test]
    fn valid_config_passes_validation() {
        let config = parse(
            "dependencies:\n  - node:\n      version: \"20\"\n      global_packages: [\"@angular/cli\"]\n  - python:\n      venv_path: .venv\n\
             commands:\n  dev: npm run dev\n  db:migrate: { cmd: x, cwd: ./api }\n\
             hooks:\n  before_up: { cmd: x, cwd: scripts }\n\
             environment:\n  DATABASE_URL: x\n",
        )
        .unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn control_characters_in_invalid_value_are_escaped() {
        let err = invalid("commands:\n  \"a\\x1b[2Jb\": x\n");
        assert!(!err.contains('\x1b'), "{err:?}");
        assert!(err.contains("\\u{1b}"), "{err}");
    }

    #[test]
    fn readme_examples_satisfy_value_validation() {
        let readme = include_str!("../README.md").replace("\r\n", "\n");
        let mut checked = 0;
        for block in readme.split("```yaml\n").skip(1) {
            let body = &block[..block.find("```").expect("closed block")];
            // Some blocks are fragments (alternatives side by side); only whole configs count.
            if let Ok(config) = yaml::from_str::<DevyConfig>(body) {
                config
                    .validate()
                    .unwrap_or_else(|e| panic!("README example fails validation: {e}\n{body}"));
                checked += 1;
            }
        }
        assert!(
            checked >= 3,
            "expected several README examples, checked {checked}"
        );
    }

    // ── locate_config ownership ───────────────────────────────────────────────

    #[test]
    fn find_config_ignores_foreign_owned_devy_yml() {
        let root = tmp_dir();
        let root = root.canonicalize().unwrap();
        let sub = root.join("work").join("app");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("devy.yml"), "name: planted\n").unwrap();
        let planted = root.join("devy.yml");
        let found = DevyConfig::find_config_with(&sub, None, |p| p != planted.as_path());
        assert_eq!(found, Discovery::Foreign(planted));
    }

    #[test]
    fn find_config_stops_at_foreign_owned_directory() {
        let root = tmp_dir();
        let root = root.canonicalize().unwrap();
        let shared = root.join("shared");
        let sub = shared.join("work").join("app");
        std::fs::create_dir_all(&sub).unwrap();
        // devy.yml above the foreign directory is never reached.
        std::fs::write(root.join("devy.yml"), "name: outer\n").unwrap();
        let found = DevyConfig::find_config_with(&sub, None, |p| p != shared.as_path());
        assert_eq!(found, Discovery::NotFound);
        let found = DevyConfig::find_config_with(&shared, None, |p| p != shared.as_path());
        assert_eq!(found, Discovery::Stopped(shared.clone()));
        // With everything owned, the walk reaches it.
        let found = DevyConfig::find_config_with(&sub, None, |_| true);
        assert_eq!(found, Discovery::Found(root.join("devy.yml")));
    }

    #[test]
    fn find_config_does_not_look_inside_foreign_owned_start_directory() {
        let root = tmp_dir();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("devy.yml"), "name: x\n").unwrap();
        let found = DevyConfig::find_config_with(&root, None, |p| p != root.as_path());
        assert_eq!(found, Discovery::Foreign(root.join("devy.yml")));
    }

    #[cfg(unix)]
    #[test]
    fn locate_config_names_nothing_when_no_devy_yml() {
        let root = tmp_dir();
        std::fs::create_dir(root.join(".git")).unwrap();
        let err = DevyConfig::locate_config(&root).unwrap_err().to_string();
        assert_eq!(err, "devy.yml not found — are you inside a devy project?");
    }

    #[cfg(unix)]
    #[test]
    fn owned_by_current_user_is_true_for_own_files() {
        let dir = tmp_dir();
        assert!(owned_by_current_user(&dir));
        assert!(!owned_by_current_user(&dir.join("missing")));
    }
}
