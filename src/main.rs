// Tests write fixtures with `std::fs::write` freely; production code must use
// `fs_safe::write_atomic` (enforced by `disallowed-methods` in clippy.toml).
#![cfg_attr(test, allow(clippy::disallowed_methods))]

mod ai;
mod cli;
mod commands;
mod config;
mod config_diff;
mod env_manager;
mod error;
mod fs_safe;
mod init_detect;
mod installers;
mod lock;
mod modules;
mod output;
mod package_manager;
mod project_env;
mod service_runner;
mod state_dir;
#[cfg(test)]
mod test_support;
mod validate;
mod yaml_safe;

use clap::Parser;
use cli::Cli;

#[cfg_attr(test, mutants::skip)] // entry point — process exit behaviour is not unit-testable
fn main() {
    if let Err(err) = run() {
        if let Some(silent) = err.downcast_ref::<error::SilentExit>() {
            std::process::exit(silent.0);
        }
        if let Some(hinted) = err.downcast_ref::<error::HintedError>() {
            output::error(&format!("{:#}", hinted.inner));
            // devy-generated (and colored), so not cleaned.
            eprintln!("{}", hinted.hint);
            std::process::exit(1);
        }
        output::error(&format!("{err:#}"));
        std::process::exit(1);
    }
}

#[cfg_attr(test, mutants::skip)] // thin delegation; Cli::parse() reads process args, not controllable in unit tests
pub(crate) fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    cli.run()
}
