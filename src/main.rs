mod cli;
mod commands;
mod config;
mod env_manager;
mod error;
mod lock;
mod modules;
mod output;
mod package_manager;
mod service_runner;
#[cfg(test)]
mod test_support;

use clap::Parser;
use cli::Cli;

#[cfg_attr(test, mutants::skip)] // entry point — process exit behaviour is not unit-testable
fn main() {
    if let Err(err) = run() {
        if let Some(silent) = err.downcast_ref::<error::SilentExit>() {
            std::process::exit(silent.0);
        }
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

#[cfg_attr(test, mutants::skip)] // thin delegation; Cli::parse() reads process args, not controllable in unit tests
pub(crate) fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    cli.run()
}
