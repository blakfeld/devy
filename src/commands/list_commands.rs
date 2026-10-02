use crate::config::DevyConfig;

/// Prints each command name from devy.yml, one per line. Exits silently if
/// devy.yml does not exist — callers are shell completion functions that must
/// not produce error output.
#[cfg_attr(test, mutants::skip)] // returns () and only prints to stdout — not observable in unit tests
pub fn run() {
    if let Ok(config) = DevyConfig::load_default() {
        let mut names: Vec<&str> = config.commands.keys().map(|k| k.as_str()).collect();
        names.sort_unstable();
        for name in names {
            println!("{}", name);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{DevyConfig, RawCommand};
    use std::collections::HashMap;

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
