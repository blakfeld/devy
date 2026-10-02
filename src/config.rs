use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

pub type ExtraValue = serde_yml::Value;

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
        let path = Self::find_config(&start).ok_or_else(|| {
            anyhow::anyhow!("devy.yml not found — are you inside a devy project?")
        })?;
        Self::load(&path)
    }

    /// Load config from the nearest `devy.yml` and return both the config and its
    /// parent directory (the project root). Use this instead of the 7-line inline
    /// pattern that was scattered across command modules.
    pub fn load_with_root() -> Result<(Self, std::path::PathBuf)> {
        let start = std::env::current_dir().context("Failed to get current directory")?;
        let config_path = Self::find_config(&start).ok_or_else(|| {
            anyhow::anyhow!("devy.yml not found — are you inside a devy project?")
        })?;
        let project_root = config_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("devy.yml has no parent directory"))?
            .to_path_buf();
        let config = Self::load(&config_path)?;
        Ok((config, project_root))
    }

    /// Walks from `start` up to the nearest `.git` root or `$HOME`,
    /// whichever comes first, looking for `devy.yml`.
    ///
    /// Stopping at the git root prevents a malicious or unrelated `devy.yml` planted
    /// in a parent directory from being picked up and having its hooks executed.
    pub(crate) fn find_config(start: &std::path::Path) -> Option<std::path::PathBuf> {
        let home = std::env::var("HOME")
            .ok()
            .map(std::path::PathBuf::from)
            .map(|h| h.canonicalize().unwrap_or(h));
        let mut dir = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        loop {
            let candidate = dir.join("devy.yml");
            if candidate.exists() {
                return Some(candidate);
            }
            if dir.join(".git").exists() {
                return None;
            }
            if home.as_deref() == Some(dir.as_path()) {
                return None;
            }
            dir = dir.parent()?.to_path_buf();
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_yml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))
    }

    pub fn normalized_dependencies(&self) -> Result<Vec<Dependency>> {
        let mut result = Vec::new();
        for raw in &self.dependencies {
            match raw {
                RawDependency::Simple(name) => {
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
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"node"));
        assert!(names.contains(&"python"));
    }

    #[test]
    fn normalized_deps_version_preserved() {
        let yaml = "dependencies:\n  - node:\n      version: \"20\"\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        assert_eq!(deps[0].version, Some("20".into()));
    }

    #[test]
    fn normalized_deps_after_install_preserved() {
        let yaml =
            "dependencies:\n  - mysql:\n      after_install: \"mysql_secure_installation\"\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
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
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
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
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let deps = config.normalized_dependencies().unwrap();
        assert!(deps[0].shell.is_none());
    }

    #[test]
    fn normalized_deps_after_install_absent_is_none() {
        let yaml = "dependencies:\n  - node\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
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
        Ok(serde_yml::from_str(yaml)?)
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

    // ── DevyConfig::find_config ───────────────────────────────────────────────

    #[test]
    fn find_config_finds_file_at_git_root_from_subdirectory() {
        // Layout: root/.git, root/devy.yml, root/a/b/ (start)
        let root = tmp_dir();
        let sub = root.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("devy.yml"), "name: test\n").unwrap();

        let found = DevyConfig::find_config(&sub);
        let expected = root.canonicalize().unwrap().join("devy.yml");
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn find_config_stops_at_git_root_when_no_devy_yml() {
        // Layout: root/.git (no devy.yml), root/a/ (start)
        let root = tmp_dir();
        let sub = root.join("a");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let found = DevyConfig::find_config(&sub);
        assert!(found.is_none());
    }

    #[test]
    fn find_config_finds_devy_yml_in_current_dir() {
        // devy.yml in the start dir itself — no walking needed.
        let root = tmp_dir();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("devy.yml"), "name: test\n").unwrap();

        let found = DevyConfig::find_config(&root);
        let expected = root.canonicalize().unwrap().join("devy.yml");
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn find_config_returns_none_when_no_git_and_no_devy_yml_up_to_home() {
        // Layout: root/ (used as HOME bound), root/inner/ (start). No .git, no devy.yml.
        // Pass root as HOME via the env var so the walk is bounded without relying on the
        // real HOME — and we never mutate CWD, so no cross-test races.
        let root = tmp_dir();
        let sub = root.join("inner");
        std::fs::create_dir_all(&sub).unwrap();

        // Temporarily override HOME. Serialise via ENV_LOCK so this test doesn't
        // race with other tests that read $HOME (e.g. rustup_bin(), rbenv_root()).
        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let orig_home = std::env::var("HOME").ok();
        // SAFETY: serialised by ENV_LOCK; no other test mutates $HOME concurrently.
        unsafe { std::env::set_var("HOME", root.to_str().unwrap()) };

        let found = DevyConfig::find_config(&sub);

        unsafe {
            match orig_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }

        assert!(
            found.is_none(),
            "find_config must return None when no .git and no devy.yml exist up to HOME"
        );
    }

    #[test]
    fn find_config_home_guard_fires_when_home_dir_missing() {
        // HOME points to a path that does not exist on disk; the guard must still fire.
        // Layout: fake_home/ (doesn't exist on disk), fake_home/inner/ (start).
        // The walk goes: inner → fake_home. At fake_home the HOME guard must stop it
        // before walking further. fake_home is set as HOME but never created on disk.
        let root = tmp_dir();
        let fake_home = root.join("nonexistent_home");
        let sub = fake_home.join("inner");
        // Only create `sub` (and its parents up to root); fake_home itself is NOT created.
        std::fs::create_dir_all(&sub).unwrap();

        let _guard = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let orig_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", fake_home.to_str().unwrap()) };

        // Walk: sub → fake_home. The HOME guard must fire at fake_home and return None.
        // Without the fix the guard never fires (raw vs. canonical mismatch) and the
        // walk continues up through root and eventually to `/`.
        let found = DevyConfig::find_config(&sub);

        unsafe {
            match orig_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }

        assert!(
            found.is_none(),
            "find_config must stop at HOME even when HOME directory does not exist"
        );
    }
}
