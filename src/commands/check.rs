use anyhow::{Context, Result};
use colored::Colorize;
use std::path::Path;

use crate::config::DevyConfig;
use crate::env_manager::{EnvManager, Shadowenv};
use crate::error::SilentExit;
use crate::modules;
use crate::output;
use crate::package_manager;
use crate::package_manager::PackageManager;

use super::ports::{self, PortMode};
use super::shared;

pub(crate) fn check_impl(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    env_mgr: &dyn EnvManager,
    project_root: &Path,
) -> Result<()> {
    let project_name = config.name.as_deref().unwrap_or("project");
    output::header(&format!("devy check · {}", project_name));

    let mut issues: usize = 0;

    let deps = config.normalized_dependencies()?;
    // Resolve ports exactly as `up` would, without assigning new ones or writing the lock.
    let lock = ports::load_lock(project_root)?;
    let mut resolved_deps = deps.clone();
    ports::resolve_and_check(&mut resolved_deps, lock.as_ref(), pm, PortMode::ReadOnly)?;

    if !deps.is_empty() {
        for dep in &deps {
            pm.validate_config(dep)
                .with_context(|| format!("{}: config validation failed", dep.name))?;
            let module = modules::get(&dep.name);
            if let Some(known) = module.known_extra_keys() {
                for key in dep.extra.keys() {
                    if !known.contains(&key.as_str()) {
                        issues += 1;
                        let hint = if known.is_empty() {
                            "this module accepts no extra keys".to_string()
                        } else {
                            format!("known keys: {}", known.join(", "))
                        };
                        output::warn(&format!(
                            "{}: unrecognized config key `{}` — {hint}",
                            dep.name, key
                        ));
                    }
                }
            }
            let warnings = module
                .config_warnings(dep)
                .into_iter()
                .chain(ports::unapplied_port_warning(dep, pm))
                .chain(modules::nix_version_warning(dep, pm));
            for warning in warnings {
                output::warn(&format!("{}: {}", dep.name, warning));
            }
            if let Some(shell) = dep.shell.as_deref()
                && let Err(e) = crate::commands::exec::validate_shell(shell)
            {
                issues += 1;
                output::warn(&format!("{}: invalid shell '{}': {}", dep.name, shell, e));
            }
        }
        output::header("Dependencies");
        issues += shared::print_dep_table(&resolved_deps, pm, true)?;
    }

    // Collect PATH prepends from all modules.
    let path_prepends: Vec<String> = deps
        .iter()
        .flat_map(|dep| modules::get(&dep.name).path_prepends(dep, project_root))
        .collect();

    if !config.environment.is_empty() || !path_prepends.is_empty() {
        output::header("Environment");
        let written_vars = env_mgr.read_vars(project_root);
        issues += shared::print_env_table(&config.environment, written_vars, true)?;

        if !path_prepends.is_empty() {
            let written_paths = env_mgr.read_path_prepends(project_root);
            issues += shared::print_path_table(&path_prepends, written_paths, true);
        }
    }

    output::blank_line();
    if issues == 0 {
        output::success("all checks passed");
        Ok(())
    } else {
        let noun = issue_noun(issues);
        eprintln!("  {}  {} {} found", "✗".red().bold(), issues, noun);
        Err(SilentExit(1).into())
    }
}

