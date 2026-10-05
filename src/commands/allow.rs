use anyhow::Result;

use crate::config::DevyConfig;
use crate::output;
use crate::trust::{self, Snapshot, Store};

/// `devy allow`: prints the trust summary and records trust for the current project.
/// `devy allow --revoke`: deletes the record and shadowenv's trust files. `allow` fails
/// when `devy.yml` is missing or invalid; `--revoke` only needs to find it, so a project
/// whose `devy.yml` no longer parses can still be revoked.
#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — reads cwd and the real trust store
pub fn run(revoke: bool) -> Result<()> {
    if revoke {
        let (_, project_root) = DevyConfig::locate_root()?;
        let project_root = project_root.as_path();
        // shadowenv keeps its own trust; without this the shell would keep applying the
        // project's environment after devy stopped trusting it. First, so it happens
        // even when the trust store cannot be used.
        trust::untrust_shadowenv(project_root);
        let store = Store::locate()?;
        // Without the copy of its `500_devy.lisp`, the shell hook's guard also refuses a
        // `shadowenv trust` run by hand: shadowenv applies the project only after the
        // next `devy up`.
        crate::env_manager::shadowenv::forget_env_file(project_root, &store.env_copy_dir());
        let root = store.revoke(project_root)?;
        output::success(&format!("revoked trust for {}", root.display()));
        return Ok(());
    }
    let (config, project_root) = DevyConfig::load_with_root()?;
    let store = Store::locate()?;
    // Taken before the summary is printed, so the record is of what was shown.
    let snapshot = Snapshot::take(&project_root, &config)?;
    output::header(&format!(
        "devy allow · {}",
        config.name.as_deref().unwrap_or("project")
    ));
    for line in trust::summary_lines(&config, &project_root) {
        println!("  {line}");
    }
    output::blank_line();
    let root = store.allow(&snapshot)?;
    output::success(&format!("allowed {}", root.display()));
    Ok(())
}
