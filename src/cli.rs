use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::commands;
use crate::commands::export::ExportFormat;
use crate::output;

#[derive(Parser)]
#[command(
    name = "devy",
    about = "Manage developer environments declaratively",
    allow_external_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up the development environment defined in devy.yml
    Up {
        /// Re-resolve all versions and rewrite devy.lock
        #[arg(long)]
        update: bool,
        /// Validate without making changes (same as `devy check`)
        #[arg(long)]
        dry_run: bool,
        /// Install missing package managers automatically (downloads and runs a remote script)
        #[arg(long)]
        bootstrap: bool,
    },
    /// Draft a devy.yml for the current directory with Claude (needs the claude CLI)
    Init {
        /// Overwrite an existing devy.yml
        #[arg(long)]
        force: bool,
        /// Skip Claude: draft devy.yml from project files (version files, compose, package.json, .env.example) only, without network access
        #[arg(long, conflicts_with = "show_context")]
        detect: bool,
        /// Print exactly what would be sent to Claude, then exit without sending it
        #[arg(long)]
        show_context: bool,
    },
    /// List services from devy.yml and their current running status
    Services,
    /// Start a named service
    Start {
        /// Service name as defined in devy.yml
        name: String,
    },
    /// Stop a named service
    Stop {
        /// Service name as defined in devy.yml
        name: String,
    },
    /// Restart a named service (stop then start)
    Restart {
        /// Service name as defined in devy.yml
        name: String,
    },
    /// Show recent log output for a service, or for every service
    Logs {
        /// Service name as defined in devy.yml (default: every service)
        name: Option<String>,
        /// Number of trailing lines to show per service
        #[arg(short = 'n', long, default_value_t = commands::logs::DEFAULT_LINES, value_parser = clap::value_parser!(u32).range(1..))]
        lines: u32,
        /// Keep streaming new log output until interrupted (Ctrl-C)
        #[arg(short, long)]
        follow: bool,
        /// Ask Claude to explain the service's logs and suggest a fix (needs the claude CLI)
        #[arg(long, requires = "name", conflicts_with = "follow")]
        explain: bool,
        /// With --explain, print exactly what would be sent to Claude, then exit without sending it
        #[arg(long, requires = "explain")]
        show_context: bool,
    },
    /// Ask Claude a question about this project's environment (needs the claude CLI)
    Ask {
        /// The question, e.g. "why can't my app reach redis?"
        #[arg(value_parser = non_blank)]
        question: String,
        /// Print exactly what would be sent to Claude, then exit without sending it
        #[arg(long)]
        show_context: bool,
    },
    /// Stop all services defined in devy.yml
    Down {
        /// Also remove docker-managed services' containers and data volumes
        #[arg(long)]
        volumes: bool,
    },
    /// Show install, service, and environment status
    Status,
    /// Validate the environment matches devy.yml without making changes
    Check,
    /// Diagnose the environment and the last failed `devy up`, with Claude when available
    Doctor {
        /// Apply a suggested devy.yml fix without asking
        #[arg(long)]
        yes: bool,
        /// Skip the AI diagnosis and show only devy's own checks (no network access)
        #[arg(long)]
        no_ai: bool,
        /// Print exactly what would be sent to Claude, then exit without sending it
        #[arg(long, conflicts_with_all = ["yes", "no_ai"])]
        show_context: bool,
    },
    /// Print a shell integration snippet to eval in your rc file
    Hook {
        /// Shell to generate the snippet for (zsh, bash, fish)
        shell: String,
    },
    /// Open a GitHub pull request for the current branch in the browser
    Pr,
    /// Export the environment as a Nix shell.nix or flake.nix
    Export {
        /// Output format
        #[arg(long, default_value = "flake")]
        format: ExportFormat,
    },
    /// List commands from devy.yml — used by shell completion, not intended for direct use
    #[command(hide = true, name = "_commands")]
    ListDefined,
    /// List service names from devy.yml — used by shell completion, not intended for direct use
    #[command(hide = true, name = "_services")]
    ListServices,
    /// Run a command defined in devy.yml
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// Rejects an empty or whitespace-only argument as a usage error.
fn non_blank(s: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        Err("must not be empty".into())
    } else {
        Ok(s.to_string())
    }
}

/// Names of devy's own subcommands, which a `commands:` entry cannot shadow.
pub(crate) fn builtin_subcommands() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .chain(["help".to_string()])
        .collect()
}

impl Cli {
    pub fn run(&self) -> Result<()> {
        match &self.command {
            Commands::Up {
                update,
                dry_run: true,
                bootstrap,
            } => {
                if *update {
                    output::warn("--update has no effect with --dry-run; ignoring");
                }
                if *bootstrap {
                    output::warn("--bootstrap has no effect with --dry-run; ignoring");
                }
                commands::check::run()
            }
            Commands::Up {
                update,
                dry_run: false,
                bootstrap,
            } => commands::up::run(*update, *bootstrap),
            Commands::Init {
                force,
                detect,
                show_context,
            } => {
                let mode = if *detect {
                    commands::init::Mode::Detect
                } else {
                    commands::init::Mode::Ai {
                        show_context: *show_context,
                    }
                };
                commands::init::run(mode, *force, std::path::Path::new("devy.yml"))
            }
            Commands::Services => commands::service::list(),
            Commands::Start { name } => commands::service::start(name),
            Commands::Stop { name } => commands::service::stop(name),
            Commands::Restart { name } => commands::service::restart(name),
            Commands::Logs {
                name,
                lines,
                follow,
                explain,
                show_context,
            } => commands::logs::run(commands::logs::Options {
                name: name.clone(),
                lines: *lines,
                follow: *follow,
                explain: *explain,
                show_context: *show_context,
            }),
            Commands::Ask {
                question,
                show_context,
            } => commands::ask::run(question, *show_context),
            Commands::Down { volumes } => commands::down::run(*volumes),
            Commands::Status => commands::status::run(),
            Commands::Check => commands::check::run(),
            Commands::Doctor {
                yes,
                no_ai,
                show_context,
            } => commands::doctor::run(*yes, *no_ai, *show_context),
            Commands::Pr => commands::pr::run(),
            Commands::Export { format } => commands::export::run(*format),
            Commands::Hook { shell } => commands::hook::run(shell),
            Commands::ListDefined => {
                commands::list_commands::run();
                Ok(())
            }
            Commands::ListServices => {
                commands::list_commands::run_services();
                Ok(())
            }
            Commands::External(args) => match args.as_slice() {
                [cmd, extra @ ..] => commands::exec::run(cmd, extra),
                [] => anyhow::bail!("external subcommand name missing"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::Mutex;

    static CD_LOCK: Mutex<()> = Mutex::new(());

    fn with_tempdir<F: FnOnce() -> R, R>(f: F) -> R {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "devy_cli_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _guard = CD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let orig = std::env::current_dir().ok();
        std::env::set_current_dir(&dir).unwrap();
        let result = f();
        if let Some(o) = orig {
            let _ = std::env::set_current_dir(o);
        }
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn cli_run_check_returns_err_without_config() {
        let result = with_tempdir(|| {
            let cli = Cli::parse_from(["devy", "check"]);
            cli.run()
        });
        assert!(
            result.is_err(),
            "check must return Err when no devy.yml exists"
        );
    }

    #[test]
    fn cli_run_hook_returns_ok_for_valid_shell() {
        // Verifies that a successful command actually routes correctly.
        let cli = Cli::parse_from(["devy", "hook", "zsh"]);
        assert!(cli.run().is_ok(), "hook zsh must return Ok");
    }
}
