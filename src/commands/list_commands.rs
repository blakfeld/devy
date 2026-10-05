use crate::config::DevyConfig;
use crate::output::clean_line;

/// Prints each command name from devy.yml, one per line. Exits silently if
/// devy.yml does not exist — callers are shell completion functions that must
/// not produce error output.
#[cfg_attr(test, mutants::skip)] // returns () and only prints to stdout — not observable in unit tests
pub fn run() {
    if let Ok(config) = DevyConfig::load_default() {
        let mut names: Vec<&str> = config.commands.keys().map(|k| k.as_str()).collect();
        names.sort_unstable();
        for name in names {
            println!("{}", clean_line(name));
        }
    }
}

/// Prints each service dependency's name from devy.yml, one per line, in declaration
/// order. Silent on any error, like `run`.
#[cfg_attr(test, mutants::skip)] // returns () and only prints to stdout — not observable in unit tests
pub fn run_services() {
    if let Ok(config) = DevyConfig::load_default() {
        for name in service_names(&config) {
            println!("{}", clean_line(&name));
        }
    }
}

fn service_names(config: &DevyConfig) -> Vec<String> {
    config
        .normalized_dependencies()
        .unwrap_or_default()
        .into_iter()
        .filter(|dep| crate::modules::get(&dep.name).is_service())
        .map(|dep| dep.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::config::{DevyConfig, RawCommand};
    use std::collections::HashMap;

    #[test]
    fn service_names_are_services_in_declaration_order() {
        let config =
            crate::test_support::make_config(&["redis", "node", "postgres"], HashMap::new());
        assert_eq!(super::service_names(&config), ["redis", "postgres"]);
    }

    fn config_with_commands(names: &[&str]) -> DevyConfig {
        let commands = names
            .iter()
            .map(|n| (n.to_string(), RawCommand::Simple(format!("echo {}", n))))
            .collect();
        DevyConfig {
            name: None,
            dependencies: vec![],
            environment: HashMap::new(),
            commands,
            hooks: Default::default(),
            package_manager: Default::default(),
            service_manager: Default::default(),
            container_cli: Default::default(),
        }
    }

    #[test]
    fn all_commands_present() {
        let config = config_with_commands(&["build", "test"]);
        let mut names: Vec<&str> = config.commands.keys().map(|k| k.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["build", "test"]);
    }
}
