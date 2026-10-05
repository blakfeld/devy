//! The project environment: the variables and PATH entries `devy up` writes to the
//! environment file, computed from devy.yml so `devy exec` can apply them directly.

use std::collections::HashMap;
use std::path::Path;

use crate::commands::ports::{self, PortMode};
use crate::config::{Dependency, DevyConfig};
use crate::modules;
use crate::package_manager::PackageManager;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectEnv {
    pub vars: HashMap<String, String>,
    /// PATH entries in order, the package manager's own entries first.
    pub path_prepends: Vec<String>,
}

/// Computes the project environment for `deps`, which must already have lock pins
/// applied and ports resolved.
///
/// Module env vars and PATH entries come first, then `<SERVICE>_HOST` / `<SERVICE>_PORT`
/// for every service, and devy.yml `environment` overrides both.
pub(crate) fn resolve(
    config: &DevyConfig,
    deps: &[Dependency],
    pm: &dyn PackageManager,
    project_root: &Path,
    mode: PortMode,
) -> ProjectEnv {
    let mut module_env: HashMap<String, String> = HashMap::new();
    // PM-level prepends (e.g. .devy/nix-profile/bin) go first so project-local
    // binaries shadow any system copies of the same tools.
    let mut path_prepends: Vec<String> = pm.path_prepends(project_root);
    for dep in deps {
        let m = modules::get(&dep.name);
        module_env.extend(m.env_vars(dep, project_root));
        module_env.extend(m.backend_env_vars(dep, pm, project_root, mode));
        path_prepends.extend(m.path_prepends(dep, project_root));
    }

    // Emit <SERVICE>_HOST and <SERVICE>_PORT for every service dep.
    // These run after module env_vars so service-specific vars (REDIS_URL, etc.) are
    // already present; config.environment can still override everything via merge_env.
    for dep in deps {
        let m = modules::get(&dep.name);
        if !m.is_service() {
            continue;
        }
        let canonical = modules::canonical_name(&dep.name);
        let prefix = canonical.replace('-', "_").to_ascii_uppercase();
        let resolved = m.port_key().and_then(|key| {
            dep.extra
                .get(key)
                .and_then(|v| v.as_u64())
                .and_then(|raw| u16::try_from(raw).ok())
        });
        let port_opt: Option<u16> = match resolved {
            Some(p) => Some(p),
            // A backend that applies ports always has one after `up`; without one the
            // port is unassigned, not the default.
            None if m.port_key().is_some() && ports::port_applicable(dep, pm) => None,
            None => m.default_port(),
        };
        module_env.insert(format!("{prefix}_HOST"), "127.0.0.1".into());
        if let Some(p) = port_opt {
            module_env.insert(format!("{prefix}_PORT"), p.to_string());
        }
    }

    ProjectEnv {
        vars: merge_env(module_env, &config.environment),
        path_prepends,
    }
}

/// Merge module-supplied env vars with user config env vars, letting config win on conflicts.
pub(crate) fn merge_env(
    module_env: HashMap<String, String>,
    config_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut merged = module_env;
    merged.extend(config_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;
    use serde_norway as yaml;

    #[test]
    fn config_env_overrides_module_env_for_same_key() {
        let module_env = HashMap::from([("FOO".to_string(), "module".to_string())]);
        let config_env = HashMap::from([("FOO".to_string(), "config".to_string())]);
        let merged = merge_env(module_env, &config_env);
        assert_eq!(
            merged.get("FOO").map(String::as_str),
            Some("config"),
            "config.environment must overwrite module env_vars on conflict"
        );
    }

    #[test]
    fn module_env_keys_absent_from_config_are_preserved() {
        let module_env = HashMap::from([("MODULE_ONLY".to_string(), "yes".to_string())]);
        let config_env = HashMap::from([("CONFIG_ONLY".to_string(), "also".to_string())]);
        let merged = merge_env(module_env, &config_env);
        assert_eq!(merged.get("MODULE_ONLY").map(String::as_str), Some("yes"));
        assert_eq!(merged.get("CONFIG_ONLY").map(String::as_str), Some("also"));
    }

    #[test]
    fn resolve_puts_package_manager_entries_first() {
        let config: DevyConfig = yaml::from_str("dependencies:\n  - python\n  - redis\n").unwrap();
        let deps = config.normalized_dependencies().unwrap();
        let pm = MockPackageManager {
            path_prepends_result: vec!["/p/.devy/nix-profile/bin".into()],
            ..Default::default()
        };
        let env = resolve(&config, &deps, &pm, Path::new("/p"), PortMode::ReadOnly);
        assert_eq!(env.path_prepends[0], "/p/.devy/nix-profile/bin");
        assert_eq!(env.path_prepends.len(), 2, "{:?}", env.path_prepends);
        assert!(env.vars.contains_key("VIRTUAL_ENV"));
        assert_eq!(
            env.vars.get("REDIS_HOST").map(String::as_str),
            Some("127.0.0.1")
        );
        assert_eq!(env.vars.get("REDIS_PORT").map(String::as_str), Some("6379"));
    }

    #[test]
    fn resolve_lets_config_environment_win() {
        let config: DevyConfig = yaml::from_str(
            "dependencies:\n  - postgres\nenvironment:\n  DATABASE_URL: postgres://custom\n",
        )
        .unwrap();
        let deps = config.normalized_dependencies().unwrap();
        let env = resolve(
            &config,
            &deps,
            &MockPackageManager::default(),
            Path::new("/p"),
            PortMode::ReadOnly,
        );
        assert_eq!(
            env.vars.get("DATABASE_URL").map(String::as_str),
            Some("postgres://custom")
        );
    }

    #[test]
    fn resolve_omits_port_for_unassigned_service() {
        let config: DevyConfig = yaml::from_str("dependencies:\n  - redis\n").unwrap();
        let mut deps = config.normalized_dependencies().unwrap();
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        crate::commands::ports::resolve_ports(
            &mut deps,
            crate::commands::ports::PortSource::Lock(None),
            &pm,
            crate::commands::ports::PortMode::ReadOnly,
        )
        .unwrap();
        let env = resolve(&config, &deps, &pm, Path::new("/p"), PortMode::ReadOnly);
        assert!(env.vars.contains_key("REDIS_HOST"));
        assert!(!env.vars.contains_key("REDIS_PORT"), "{:?}", env.vars);
    }
}
