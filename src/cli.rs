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
    Services {
        /// Print one JSON document instead of the listing
        #[arg(long)]
        json: bool,
    },
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
    Status {
        /// Print one JSON document instead of the tables
        #[arg(long)]
        json: bool,
    },
    /// Validate the environment matches devy.yml without making changes
    Check {
        /// Print one JSON document instead of the tables
        #[arg(long)]
        json: bool,
    },
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
    /// Remove services and containers left behind by removed checkouts
    Prune {
        /// Remove them without asking
        #[arg(long)]
        yes: bool,
        /// Also remove the data volumes of removed containers
        #[arg(long)]
        volumes: bool,
    },
    /// Export the environment as a Nix shell.nix or flake.nix
    Export {
        /// Output format
        #[arg(long, default_value = "flake")]
        format: ExportFormat,
    },
    /// Run a program with the project environment from devy.yml, without a shell
    Exec {
        /// The program to run, followed by its arguments
        #[arg(
            value_name = "PROGRAM",
            trailing_var_arg = true,
            allow_hyphen_values = true,
            required = true
        )]
        argv: Vec<String>,
    },
    /// Write the devy skill for coding agents, and the devy block in AGENTS.md and GEMINI.md
    ///
    /// Writes .claude/skills/devy/SKILL.md for Claude Code, and .agents/skills/devy/SKILL.md
    /// for Codex, Gemini CLI, Cursor, Copilot, Windsurf, OpenCode and Amp. Without --agent
    /// or --all, the shared .agents skill is written only when the project shows signs of
    /// one of those agents (for example .cursor/, .gemini/ or AGENTS.md).
    AgentSetup {
        /// Overwrite the skill files even if devy didn't write them
        #[arg(long)]
        force: bool,
        /// Create AGENTS.md with the devy block when it doesn't exist
        #[arg(long)]
        agents_md: bool,
        /// Write the skill only for this agent (repeatable); turns detection off
        #[arg(long = "agent", value_enum, value_name = "NAME")]
        agents: Vec<commands::agent_setup::AgentName>,
        /// Write the skill for every supported agent
        #[arg(long, conflicts_with = "agents")]
        all: bool,
        /// Print the skill to stdout without writing anything
        #[arg(long, conflicts_with_all = ["force", "agents_md", "agents", "all"])]
        print: bool,
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
                commands::check::run(false)
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
            Commands::Services { json } => {
                if *json {
                    commands::json::disable_color();
                }
                commands::service::list(*json)
            }
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
            Commands::Status { json } => {
                if *json {
                    commands::json::disable_color();
                }
                commands::status::run(*json)
            }
            Commands::Check { json } => {
                if *json {
                    commands::json::disable_color();
                }
                commands::check::run(*json)
            }
            Commands::Doctor {
                yes,
                no_ai,
                show_context,
            } => commands::doctor::run(*yes, *no_ai, *show_context),
            Commands::Pr => commands::pr::run(),
            Commands::Prune { yes, volumes } => commands::prune::run(*yes, *volumes),
            Commands::Export { format } => commands::export::run(*format),
            Commands::Hook { shell } => commands::hook::run(shell),
            Commands::Exec { argv } => commands::exec_env::run(argv),
            Commands::AgentSetup {
                force,
                agents_md,
                agents,
                all,
                print,
            } => commands::agent_setup::run(
                commands::agent_setup::Options {
                    force: *force,
                    agents_md: *agents_md,
                    selection: commands::agent_setup::Selection::from_flags(agents, *all),
                },
                *print,
            ),
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
        // A fresh, empty directory (removed on drop, even if `f` panics). The current
        // directory is process-wide: CD_LOCK only orders these tests, and while one runs,
        // other tests' `fs_safe::project_root()` fallback (no recorded root) sees `dir`.
        // That is harmless for them because `dir` is an empty leaf of the temp dir.
        let dir = crate::test_support::tmp_dir();
        let _guard = CD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Declared after `dir`, so it restores the working directory before `dir` is
        // removed — also when `f` panics, which would otherwise leave the process in a
        // deleted directory.
        struct RestoreCwd(Option<std::path::PathBuf>);
        impl Drop for RestoreCwd {
            fn drop(&mut self) {
                if let Some(o) = &self.0 {
                    let _ = std::env::set_current_dir(o);
                }
            }
        }
        let _restore = RestoreCwd(std::env::current_dir().ok());
        std::env::set_current_dir(&dir).unwrap();
        f()
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

    fn exec_argv(args: &[&str]) -> Vec<String> {
        match Cli::try_parse_from(args).unwrap().command {
            Commands::Exec { argv } => argv,
            _ => panic!("expected exec"),
        }
    }

    #[test]
    fn exec_keeps_program_flags_in_argv() {
        assert_eq!(
            exec_argv(&["devy", "exec", "cargo", "test", "--help"]),
            ["cargo", "test", "--help"]
        );
        assert_eq!(
            exec_argv(&["devy", "exec", "ls", "-la", "--", "x"]),
            ["ls", "-la", "--", "x"]
        );
    }

    #[test]
    fn exec_strips_a_leading_double_dash() {
        assert_eq!(
            exec_argv(&["devy", "exec", "--", "printf", "%s\\n", "$HOME; rm -rf x"]),
            ["printf", "%s\\n", "$HOME; rm -rf x"]
        );
        assert_eq!(
            exec_argv(&["devy", "exec", "--", "--version"]),
            ["--version"]
        );
    }

    #[test]
    fn exec_without_program_is_a_usage_error() {
        let err = Cli::try_parse_from(["devy", "exec"])
            .err()
            .expect("usage error");
        assert_eq!(err.exit_code(), 2);
        let err = Cli::try_parse_from(["devy", "exec", "--"])
            .err()
            .expect("usage error");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn exec_and_agent_setup_are_builtins() {
        let builtins = builtin_subcommands();
        assert!(builtins.contains(&"exec".to_string()));
        assert!(builtins.contains(&"agent-setup".to_string()));
    }

    #[test]
    fn prune_is_a_builtin_with_yes_and_volumes_flags() {
        assert!(builtin_subcommands().contains(&"prune".to_string()));
        for (args, want) in [
            (&["devy", "prune"][..], (false, false)),
            (&["devy", "prune", "--yes"][..], (true, false)),
            (&["devy", "prune", "--volumes"][..], (false, true)),
            (&["devy", "prune", "--volumes", "--yes"][..], (true, true)),
        ] {
            match Cli::try_parse_from(args).unwrap().command {
                Commands::Prune { yes, volumes } => assert_eq!((yes, volumes), want, "{args:?}"),
                _ => panic!("expected prune"),
            }
        }
    }

    #[test]
    fn agent_setup_print_conflicts_with_write_flags() {
        for flag in ["--force", "--agents-md"] {
            let err = Cli::try_parse_from(["devy", "agent-setup", "--print", flag])
                .err()
                .expect("usage error");
            assert_eq!(err.exit_code(), 2);
        }
        assert!(Cli::try_parse_from(["devy", "agent-setup", "--force", "--agents-md"]).is_ok());
        for args in [
            &["--print", "--agent", "codex"][..],
            &["--print", "--all"][..],
        ] {
            let err = Cli::try_parse_from([&["devy", "agent-setup"][..], args].concat())
                .err()
                .expect("usage error");
            assert_eq!(err.exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn agent_setup_agent_repeats_and_conflicts_with_all() {
        use commands::agent_setup::AgentName;
        let cli = Cli::try_parse_from([
            "devy",
            "agent-setup",
            "--agent",
            "cursor",
            "--agent",
            "claude",
            "--force",
        ])
        .unwrap();
        match cli.command {
            Commands::AgentSetup {
                agents, all, force, ..
            } => {
                assert_eq!(agents, [AgentName::Cursor, AgentName::Claude]);
                assert!(!all && force);
            }
            _ => panic!("expected agent-setup"),
        }
        match Cli::try_parse_from(["devy", "agent-setup", "--all", "--agents-md"])
            .unwrap()
            .command
        {
            Commands::AgentSetup { agents, all, .. } => assert!(all && agents.is_empty()),
            _ => panic!("expected agent-setup"),
        }

        let err = Cli::try_parse_from(["devy", "agent-setup", "--agent", "codex", "--all"])
            .err()
            .expect("usage error");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn agent_setup_rejects_unknown_agent_listing_accepted_names() {
        let err = Cli::try_parse_from(["devy", "agent-setup", "--agent", "emacs"])
            .err()
            .expect("usage error");
        assert_eq!(err.exit_code(), 2);
        let msg = err.to_string();
        for name in [
            "claude", "codex", "gemini", "cursor", "copilot", "windsurf", "opencode", "amp",
        ] {
            assert!(msg.contains(name), "{msg}");
        }
    }

    #[test]
    fn cli_run_hook_returns_ok_for_valid_shell() {
        // Verifies that a successful command actually routes correctly.
        let cli = Cli::parse_from(["devy", "hook", "zsh"]);
        assert!(cli.run().is_ok(), "hook zsh must return Ok");
    }
}