#[cfg_attr(test, mutants::skip)] // cosmetic singular/plural; no observable behavioral difference
fn issue_noun(count: usize) -> &'static str {
    if count == 1 { "issue" } else { "issues" }
}

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper
pub fn run() -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;
    let pm = package_manager::detect(config.package_manager, &project_root)?;
    let env_mgr = Shadowenv;
    check_impl(&config, pm.as_ref(), &env_mgr, &project_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevyConfig;
    use crate::env_manager::MockEnvManager;
    use crate::package_manager::MockPackageManager;
    use std::collections::HashMap;

    fn make_config(dep_names: &[&str], env: HashMap<String, String>) -> DevyConfig {
        crate::test_support::make_config(dep_names, env)
    }

    #[test]
    fn check_impl_returns_ok_when_all_installed() {
        let config = make_config(&["node"], HashMap::new());

        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_ok());
    }

    #[test]
    fn check_impl_returns_err_when_dep_not_installed() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager::default(); // installed=false
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_err());
    }

    #[test]
    fn check_impl_counts_two_missing_deps_as_issues() {
        let config = make_config(&["node", "python"], HashMap::new());
        let pm = MockPackageManager::default();
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(result.is_err(), "two missing deps must produce errors");
    }

    #[test]
    fn check_impl_empty_deps_skips_dep_table() {
        let config = make_config(&[], HashMap::new());
        let pm = MockPackageManager::default();
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_ok());
    }

    #[test]
    fn check_impl_single_issue_uses_singular_noun() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager::default();
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_err());
    }

    #[test]
    fn check_impl_zero_issues_returns_ok() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_ok());
    }

    #[test]
    fn check_impl_not_installed_service_counts_as_issue() {
        let config = make_config(&["mysql"], HashMap::new()); // mysql is a service
        let pm = MockPackageManager::default(); // installed=false
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "not-installed service must count as an issue"
        );
    }

    #[test]
    fn check_impl_env_issue_without_dep_issues_returns_err() {
        let mut env = HashMap::new();
        env.insert("MY_VAR".to_string(), "value".to_string());
        // Empty deps so the dep block contributes 0 to issues.
        let config = make_config(&[], env);
        let pm = MockPackageManager::default();
        // print_env_table with bold_errors=true and no shadowenv file returns env.len() = 1 issue.
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "a missing env var must be counted as an issue"
        );
    }

    #[test]
    fn check_impl_dep_and_env_issues_both_contribute() {
        let mut env = HashMap::new();
        env.insert("MY_VAR".to_string(), "val".to_string());
        let config = make_config(&["node"], env);
        let pm = MockPackageManager::default(); // nothing installed, no shadowenv file
        assert!(check_impl(&config, &pm, &MockEnvManager::default(), Path::new(".")).is_err());
    }

    #[test]
    fn check_impl_returns_err_on_unrecognized_extra_key() {
        // minio has known_extra_keys; portx is not in the list — must count as an issue.
        let yaml = "dependencies:\n  - minio:\n      portx: 9001\n";
        let config: crate::config::DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "unrecognized extra key must count as an issue"
        );
    }

    #[test]
    fn check_impl_returns_err_on_port_conflict() {
        // mysql and mariadb both default to 3306 — check must catch this, not just up.
        let config = make_config(&["mysql", "mariadb"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "port conflict must be caught by devy check"
        );
    }

    #[test]
    fn check_impl_warns_on_extra_key_for_module_with_empty_allowlist() {
        // ruby has no known extra keys (inherits Some(&[]) default) — any extra key must warn.
        let yaml = "dependencies:\n  - ruby:\n      vrsion: \"3.3.0\"\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let warn_count = crate::output::with_warn_capture(|| {
            let _ = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        });
        assert!(
            warn_count > 0,
            "check_impl must warn on unknown extra key for module with empty allowlist"
        );
    }

    #[test]
    fn check_impl_emits_config_warnings_for_minio_with_credentials() {
        // config_warnings on MinioModule fires when access_key is configured.
        // check_impl must call it and emit the warning via output::warn.
        let yaml = "dependencies:\n  - minio:\n      access_key: myuser\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let warn_count = crate::output::with_warn_capture(|| {
            let _ = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        });
        assert!(
            warn_count > 0,
            "check_impl must emit config_warnings for minio with credentials"
        );
    }

    #[test]
    fn check_impl_returns_err_on_invalid_shell() {
        use crate::config::{DepConfig, RawDependency};
        use std::collections::HashMap as HM;
        let mut map = HM::new();
        map.insert(
            "node".to_string(),
            Some(DepConfig {
                shell: Some("not-a-shell".to_string()),
                ..Default::default()
            }),
        );
        let config = DevyConfig {
            name: None,
            dependencies: vec![RawDependency::Configured(map)],
            environment: HashMap::new(),
            commands: HashMap::new(),
            hooks: Default::default(),
            package_manager: Default::default(),
        };
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let result = check_impl(&config, &pm, &MockEnvManager::default(), Path::new("."));
        assert!(
            result.is_err(),
            "check_impl must return Err when dep.shell is not in the allowed list"
        );
    }

    #[test]
    fn check_impl_warns_on_unhonored_nix_version_without_counting_issue() {
        let dir = crate::test_support::tmp_dir();
        let yaml = "dependencies:\n  - jq:\n      version: \"1.6\"\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            ..Default::default()
        };
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(check_impl(&config, &pm, &MockEnvManager::default(), &dir));
        });
        assert!(
            warnings
                .iter()
                .any(|w| w == "jq: version 1.6 is not supported by the nix backend — installing the nixpkgs default"),
            "{warnings:?}"
        );
        assert!(
            result.unwrap().is_ok(),
            "the warning must not count as an issue"
        );
    }

    #[test]
    fn check_impl_accepts_typescript_global_packages() {
        let yaml = "dependencies:\n  - typescript:\n      global_packages: [eslint]\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(check_impl(
                &config,
                &pm,
                &MockEnvManager::default(),
                Path::new("."),
            ));
        });
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn check_impl_no_conflict_for_unassigned_ports_under_nix() {
        let dir = crate::test_support::tmp_dir();
        let config = make_config(&["mysql", "mariadb"], HashMap::new());
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let result = check_impl(&config, &pm, &MockEnvManager::default(), &dir);
        assert!(result.is_ok(), "{result:?}");
        assert!(
            !dir.join(crate::lock::PATH).exists(),
            "check must never write devy.lock"
        );
    }

    #[test]
    fn check_impl_warns_on_unapplied_explicit_port_without_counting_issue() {
        let dir = crate::test_support::tmp_dir();
        let yaml = "dependencies:\n  - redis:\n      port: 6380\n";
        let config: DevyConfig = serde_yml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            name: "brew",
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(check_impl(&config, &pm, &MockEnvManager::default(), &dir));
        });
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("cannot make redis listen on port 6380 with brew")),
            "{warnings:?}"
        );
        assert!(
            result.unwrap().is_ok(),
            "a warning must not count as an issue"
        );
    }
}
