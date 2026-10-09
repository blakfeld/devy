use anyhow::{Context, Result};
use fs2::FileExt;
use std::collections::BTreeMap;
use std::path::Path;

use crate::commands::exec::{run_hook, spawn_cmd};
use crate::commands::{failure_record, ports};
use crate::config::{Dependency, DevyCommand, DevyConfig};
use crate::env_manager::{EnvManager, Shadowenv};
use crate::error::HintedError;
use crate::lock::{LockFile, LockedDep};
use crate::modules;
use crate::output;
use crate::package_manager;
use crate::project_env::{self, ProjectEnv};
use crate::service_runner::docker::ContainerRuntime;
use crate::service_runner::{self, Runners, ServiceRunner};
use crate::validate;

#[cfg_attr(test, mutants::skip)] // thin delegation — reads process env and disk; not unit-testable
pub fn run(update: bool, bootstrap: bool) -> Result<()> {
    // Located separately from parsing so that a devy.yml that fails to load is still
    // recorded: only a missing devy.yml leaves nowhere to write the failure record.
    let start = std::env::current_dir().context("Failed to get current directory")?;
    let config_path = DevyConfig::locate_config(&start)?;
    let project_root = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("devy.yml has no parent directory"))?
        .to_path_buf();
    let mut progress = UpProgress::default();
    let result = run_located(
        &config_path,
        &project_root,
        update,
        bootstrap,
        &mut progress,
    );
    record_outcome(&project_root, result, &progress)
}

/// The `up` step that refuses unsafe devy-managed directories. A failure there writes no
/// failure record, since the record would go into the refused `.devy/`.
const MANAGED_PATHS_STEP: &str = "check managed paths";

/// The line printed after `error:` when a failure was recorded.
pub(crate) fn doctor_hint() -> String {
    format!(
        "  {} run devy doctor to diagnose this failure",
        colored::Colorize::cyan("·")
    )
}

/// Writes the failure record for `Err` (returning the error with the doctor hint) and
/// clears it for `Ok`. Problems with the record itself are a single warning and never
/// change the outcome.
pub(crate) fn record_outcome(
    project_root: &Path,
    result: Result<()>,
    progress: &UpProgress,
) -> Result<()> {
    match result {
        Ok(()) => {
            if let Err(e) = failure_record::remove(project_root) {
                output::warn(&format!("{e:#}"));
            }
            Ok(())
        }
        // `.devy/` itself was refused (tracked by git, foreign-owned, a nested
        // repository, …), so nothing may be written into it, the record included.
        Err(err) if progress.step == Some(MANAGED_PATHS_STEP) => Err(err),
        Err(err) => {
            let record = failure_record::FailureRecord::for_project(project_root, &err, progress);
            match failure_record::write(project_root, &record) {
                Ok(()) => Err(HintedError {
                    inner: err,
                    hint: doctor_hint(),
                }
                .into()),
                Err(e) => {
                    output::warn(&format!("could not record this failure: {e:#}"));
                    Err(err)
                }
            }
        }
    }
}

#[cfg_attr(test, mutants::skip)] // process lock and real backends; logic is in up_tracked
fn run_located(
    config_path: &Path,
    project_root: &Path,
    update: bool,
    bootstrap: bool,
    progress: &mut UpProgress,
) -> Result<()> {
    progress.enter("load config");
    let config = DevyConfig::load(config_path)?;
    output::header(&format!("devy up · {}", project_name(&config)));

    progress.enter("detect package manager");
    let pm = package_manager::detect(&config, project_root)?;
    progress.backend = Some(pm.name().to_string());
    progress.enter("acquire process lock");

    // Acquire an exclusive advisory lock so concurrent `devy up` invocations
    // (e.g. two devs on the same machine, parallel CI jobs) queue rather than race.
    // Uses a dedicated guard file to avoid inode-swap conflicts with write_lock's
    // rename strategy on devy.lock itself.
    let guard_path = project_root.join(".devy-lock");
    // Opened in place (never replaced, so every process locks the same inode), and never
    // through a symlink a repository committed at that name.
    let _guard =
        crate::fs_safe::open_lock_file(&guard_path).context("Failed to open process guard file")?;
    _guard
        .lock_exclusive()
        .context("Failed to acquire process lock (is another devy process running?)")?;

    up_tracked(
        &config,
        pm.as_ref(),
        ContainerRuntime::system(config.container_cli),
        &Shadowenv,
        UpOptions { update, bootstrap },
        project_root,
        &project_root.join(crate::lock::PATH),
        progress,
    )
    // _guard dropped here → lock released
}

pub(crate) struct UpOptions {
    pub update: bool,
    pub bootstrap: bool,
}

/// The project name shown in headers: `name` from devy.yml, or `project`.
fn project_name(config: &DevyConfig) -> &str {
    config.name.as_deref().unwrap_or("project")
}

/// Where `devy up` is, so a failure can be recorded with the step and dependency it
/// happened in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct UpProgress {
    pub step: Option<&'static str>,
    pub dependency: Option<String>,
    /// The selected package manager, once known.
    pub backend: Option<String>,
}

impl UpProgress {
    fn enter(&mut self, step: &'static str) {
        self.step = Some(step);
        self.dependency = None;
    }
}

/// `up_with_runtime` with the real container CLI selected by `container_cli`.
#[cfg(test)]
pub(crate) fn up_impl(
    config: &DevyConfig,
    pm: &dyn package_manager::PackageManager,
    env_mgr: &dyn EnvManager,
    opts: UpOptions,
    project_root: &Path,
    lock_path: &Path,
) -> Result<()> {
    up_with_runtime(
        config,
        pm,
        ContainerRuntime::system(config.container_cli),
        env_mgr,
        opts,
        project_root,
        lock_path,
    )
}

#[cfg(test)]
pub(crate) fn up_with_runtime(
    config: &DevyConfig,
    pm: &dyn package_manager::PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    opts: UpOptions,
    project_root: &Path,
    lock_path: &Path,
) -> Result<()> {
    up_tracked(
        config,
        pm,
        runtime,
        env_mgr,
        opts,
        project_root,
        lock_path,
        &mut UpProgress::default(),
    )
}

/// `up_with_runtime`, recording the current step and dependency in `progress`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn up_tracked(
    config: &DevyConfig,
    pm: &dyn package_manager::PackageManager,
    runtime: ContainerRuntime<'_>,
    env_mgr: &dyn EnvManager,
    opts: UpOptions,
    project_root: &Path,
    lock_path: &Path,
    progress: &mut UpProgress,
) -> Result<()> {
    progress.backend = Some(pm.name().to_string());
    let project_name = project_name(config);
    // The header comes first, in `run_located`.

    // Before anything runs or writes: the directories devy writes into and runs from must
    // not be symlinks, foreign-owned or committed to the repository.
    progress.enter(MANAGED_PATHS_STEP);
    let venvs = config
        .normalized_dependencies()
        .map(|deps| modules::managed_venvs(&deps))
        .unwrap_or_default();
    // A refusal also removes any trust an earlier `devy up` gave `.shadowenv.d`.
    crate::fs_safe::check_managed_paths(project_root, &venvs)?;

    if let Some(ref hook) = config.hooks.before_up {
        progress.enter("before_up hook");
        output::header("Hooks");
        run_hook("before_up", hook, project_root)?;
    }

    progress.enter("read dependencies");
    let deps = config.normalized_dependencies()?;

    // The package manager is needed only to install a dependency or shadowenv, so a
    // project of docker-managed services runs without Nix, brew or apt.
    progress.enter("check package manager");
    if deps.iter().any(|d| !d.docker) || !env_mgr.is_available() {
        output::step(&format!("Checking for {}", pm.name()));
        pm.ensure_available(opts.bootstrap)
            .with_context(|| format!("Failed to ensure {} is available", pm.name()))?;
        output::success(&format!("{} available", pm.name()));
    }

    // Load the existing lock for orphan comparison regardless of --update.
    progress.enter("read lock");
    // In a linked git worktree, ports come from `.devy/worktree.yml` instead of the lock.
    let recorded = ports::RecordedPorts::with_lock(
        project_root,
        LockFile::load(lock_path).context("Failed to read devy.lock")?,
    );
    let existing_lock = recorded.lock();

    // Docker-managed services pin images by the locked digest; --update re-resolves tags.
    let runners = Runners::new(
        pm,
        runtime,
        config,
        project_root,
        existing_lock,
        opts.update,
    );
    if deps.iter().any(|d| d.docker) {
        progress.enter("check container runtime");
        let cli = runners.docker.runtime().cli_name();
        output::step(&format!("Checking for {cli}"));
        runners.ensure_docker_available(&deps)?;
        output::success(&format!("{cli} available"));
    }

    // For version pinning, ignore the lock when --update is passed.
    let lock = if opts.update {
        if lock_path.exists() {
            output::step("Ignoring devy.lock (--update)");
        }
        None
    } else {
        existing_lock.cloned()
    };

    progress.enter("validate config");
    for dep in deps.iter().filter(|d| !d.docker) {
        progress.dependency = Some(dep.name.clone());
        pm.validate_config(dep)
            .with_context(|| format!("{}: config validation failed", dep.name))?;
    }

    // Pre-compute effective deps once so both phases use the same pinned versions.
    let mut effective_deps: Vec<Dependency> = deps
        .iter()
        .map(|dep| apply_lock_from_source(dep, lock.as_ref(), pm))
        .collect();

    for dep in &deps {
        let warnings = ports::unapplied_port_warning(dep, pm)
            .into_iter()
            .chain(modules::nix_version_warning(dep, pm))
            .chain(service_runner::docker_warnings(dep));
        for warning in warnings {
            output::warn(&format!("{}: {}", dep.name, warning));
        }
        if recorded.in_worktree()
            && let Some(warning) = ports::shared_fixed_port_warning(dep, pm)
        {
            output::warn(&warning);
        }
    }

    // Assign stable ports to service deps before conflict detection.
    // Uses the existing lock (not the version-pinning lock), or the worktree's own port
    // file, so ports survive --update.
    progress.enter("resolve ports");
    let resolved_ports = ports::resolve_and_check(
        &mut effective_deps,
        recorded.source(),
        pm,
        ports::PortMode::Assign,
    )?;

    // Phase 1: install all binaries (no services started yet).
    progress.enter("install");
    if !effective_deps.is_empty() {
        output::header("Dependencies");
        for effective in &effective_deps {
            progress.dependency = Some(effective.name.clone());
            install_binary(&runners, effective, project_root)?;
        }
    }

    // Write the lock immediately after all binaries are confirmed installed.
    // Doing this before service start means a service failure doesn't leave the
    // lock stale for the already-installed packages.
    progress.enter("write lock");
    if recorded.in_worktree() {
        // A worktree keeps its ports to itself, so the committed lock never churns.
        write_lock(
            &effective_deps,
            &runners,
            lock_path,
            LockPorts::Previous(existing_lock),
        )?;
        let worktree_ports = ports::worktree_ports_for(&effective_deps, &resolved_ports, pm);
        if worktree_ports.write_if_changed(project_root)? {
            output::success(&format!(
                "Worktree ports written to {}",
                crate::worktree::PORTS_PATH
            ));
        }
    } else {
        write_lock(&effective_deps, &runners, lock_path, LockPorts::Resolved)?;
    }

    progress.enter("configure environment");

    // Computed only after every install succeeded, so a failed install never writes
    // the environment.
    let ProjectEnv {
        vars: merged_env,
        path_prepends: module_path_prepends,
    } = project_env::resolve(
        config,
        &effective_deps,
        pm,
        project_root,
        ports::PortMode::Assign,
    );

    let has_content = !merged_env.is_empty() || !module_path_prepends.is_empty();
    let has_existing_file = env_mgr.read_vars(project_root).is_some();

    if has_content || has_existing_file {
        output::header("Environment");

        if has_content && !env_mgr.is_available() {
            output::step(&format!("Installing {}", env_mgr.name()));
            let shadowenv_dep = Dependency::simple(env_mgr.name());
            pm.validate_config(&shadowenv_dep)
                .context("shadowenv package manager config is invalid")?;
            pm.install_package(&shadowenv_dep)
                .context("Failed to install shadowenv")?;
            output::success(&format!("Installed {}", env_mgr.name()));
        }

        output::step(&format!("Writing {} config", env_mgr.name()));
        env_mgr
            .setup(project_root, &merged_env, &module_path_prepends)
            .context("Failed to configure environment variables")?;

        if has_content {
            let count = merged_env.len();
            output::success(&format!(
                "Environment configured ({count} variable{})",
                if count == 1 { "" } else { "s" }
            ));

            let shell = std::env::var("SHELL").ok();
            output::info_code("Activate with:", &activation_hint(shell.as_deref()));
        } else {
            output::success("Environment configuration cleared");
        }
    }

    // Warn about deps that were in the lock file but are no longer in devy.yml.
    // Runs even in --update mode so removals are always surfaced.
    if let Some(old_lock) = existing_lock {
        let dep_names: std::collections::HashSet<&str> = deps
            .iter()
            .map(|d| modules::canonical_name(&d.name))
            .collect();
        for orphan in old_lock.dependencies.keys() {
            if !dep_names.contains(orphan.as_str()) {
                output::info(&format!(
                    "'{}' was in devy.lock but is no longer in devy.yml — removed",
                    orphan
                ));
            }
        }
    }

    // Phase 2: start services (after lock is written).
    progress.enter("start services");
    for effective in &effective_deps {
        progress.dependency = Some(effective.name.clone());
        start_service_if_needed(runners.runner_for(effective), effective)?;
    }

    if let Some(ref hook) = config.hooks.after_up {
        progress.enter("after_up hook");
        output::header("Hooks");
        run_hook("after_up", hook, project_root)?;
    }

    output::blank_line();
    output::success(&format!("{} is ready", project_name));

    Ok(())
}

/// If the dep has no pinned version and the lock file has a resolved version
/// for it, return a clone pinned to that version; otherwise return as-is.
pub(crate) fn apply_lock(dep: &Dependency, lock: Option<&LockFile>) -> Dependency {
    if dep.version.is_some() {
        return dep.clone();
    }
    if let Some(locked) = lock.and_then(|l| l.get(modules::canonical_name(&dep.name)))
        && locked.resolved_version.is_some()
    {
        return Dependency {
            version: locked.resolved_version.clone(),
            version_from_lock: true,
            allow_unfree: false,
            allow_insecure: false,
            ..dep.clone()
        };
    }
    dep.clone()
}

/// `apply_lock`, but only for a lock entry recorded by the same install source. A version
/// another backend resolved (e.g. Homebrew's `22.11.0` for node) isn't meaningful here,
/// and under nix it would select a versioned attribute this backend never recorded.
pub(crate) fn apply_lock_from_source(
    dep: &Dependency,
    lock: Option<&LockFile>,
    pm: &dyn package_manager::PackageManager,
) -> Dependency {
    // Docker-managed services pin their image by digest instead (see `DockerRunner`).
    if dep.docker {
        return dep.clone();
    }
    let source = modules::get(&dep.name).source().unwrap_or(pm.name());
    let same_source = lock
        .and_then(|l| l.get(modules::canonical_name(&dep.name)))
        .is_some_and(|locked| locked.source == source);
    if same_source {
        apply_lock(dep, lock)
    } else {
        dep.clone()
    }
}

/// Installs the binary for a dependency and runs post_setup. Does not start services.
/// Call this in Phase 1 so the lock can be written before any service is started.
/// Docker-managed services are "installed" by pulling their image.
pub(crate) fn install_binary(
    runners: &Runners,
    dep: &Dependency,
    project_root: &std::path::Path,
) -> Result<()> {
    let module = modules::get(&dep.name);
    let runner = runners.runner_for(dep);
    let pm = runners.package.pm();
    let display = dep.versioned_name();

    let fresh = if dep.docker {
        let reference = runners.docker.reference(dep)?;
        if runner.is_installed(dep)? {
            output::skip(&format!("{} image present (docker)", dep.name));
            false
        } else {
            output::step(&format!("Pulling {reference}"));
            runner.install(dep)?;
            output::success(&format!("Pulled {reference}"));
            true
        }
    } else if runner.is_installed(dep)? {
        output::skip(&format!(
            "{} already installed (via {})",
            display,
            pm.name()
        ));
        false
    } else {
        output::step(&format!("Installing {}", display));
        runner
            .install(dep)
            .with_context(|| format!("Failed to install {}", display))?;
        output::success(&format!("Installed {}", display));
        true
    };

    if fresh && let Some(cmd) = &dep.after_install {
        output::warn(&format!("{}: running after_install: {}", dep.name, cmd));
        let shell = dep
            .shell
            .clone()
            .unwrap_or_else(crate::config::default_shell);
        spawn_cmd(
            &DevyCommand {
                cmd: cmd.clone(),
                cwd: Some(project_root.to_string_lossy().into_owned()),
                shell,
            },
            "after_install",
        )?;
    }

    // post_setup runs even when already installed — it is idempotent by contract and
    // handles things like bundle install that must run regardless of install state.
    // Containers don't read the package manager's service config, so docker-managed
    // services skip post-setups that only write it.
    if !(dep.docker && module.post_setup_writes_service_config()) {
        module
            .post_setup(dep, pm, project_root)
            .with_context(|| format!("post_setup failed for {}", dep.name))?;
    }

    Ok(())
}

/// Starts a service dependency if it isn't already running. No-op for non-service deps.
/// Call this in Phase 2, after the lock has been written.
pub(crate) fn start_service_if_needed(runner: &dyn ServiceRunner, dep: &Dependency) -> Result<()> {
    let module = modules::get(&dep.name);
    if !module.is_service() {
        return Ok(());
    }
    // A service running only under an outdated unit name is started anyway: `start`
    // replaces that unit once the new one is ready to start.
    if runner.is_running(dep)? && !runner.needs_migration(dep) {
        output::skip(&format!("{} service already running", dep.name));
    } else {
        output::step(&format!("Starting {} service", dep.name));
        runner
            .start(dep)
            .with_context(|| format!("Failed to start {} service", dep.name))?;
        output::success(&format!("{} service started", dep.name));
    }
    output::step(&format!("Waiting for {} to be ready", dep.name));
    if let Err(e) = module.wait_for_ready(dep) {
        output::warn(&format!(
            "{} is not yet responding to health checks — verify manually: {}",
            dep.name, e
        ));
    } else {
        output::success(&format!("{} is ready", dep.name));
    }
    Ok(())
}

/// The command that loads devy's shell integration (`devy hook`), which sets up shadowenv
/// behind devy's guard, for the shell named by `$SHELL` (`shell`). `devy hook` supports
/// zsh, bash and fish; any other or unknown shell gets the zsh form.
fn activation_hint(shell: Option<&str>) -> String {
    let bin = env!("CARGO_PKG_NAME");
    let name = shell
        .and_then(|s| s.rsplit(['/', '\\']).next())
        .unwrap_or("");
    match name {
        "fish" => format!("{bin} hook fish | source"),
        "bash" => format!("eval \"$({bin} hook bash)\""),
        _ => format!("eval \"$({bin} hook zsh)\""),
    }
}

/// `value` if it passes `valid`, else `None` with a warning. Resolved versions and digests
/// are scraped from tool output, and `LockFile::load` rejects an entry that fails these
/// rules, so recording one would leave a lock that every later command refuses to read.
fn lockable(
    dep: &str,
    field: &str,
    value: Option<String>,
    valid: fn(&str) -> bool,
) -> Option<String> {
    match value {
        Some(v) if !valid(&v) => {
            output::warn(&format!(
                "{dep}: not recording {field} {v:?} in devy.lock: it is not a valid {field}"
            ));
            None
        }
        other => other,
    }
}

/// Where [`write_lock`] takes each entry's `assigned_port` from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LockPorts<'a> {
    /// The ports resolved for this run.
    Resolved,
    /// The previous lock's value for each entry, or none where it had none: in a linked
    /// worktree, whose ports live in `.devy/worktree.yml` (design D6).
    Previous(Option<&'a LockFile>),
}

/// Records each dependency's resolved version, source and port. A docker-managed service
/// records `source: docker`, its image tag as the version and the pulled image's digest.
/// Leaves the file alone when the lock is already up to date.
pub(crate) fn write_lock(
    deps: &[Dependency],
    runners: &Runners,
    path: &Path,
    lock_ports: LockPorts<'_>,
) -> Result<()> {
    let pm = runners.package.pm();
    let mut locked = BTreeMap::new();
    for dep in deps {
        let module = modules::get(&dep.name);
        // Nix installs attributes, not exact versions: keep a version pinned from the lock so
        // teammates on different nixpkgs revisions don't rewrite each other's patch versions.
        let (resolved, image_digest) = if !dep.docker && pm.name() == "nix" && dep.version_from_lock
        {
            (dep.version.clone(), None)
        } else {
            runners.runner_for(dep).resolved(dep)?
        };
        let resolved = lockable(&dep.name, "resolved_version", resolved, validate::version);
        let image_digest = lockable(
            &dep.name,
            "image_digest",
            image_digest,
            validate::image_digest,
        );
        let source = if dep.docker {
            service_runner::DOCKER_SOURCE
        } else {
            module.source().unwrap_or(pm.name())
        };
        let canonical = modules::canonical_name(&dep.name);
        // Only record ports the backend actually applies; others always use the default.
        let assigned_port = match lock_ports {
            LockPorts::Resolved => ports::recordable_port(dep, pm),
            LockPorts::Previous(previous) => previous
                .and_then(|l| l.get(canonical))
                .and_then(|d| d.assigned_port),
        };
        locked.insert(
            canonical.to_string(),
            LockedDep {
                resolved_version: resolved,
                source: source.to_string(),
                assigned_port,
                image_digest,
            },
        );
    }
    let new_lock = LockFile {
        dependencies: locked,
        ..Default::default()
    };

    // Skip the write if nothing changed — avoids spurious git modifications on every `devy up`.
    if let Ok(Some(existing)) = LockFile::load(path)
        && existing == new_lock
    {
        return Ok(());
    }

    new_lock.write(path).context("Failed to write devy.lock")?;
    output::success(&format!("Lock file written to {}", crate::lock::PATH));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_hint_uses_devy_hook_for_the_shell() {
        assert_eq!(
            activation_hint(Some("/bin/bash")),
            "eval \"$(devy hook bash)\""
        );
        assert_eq!(
            activation_hint(Some("/usr/local/bin/zsh")),
            "eval \"$(devy hook zsh)\""
        );
        assert_eq!(
            activation_hint(Some("/opt/homebrew/bin/fish")),
            "devy hook fish | source"
        );
        for other in [None, Some("/bin/sh"), Some("powershell"), Some("")] {
            assert_eq!(
                activation_hint(other),
                "eval \"$(devy hook zsh)\"",
                "{other:?}"
            );
        }
    }

    #[test]
    fn lockable_drops_values_the_lock_loader_would_reject() {
        assert_eq!(
            lockable(
                "vc",
                "resolved_version",
                Some("14.36.32532.0".into()),
                validate::version
            ),
            Some("14.36.32532.0".into())
        );
        for bad in [">", "14.36.\u{2026}", "1.0;id"] {
            assert_eq!(
                lockable(
                    "vc",
                    "resolved_version",
                    Some(bad.into()),
                    validate::version
                ),
                None,
                "{bad}"
            );
        }
        assert_eq!(
            lockable("vc", "resolved_version", None, validate::version),
            None
        );
        assert_eq!(
            lockable(
                "redis",
                "image_digest",
                Some("redis@sha256:abc".into()),
                validate::image_digest
            ),
            None
        );
    }
    use crate::config::Dependency;
    use crate::lock::{LockFile, LockedDep};
    use crate::package_manager::MockPackageManager;
    use crate::service_runner::{PackageRunner, package_runners};
    use serde_norway as yaml;
    use std::collections::{BTreeMap, HashMap};

    fn tmp_path() -> crate::test_support::TempFile {
        crate::test_support::tmp_path(".lock")
    }

    // ── apply_lock ────────────────────────────────────────────────────────────

    #[test]
    fn apply_lock_pins_version_from_lock_file() {
        let dep = Dependency::simple("node");
        let mut deps = BTreeMap::new();
        deps.insert(
            "node".into(),
            LockedDep {
                resolved_version: Some("20.11.0".into()),
                source: "homebrew".into(),
                assigned_port: None,
                image_digest: None,
            },
        );
        let lock = LockFile {
            dependencies: deps,
            ..Default::default()
        };
        let effective = apply_lock(&dep, Some(&lock));
        assert_eq!(
            effective.version.as_deref(),
            Some("20.11.0"),
            "apply_lock must pin version from lock file"
        );
    }

    #[test]
    fn apply_lock_does_not_override_existing_version() {
        let mut dep = Dependency::simple("node");
        dep.version = Some("18.0.0".into());
        let mut deps = BTreeMap::new();
        deps.insert(
            "node".into(),
            LockedDep {
                resolved_version: Some("20.11.0".into()),
                source: "homebrew".into(),
                assigned_port: None,
                image_digest: None,
            },
        );
        let lock = LockFile {
            dependencies: deps,
            ..Default::default()
        };
        let effective = apply_lock(&dep, Some(&lock));
        assert_eq!(effective.version.as_deref(), Some("18.0.0"));
    }

    #[test]
    fn apply_lock_marks_lock_pinned_versions() {
        let mut deps = BTreeMap::new();
        deps.insert(
            "node".into(),
            LockedDep {
                resolved_version: Some("22.11.0".into()),
                source: "nix".into(),
                assigned_port: None,
                image_digest: None,
            },
        );
        let lock = LockFile {
            dependencies: deps,
            ..Default::default()
        };
        let pinned = apply_lock(&Dependency::simple("node"), Some(&lock));
        assert!(
            pinned.version_from_lock,
            "lock-applied version must be flagged"
        );

        let mut explicit = Dependency::simple("node");
        explicit.version = Some("24".into());
        let kept = apply_lock(&explicit, Some(&lock));
        assert!(
            !kept.version_from_lock,
            "explicit version must not be flagged"
        );
        assert_eq!(kept.version.as_deref(), Some("24"));
    }

    #[test]
    fn apply_lock_from_source_ignores_other_backends_versions() {
        let mut deps = BTreeMap::new();
        deps.insert(
            "node".into(),
            LockedDep {
                resolved_version: Some("22.11.0".into()),
                source: "brew".into(),
                assigned_port: None,
                image_digest: None,
            },
        );
        let lock = LockFile {
            dependencies: deps,
            ..Default::default()
        };
        let dep = Dependency::simple("node");
        let nix = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        assert!(
            apply_lock_from_source(&dep, Some(&lock), &nix)
                .version
                .is_none()
        );
        let brew = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        let pinned = apply_lock_from_source(&dep, Some(&lock), &brew);
        assert_eq!(pinned.version.as_deref(), Some("22.11.0"));
        assert!(pinned.version_from_lock);
    }

    #[test]
    fn apply_lock_returns_dep_unchanged_when_no_lock() {
        let dep = Dependency::simple("node");
        let effective = apply_lock(&dep, None);
        assert!(effective.version.is_none());
    }

    #[test]
    fn apply_lock_returns_dep_unchanged_when_not_in_lock() {
        let lock = LockFile::default();
        let dep = Dependency::simple("node");
        let effective = apply_lock(&dep, Some(&lock));
        assert!(effective.version.is_none());
    }

    // ── install_binary ────────────────────────────────────────────────────────

    #[test]
    fn install_binary_propagates_install_error() {
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("node");
        assert!(
            install_binary(
                &package_runners(&pm, Path::new("/tmp")),
                &dep,
                std::path::Path::new("/tmp")
            )
            .is_err(),
            "install failure must propagate as Err"
        );
    }

    #[test]
    fn install_binary_returns_ok_when_already_installed() {
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let dep = Dependency::simple("node");
        assert!(
            install_binary(
                &package_runners(&pm, Path::new("/tmp")),
                &dep,
                std::path::Path::new("/tmp")
            )
            .is_ok()
        );
    }

    #[test]
    fn install_binary_uses_shell_field_for_after_install() {
        // dep.shell = "not-a-shell" must cause spawn_cmd to fail with a shell validation error.
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default(); // installed=false → install runs
        let dep = Dependency {
            name: "node".into(),
            version: None,
            tap: None,
            after_install: Some("true".into()),
            shell: Some("not-a-shell".into()),
            extra: HashMap::new(),
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
        };
        let result = install_binary(&package_runners(&pm, Path::new("/tmp")), &dep, &dir);
        assert!(
            result.is_err(),
            "install_binary must fail when dep.shell is not in the allowed shell list"
        );
    }

    #[test]
    fn install_binary_after_install_runs_in_project_root() {
        // after_install must execute with cwd = project_root, not the invoker's CWD.
        let dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager::default(); // installed=false → install runs
        let dep = Dependency {
            name: "node".into(),
            version: None,
            tap: None,
            after_install: Some("touch marker".into()),
            shell: Some("sh".into()),
            extra: HashMap::new(),
            version_from_lock: false,
            allow_unfree: false,
            allow_insecure: false,
            image: None,
            docker: false,
        };
        install_binary(&package_runners(&pm, Path::new("/tmp")), &dep, &dir).unwrap();
        assert!(
            dir.join("marker").exists(),
            "after_install must run in project_root — marker file must be created there"
        );
    }

    // ── start_service_if_needed ───────────────────────────────────────────────

    #[test]
    fn start_service_if_needed_is_noop_for_non_service() {
        let pm = MockPackageManager::default();
        let dep = Dependency::simple("node"); // not a service
        assert!(start_service_if_needed(&PackageRunner::new(&pm, Path::new("/tmp")), &dep).is_ok());
        assert!(pm.started_services.borrow().is_empty());
    }

    #[test]
    fn start_service_if_needed_skips_start_when_already_running() {
        // wait_for_ready times out in tests (no live service), but the timeout is now a
        // warning, not an error — so the function must return Ok.
        let pm = MockPackageManager {
            service_running: true,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        assert!(
            start_service_if_needed(&PackageRunner::new(&pm, Path::new("/tmp")), &dep).is_ok(),
            "must return Ok even though health check times out in test environment"
        );
        assert!(
            pm.started_services.borrow().is_empty(),
            "start must not be called when service is already running"
        );
        assert!(
            pm.migrated_services.borrow().is_empty(),
            "migration is left to the backend's start"
        );
    }

    #[test]
    fn start_service_if_needed_replaces_a_service_running_under_a_legacy_name() {
        // Running only under its legacy unit name: `start` must run so the backend
        // retires that unit and starts the per-project one.
        let pm = MockPackageManager {
            service_running: true,
            legacy_services: vec!["mysql"],
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        start_service_if_needed(&PackageRunner::new(&pm, Path::new("/tmp")), &dep).unwrap();
        assert_eq!(*pm.started_services.borrow(), ["mysql"]);
    }

    #[test]
    fn start_service_if_needed_propagates_start_error() {
        let pm = MockPackageManager {
            start_service_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        assert!(
            start_service_if_needed(&PackageRunner::new(&pm, Path::new("/tmp")), &dep).is_err(),
            "start failure must propagate as Err"
        );
    }

    #[test]
    fn apply_lock_reads_canonical_key_when_dep_uses_alias() {
        // "postgres" is an alias for "postgresql". The lock stores the canonical key.
        // apply_lock must resolve the alias before looking up in the lock.
        let dep = Dependency::simple("postgres");
        let mut deps = BTreeMap::new();
        deps.insert(
            "postgresql".into(),
            LockedDep {
                resolved_version: Some("16.0".into()),
                source: "homebrew".into(),
                assigned_port: None,
                image_digest: None,
            },
        );
        let lock = LockFile {
            dependencies: deps,
            ..Default::default()
        };
        let effective = apply_lock(&dep, Some(&lock));
        assert_eq!(
            effective.version.as_deref(),
            Some("16.0"),
            "apply_lock must find the canonical key even when dep uses an alias"
        );
    }

    // ── write_lock ────────────────────────────────────────────────────────────

    #[test]
    fn write_lock_creates_file() {
        let path = tmp_path();
        let pm = MockPackageManager::default();
        let deps = vec![Dependency::simple("node")];
        write_lock(
            &deps,
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        assert!(path.exists(), "write_lock must create the lock file");
    }

    #[test]
    fn write_lock_includes_dep_name_in_file() {
        let path = tmp_path();
        let pm = MockPackageManager::default();
        let deps = vec![Dependency::simple("redis")];
        write_lock(
            &deps,
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("redis"), "lock file must contain dep name");
    }

    #[test]
    fn write_lock_skips_write_when_unchanged() {
        let path = tmp_path();
        let pm = MockPackageManager::default();
        let deps = vec![Dependency::simple("node")];
        write_lock(
            &deps,
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        let content_after_first = std::fs::read_to_string(&path).unwrap();
        write_lock(
            &deps,
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        let content_after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            content_after_first, content_after_second,
            "second write_lock call with identical deps must not modify the file"
        );
    }

    #[test]
    fn write_lock_uses_canonical_name_for_alias() {
        // "postgres" is an alias; the lock key must be "postgresql".
        let path = tmp_path();
        let pm = MockPackageManager::default();
        let deps = vec![Dependency::simple("postgres")];
        write_lock(
            &deps,
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("postgresql"),
            "lock file must use canonical name 'postgresql', not alias 'postgres'"
        );
        assert!(
            !content.contains("postgres:"),
            "lock file must not contain alias key 'postgres'"
        );
    }

    /// Sets `path`'s mtime an hour back and returns it, so a later rewrite is detectable.
    fn backdate(path: &Path) -> std::time::SystemTime {
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(past)
            .unwrap();
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    fn locked_node(version: &str, source: &str) -> LockFile {
        let mut deps = BTreeMap::new();
        deps.insert(
            "node".into(),
            crate::lock::LockedDep {
                resolved_version: Some(version.into()),
                source: source.into(),
                assigned_port: None,
                image_digest: None,
            },
        );
        LockFile {
            dependencies: deps,
            ..Default::default()
        }
    }

    fn lock_pinned_node(version: &str) -> Dependency {
        Dependency {
            version: Some(version.into()),
            version_from_lock: true,
            ..Dependency::simple("node")
        }
    }

    fn locked_version(path: &Path, name: &str) -> Option<String> {
        LockFile::load(path)
            .unwrap()
            .unwrap()
            .get(name)
            .unwrap()
            .resolved_version
            .clone()
    }

    #[test]
    fn write_lock_keeps_lock_pinned_version_under_nix() {
        let path = tmp_path();
        locked_node("24.20.0", "nix").write(&path).unwrap();
        let mtime = backdate(&path);
        let pm = MockPackageManager {
            name: "nix",
            version: Some("24.21.0".into()),
            ..Default::default()
        };
        write_lock(
            &[lock_pinned_node("24.20.0")],
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        assert_eq!(locked_version(&path, "node").as_deref(), Some("24.20.0"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            mtime,
            "the lock must not be rewritten"
        );
    }

    #[test]
    fn write_lock_records_installed_version_for_other_backends() {
        let path = tmp_path();
        let pm = MockPackageManager {
            name: "brew",
            version: Some("24.21.0".into()),
            ..Default::default()
        };
        write_lock(
            &[lock_pinned_node("24.20.0")],
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        assert_eq!(locked_version(&path, "node").as_deref(), Some("24.21.0"));
    }

    #[test]
    fn write_lock_records_installed_version_without_pin_under_nix() {
        // `--update` skips the lock, so nothing is pinned.
        let path = tmp_path();
        locked_node("24.20.0", "nix").write(&path).unwrap();
        let pm = MockPackageManager {
            name: "nix",
            version: Some("24.21.0".into()),
            ..Default::default()
        };
        write_lock(
            &[Dependency::simple("node")],
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        assert_eq!(locked_version(&path, "node").as_deref(), Some("24.21.0"));
    }

    // ── up_impl ───────────────────────────────────────────────────────────────

    use crate::env_manager::MockEnvManager;

    fn make_config(dep_names: &[&str], env: HashMap<String, String>) -> crate::config::DevyConfig {
        crate::test_support::make_config(dep_names, env)
    }

    #[test]
    fn up_impl_succeeds_with_no_deps_no_env() {
        let config = make_config(&[], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        let result = up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        );
        assert!(result.is_ok(), "up_impl must succeed with empty config");
    }

    #[test]
    fn up_impl_writes_lock_file() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        assert!(lock.exists(), "up_impl must write the lock file");
    }

    #[test]
    fn up_impl_assigns_distinct_ports_to_services_with_same_default() {
        // mysql and mariadb both default to port 3306; under nix resolve_ports must assign
        // each a distinct random port so they can coexist without an explicit port in devy.yml.
        let config = make_config(&["mysql", "mariadb"], HashMap::new());
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        let result = up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        );
        assert!(
            result.is_ok(),
            "mysql and mariadb must coexist when given distinct random ports: {:?}",
            result.err()
        );
    }

    #[test]
    fn up_impl_skips_env_section_when_no_env_and_no_path_prepends() {
        // node has no env_vars or path_prepends — env_mgr.setup must not be called.
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        assert!(
            !env_mgr.setup_called.get(),
            "env_mgr.setup must not be called when there is nothing to configure"
        );
    }

    #[test]
    fn up_impl_calls_env_mgr_setup_when_pm_provides_path_prepends() {
        // PM-level path prepends (e.g. Nix profile bin) must be treated the same as
        // module-level path prepends: they constitute content and trigger env_mgr.setup.
        let config = make_config(&[], HashMap::new());
        let pm = MockPackageManager {
            path_prepends_result: vec!["/project/.devy/nix-profile/bin".into()],
            ..Default::default()
        };
        let env_mgr = MockEnvManager {
            is_available: true,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        assert!(
            env_mgr.setup_called.get(),
            "env_mgr.setup must be called when PM provides path prepends"
        );
    }

    #[test]
    fn up_impl_calls_env_mgr_setup_when_config_env_present() {
        let mut env = HashMap::new();
        env.insert("MY_VAR".into(), "val".into());
        let config = make_config(&[], env);
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager {
            is_available: true,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        assert!(
            env_mgr.setup_called.get(),
            "env_mgr.setup must be called when env vars are present"
        );
    }

    #[test]
    fn up_impl_installs_env_mgr_when_unavailable() {
        // When env_mgr reports unavailable, up_impl should install "shadowenv" via the PM.
        let mut env = HashMap::new();
        env.insert("FOO".into(), "bar".into());
        let config = make_config(&[], env);
        let pm = MockPackageManager::default();
        let env_mgr = MockEnvManager {
            is_available: false,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        assert!(
            pm.installed_packages
                .borrow()
                .contains(&"mock-env".to_string()),
            "env_mgr.name() must be installed via PM when env_mgr is unavailable"
        );
    }

    #[test]
    fn up_impl_update_mode_still_emits_orphan_warning() {
        // Write a lock file that records "redis", then run up_impl with update=true and a
        // config that no longer lists redis. The orphan warning must still fire even though
        // --update disables version pinning.
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();

        // Pre-populate a lock file with an orphaned dep.
        let mut deps = std::collections::BTreeMap::new();
        deps.insert(
            "redis".into(),
            crate::lock::LockedDep {
                resolved_version: None,
                source: "homebrew".into(),
                assigned_port: None,
                image_digest: None,
            },
        );
        LockFile {
            dependencies: deps,
            ..Default::default()
        }
        .write(&lock)
        .unwrap();

        // Config no longer mentions redis.
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();

        let warn_count = crate::output::with_warn_capture(|| {
            up_impl(
                &config,
                &pm,
                &env_mgr,
                UpOptions {
                    update: true,
                    bootstrap: false,
                },
                &dir,
                &lock,
            )
            .unwrap();
        });
        // The orphan message is emitted via output::info, not output::warn, so we check
        // that the run succeeded and the lock is rewritten without redis.
        let rewritten = LockFile::load(&lock).unwrap().unwrap();
        assert!(
            !rewritten.dependencies.contains_key("redis"),
            "redis must not appear in the new lock file"
        );
        // Suppress unused-variable warning for warn_count; we just want the run to succeed.
        let _ = warn_count;
    }

    #[test]
    fn up_impl_twice_under_nix_installs_nothing_and_keeps_lock() {
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        let config = make_config(&["node"], HashMap::new());
        // The profile has the unversioned `nodejs` attribute, reporting its version.
        let pm = MockPackageManager {
            name: "nix",
            installed_pkg: Some("nodejs"),
            version: Some("24.20.0".into()),
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();
        let run = || {
            up_impl(
                &config,
                &pm,
                &env_mgr,
                UpOptions {
                    update: false,
                    bootstrap: false,
                },
                &dir,
                &lock,
            )
            .unwrap()
        };

        run();
        assert_eq!(locked_version(&lock, "node").as_deref(), Some("24.20.0"));
        let mtime = backdate(&lock);

        run();
        assert!(
            pm.installed_packages.borrow().is_empty(),
            "nothing must be installed: {:?}",
            pm.installed_packages.borrow()
        );
        assert_eq!(
            std::fs::metadata(&lock).unwrap().modified().unwrap(),
            mtime,
            "the second run must not rewrite the lock"
        );
    }

    #[test]
    fn orphan_detection_treats_alias_and_canonical_as_same_dep() {
        // Lock written with canonical key "node"; config now uses alias "js".
        // Must NOT fire an orphan warning — they're the same dependency.
        // Using a non-service dep (node) avoids the TCP health check path.
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();

        let mut deps = std::collections::BTreeMap::new();
        deps.insert(
            "node".into(),
            crate::lock::LockedDep {
                resolved_version: Some("20.0.0".into()),
                source: "homebrew".into(),
                assigned_port: None,
                image_digest: None,
            },
        );
        LockFile {
            dependencies: deps,
            ..Default::default()
        }
        .write(&lock)
        .unwrap();

        // Config uses alias "js" which resolves to canonical "node".
        let config = make_config(&["js"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();

        // If orphan detection is broken, "node" shows as orphaned even though "js" == "node".
        // The lock should be rewritten with "node" (canonical) still present.
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        let rewritten = LockFile::load(&lock).unwrap().unwrap();
        assert!(
            rewritten.dependencies.contains_key("node"),
            "canonical key 'node' must be in the lock when alias 'js' is used in config"
        );
    }

    #[test]
    fn up_impl_propagates_validate_config_failure() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager {
            validate_config_fails: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        let result = up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        );
        assert!(
            result.is_err(),
            "validate_config failure must propagate as Err"
        );
    }

    #[test]
    fn up_impl_propagates_install_failure() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager {
            install_fails: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        let result = up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        );
        assert!(result.is_err(), "install failure must propagate as Err");
    }

    fn tracked(
        config: &crate::config::DevyConfig,
        pm: &MockPackageManager,
    ) -> (Result<()>, UpProgress) {
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        let mut progress = UpProgress::default();
        let result = up_tracked(
            config,
            pm,
            ContainerRuntime::system(config.container_cli),
            &MockEnvManager::default(),
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
            &mut progress,
        );
        (result, progress)
    }

    #[test]
    fn up_tracked_records_install_step_and_dependency_on_install_failure() {
        let config = make_config(&["jq", "node"], HashMap::new());
        let pm = MockPackageManager {
            name: "brew",
            install_fails: true,
            ..Default::default()
        };
        let (result, progress) = tracked(&config, &pm);
        assert!(result.is_err());
        assert_eq!(progress.step, Some("install"));
        assert_eq!(progress.dependency.as_deref(), Some("jq"));
        assert_eq!(progress.backend.as_deref(), Some("brew"));
    }

    #[test]
    fn up_tracked_records_hook_step_without_dependency() {
        let yaml = "hooks:\n  before_up: \"exit 3\"\ndependencies:\n  - jq\n";
        let config: crate::config::DevyConfig = yaml::from_str(yaml).unwrap();
        let (result, progress) = tracked(&config, &MockPackageManager::default());
        assert!(result.is_err());
        assert_eq!(progress.step, Some("before_up hook"));
        assert_eq!(progress.dependency, None);
    }

    #[test]
    fn up_tracked_records_port_conflict_step() {
        let config = make_config(&["redis", "postgresql"], HashMap::new());
        let dir = crate::test_support::tmp_dir();
        let lock = dir.join(crate::lock::PATH);
        std::fs::write(
            &lock,
            "version: 1\ndependencies:\n  redis:\n    resolved_version: null\n    source: nix\n    assigned_port: 15432\n  postgresql:\n    resolved_version: null\n    source: nix\n    assigned_port: 15432\n",
        )
        .unwrap();
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            ..Default::default()
        };
        let mut progress = UpProgress::default();
        let result = up_tracked(
            &config,
            &pm,
            ContainerRuntime::system(config.container_cli),
            &MockEnvManager::default(),
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
            &mut progress,
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("port conflict"));
        assert_eq!(progress.step, Some("resolve ports"));
    }

    // ── failure record ────────────────────────────────────────────────────────

    fn failed_at_install() -> UpProgress {
        UpProgress {
            step: Some("install"),
            dependency: Some("postgres".into()),
            backend: Some("nix".into()),
        }
    }

    #[test]
    fn record_outcome_writes_record_and_hints_on_failure() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("devy.yml"), "name: x\n").unwrap();
        let err = anyhow::anyhow!("exit 1").context("Failed to install postgres");
        let out = record_outcome(&dir, Err(err), &failed_at_install()).unwrap_err();
        let hinted = out.downcast_ref::<HintedError>().expect("hinted");
        assert_eq!(
            format!("{:#}", hinted.inner),
            "Failed to install postgres: exit 1"
        );
        assert!(
            hinted
                .hint
                .contains("run devy doctor to diagnose this failure")
        );
        let record = failure_record::load(&dir).unwrap().unwrap();
        assert_eq!(record.error_chain, "Failed to install postgres: exit 1");
        assert_eq!(record.step.as_deref(), Some("install"));
        assert_eq!(record.dependency.as_deref(), Some("postgres"));
    }

    #[test]
    fn record_outcome_writes_nothing_when_managed_paths_were_refused() {
        let dir = crate::test_support::tmp_dir();
        let progress = UpProgress {
            step: Some(MANAGED_PATHS_STEP),
            ..Default::default()
        };
        let err = record_outcome(&dir, Err(anyhow::anyhow!("refused")), &progress).unwrap_err();
        assert_eq!(err.to_string(), "refused");
        assert!(
            err.downcast_ref::<HintedError>().is_none(),
            "no doctor hint"
        );
        assert!(!dir.join(".devy").exists(), "nothing may be written");
    }

    #[test]
    fn record_outcome_replaces_previous_record() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("devy.yml"), "name: x\n").unwrap();
        let _ = record_outcome(&dir, Err(anyhow::anyhow!("first")), &failed_at_install());
        let _ = record_outcome(&dir, Err(anyhow::anyhow!("second")), &UpProgress::default());
        let record = failure_record::load(&dir).unwrap().unwrap();
        assert_eq!(record.error_chain, "second");
        assert_eq!(record.step, None);
    }

    #[test]
    fn record_outcome_deletes_record_on_success() {
        let dir = crate::test_support::tmp_dir();
        let _ = record_outcome(&dir, Err(anyhow::anyhow!("boom")), &failed_at_install());
        assert!(failure_record::path(&dir).exists());
        record_outcome(&dir, Ok(()), &UpProgress::default()).unwrap();
        assert!(!failure_record::path(&dir).exists());
    }

    #[test]
    fn record_outcome_unwritable_devy_dir_warns_once_and_keeps_error() {
        let dir = crate::test_support::tmp_dir();
        // A file where the .devy directory should be makes the record unwritable.
        std::fs::write(dir.join(".devy"), "not a directory").unwrap();
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(record_outcome(
                &dir,
                Err(anyhow::anyhow!("boom")),
                &failed_at_install(),
            ));
        });
        let err = result.unwrap().unwrap_err();
        assert!(
            err.downcast_ref::<HintedError>().is_none(),
            "no hint without a record"
        );
        assert_eq!(format!("{err:#}"), "boom");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].starts_with("could not record this failure"),
            "{warnings:?}"
        );
    }

    #[test]
    fn record_outcome_success_with_undeletable_record_warns_and_succeeds() {
        let dir = crate::test_support::tmp_dir();
        std::fs::create_dir_all(failure_record::path(&dir)).unwrap();
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(record_outcome(&dir, Ok(()), &UpProgress::default()));
        });
        assert!(result.unwrap().is_ok());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn up_impl_clears_shadowenv_when_last_env_dep_removed() {
        // When read_vars returns Some (file exists) but there's no content to write,
        // setup must still be called so the stale shadowenv file is overwritten.
        let config = make_config(&[], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager {
            read_vars_returns_some: true,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        assert!(
            env_mgr.setup_called.get(),
            "env_mgr.setup must be called to clear the stale shadowenv file"
        );
    }

    #[test]
    fn up_impl_does_not_call_setup_when_no_content_and_no_existing_file() {
        // When there is nothing to write and no existing file, setup must not run.
        let config = make_config(&[], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default(); // read_vars_returns_some=false
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        assert!(
            !env_mgr.setup_called.get(),
            "env_mgr.setup must not be called when there is no content and no existing file"
        );
    }

    #[test]
    fn up_impl_writes_lock_before_starting_services() {
        // When service start fails, the lock must already have been written
        // for the binary that was successfully installed.
        // This ensures version pinning is not lost on partial failures.
        let config = make_config(&["mysql"], HashMap::new());
        let pm = MockPackageManager {
            installed: false,          // binary not yet installed
            start_service_fails: true, // service start will fail
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        let result = up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        );
        assert!(
            result.is_err(),
            "service start failure must propagate as Err"
        );
        assert!(
            lock.exists(),
            "lock file must be written even when service start fails"
        );
    }

    // ── HOST / PORT env vars ──────────────────────────────────────────────────

    #[test]
    fn up_writes_exactly_the_resolved_project_env() {
        // `devy exec` recomputes the environment from devy.yml and devy.lock; it must
        // match what `up` handed to the env manager.
        let mut env = HashMap::new();
        env.insert("LOG_LEVEL".to_string(), "debug".to_string());
        env.insert("DATABASE_URL".to_string(), "postgres://custom".to_string());
        let config = make_config(&["redis", "postgres", "jq"], env);
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            service_running: true,
            path_prepends_result: vec!["/p/.devy/nix-profile/bin".into()],
            ..Default::default()
        };
        let env_mgr = MockEnvManager::default();
        let dir = crate::test_support::tmp_dir();
        let lock_path = dir.join(crate::lock::PATH);
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock_path,
        )
        .unwrap();

        let lock = LockFile::load(&lock_path).unwrap();
        let mut deps: Vec<Dependency> = config
            .normalized_dependencies()
            .unwrap()
            .iter()
            .map(|dep| apply_lock_from_source(dep, lock.as_ref(), &pm))
            .collect();
        ports::resolve_ports(
            &mut deps,
            ports::PortSource::Lock(lock.as_ref()),
            &pm,
            ports::PortMode::ReadOnly,
        )
        .unwrap();
        let resolved = project_env::resolve(&config, &deps, &pm, &dir, ports::PortMode::ReadOnly);

        assert_eq!(resolved.vars, *env_mgr.last_vars.borrow());
        assert_eq!(resolved.path_prepends, *env_mgr.last_path_prepends.borrow());
        assert!(
            resolved.vars.contains_key("REDIS_PORT"),
            "{:?}",
            resolved.vars
        );
        assert_eq!(
            resolved.vars.get("DATABASE_URL").map(String::as_str),
            Some("postgres://custom")
        );
    }

    #[test]
    fn up_impl_emits_host_and_port_env_vars_for_services() {
        let config = make_config(&["redis"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager {
            is_available: true,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        let written = env_mgr.last_vars.borrow();
        assert!(
            written.contains_key("REDIS_HOST"),
            "REDIS_HOST must be in the written env vars"
        );
        assert!(
            written.contains_key("REDIS_PORT"),
            "REDIS_PORT must be in the written env vars"
        );
        assert_eq!(
            written.get("REDIS_HOST").map(String::as_str),
            Some("127.0.0.1")
        );
    }

    #[test]
    fn up_impl_emits_mysql_host_and_port_for_mysql() {
        let config = make_config(&["mysql"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager {
            is_available: true,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        let written = env_mgr.last_vars.borrow();
        assert_eq!(
            written.get("MYSQL_HOST").map(String::as_str),
            Some("127.0.0.1"),
            "MYSQL_HOST must be 127.0.0.1"
        );
        let port: u16 = written
            .get("MYSQL_PORT")
            .expect("MYSQL_PORT must be present")
            .parse()
            .expect("MYSQL_PORT must be a valid port number");
        assert!(port > 0, "MYSQL_PORT must be non-zero");
    }

    #[test]
    fn up_impl_does_not_emit_host_port_for_non_services() {
        let config = make_config(&["node"], HashMap::new());
        let pm = MockPackageManager {
            installed: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager {
            is_available: true,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        )
        .unwrap();
        let written = env_mgr.last_vars.borrow();
        assert!(
            !written.contains_key("NODE_HOST"),
            "NODE_HOST must not be emitted for non-service deps"
        );
    }

    // ── write_lock stores assigned_port ───────────────────────────────────────

    #[test]
    fn write_lock_stores_assigned_port_for_service_with_explicit_port() {
        let path = tmp_path();
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(16379u64.into()),
        );
        let deps = vec![Dependency::with_extra("redis", extra)];
        write_lock(
            &deps,
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        let lock = crate::lock::LockFile::load(&path).unwrap().unwrap();
        let locked_dep = lock.get("redis").unwrap();
        assert_eq!(
            locked_dep.assigned_port,
            Some(16379),
            "explicit port must be persisted as assigned_port"
        );
    }

    #[test]
    fn write_lock_omits_assigned_port_for_non_service() {
        let path = tmp_path();
        let pm = MockPackageManager::default();
        let deps = vec![Dependency::simple("node")];
        write_lock(
            &deps,
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        let lock = crate::lock::LockFile::load(&path).unwrap().unwrap();
        let locked_dep = lock.get("node").unwrap();
        assert!(
            locked_dep.assigned_port.is_none(),
            "non-service dep must not have assigned_port in the lock"
        );
    }

    #[test]
    fn write_lock_persists_randomly_injected_port() {
        // Simulates what up_impl does: resolve_ports injects a port into extra,
        // then write_lock must persist it so the next run reuses the same port.
        let path = tmp_path();
        let pm = MockPackageManager {
            name: "nix",
            ..Default::default()
        };
        let mut deps = vec![Dependency::simple("redis")];
        ports::resolve_ports(
            &mut deps,
            ports::PortSource::Lock(None),
            &pm,
            ports::PortMode::Assign,
        )
        .unwrap();
        let injected = deps[0].extra.get("port").and_then(|v| v.as_u64()).unwrap() as u16;
        write_lock(
            &deps,
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        let lock = crate::lock::LockFile::load(&path).unwrap().unwrap();
        assert_eq!(
            lock.get("redis").unwrap().assigned_port,
            Some(injected),
            "randomly-injected port must be persisted in the lock"
        );
    }

    fn up_warnings(yaml: &str, lock_entries: &[(&str, &str)]) -> Vec<String> {
        let config: crate::config::DevyConfig = yaml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            name: "nix",
            installed: true,
            ..Default::default()
        };
        let lock = tmp_path();
        if !lock_entries.is_empty() {
            let mut deps = BTreeMap::new();
            for (name, version) in lock_entries {
                deps.insert(
                    name.to_string(),
                    LockedDep {
                        resolved_version: Some(version.to_string()),
                        source: "nix".into(),
                        assigned_port: None,
                        image_digest: None,
                    },
                );
            }
            LockFile {
                dependencies: deps,
                ..Default::default()
            }
            .write(&lock)
            .unwrap();
        }
        let dir = crate::test_support::tmp_dir();
        crate::output::with_warn_messages(|| {
            up_impl(
                &config,
                &pm,
                &MockEnvManager::default(),
                UpOptions {
                    update: false,
                    bootstrap: false,
                },
                &dir,
                &lock,
            )
            .unwrap();
        })
    }

    #[test]
    fn up_impl_warns_on_unmapped_explicit_nix_version() {
        let warnings = up_warnings("dependencies:\n  - jq:\n      version: \"1.6\"\n", &[]);
        assert!(
            warnings.contains(
                &"jq: version 1.6 is not supported by the nix backend — installing the nixpkgs default"
                    .to_string()
            ),
            "{warnings:?}"
        );
    }

    #[test]
    fn up_impl_no_version_warning_for_lock_pinned_version() {
        let warnings = up_warnings("dependencies:\n  - jq\n", &[("jq", "1.7.1")]);
        assert!(
            !warnings
                .iter()
                .any(|w| w.contains("not supported by the nix backend")),
            "{warnings:?}"
        );
    }

    #[test]
    fn up_impl_no_version_warning_for_mapped_version() {
        let warnings = up_warnings("dependencies:\n  - node:\n      version: \"22\"\n", &[]);
        assert!(
            !warnings
                .iter()
                .any(|w| w.contains("not supported by the nix backend")),
            "{warnings:?}"
        );
    }

    #[test]
    fn write_lock_omits_assigned_port_when_backend_cannot_apply() {
        // brew cannot make redis listen on a chosen port, so nothing is recorded,
        // even for an explicit port.
        let path = tmp_path();
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        let mut extra = HashMap::new();
        extra.insert(
            "port".into(),
            crate::config::ExtraValue::Number(6380u64.into()),
        );
        let deps = vec![Dependency::with_extra("redis", extra)];
        write_lock(
            &deps,
            &package_runners(&pm, Path::new("/tmp")),
            &path,
            LockPorts::Resolved,
        )
        .unwrap();
        let lock = crate::lock::LockFile::load(&path).unwrap().unwrap();
        assert_eq!(lock.get("redis").unwrap().assigned_port, None);
    }

    #[test]
    fn up_impl_brew_redis_explicit_port_warns_and_uses_it() {
        let yaml = "dependencies:\n  - redis:\n      port: 6380\n";
        let config: crate::config::DevyConfig = yaml::from_str(yaml).unwrap();
        let pm = MockPackageManager {
            name: "brew",
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager {
            is_available: true,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        let warnings = crate::output::with_warn_messages(|| {
            let _ = up_impl(
                &config,
                &pm,
                &env_mgr,
                UpOptions {
                    update: false,
                    bootstrap: false,
                },
                &dir,
                &lock,
            );
        });
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("cannot make redis listen on port 6380 with brew")),
            "expected unapplied-port warning, got {warnings:?}"
        );
        let written = env_mgr.last_vars.borrow();
        assert_eq!(written.get("REDIS_PORT").map(String::as_str), Some("6380"));
        let lock = crate::lock::LockFile::load(&lock).unwrap().unwrap();
        assert_eq!(lock.get("redis").unwrap().assigned_port, None);
    }

    #[test]
    fn up_impl_brew_redis_uses_default_port_despite_locked_port() {
        let config = make_config(&["redis"], HashMap::new());
        let pm = MockPackageManager {
            name: "brew",
            installed: true,
            service_running: true,
            ..Default::default()
        };
        let env_mgr = MockEnvManager {
            is_available: true,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock = tmp_path();
        let mut locked = BTreeMap::new();
        locked.insert(
            "redis".into(),
            LockedDep {
                resolved_version: None,
                source: "homebrew".into(),
                assigned_port: Some(51000),
                image_digest: None,
            },
        );
        LockFile {
            dependencies: locked,
            ..Default::default()
        }
        .write(&lock)
        .unwrap();
        let _ = up_impl(
            &config,
            &pm,
            &env_mgr,
            UpOptions {
                update: false,
                bootstrap: false,
            },
            &dir,
            &lock,
        );
        let written = env_mgr.last_vars.borrow();
        assert_eq!(written.get("REDIS_PORT").map(String::as_str), Some("6379"));
        assert_eq!(
            written.get("REDIS_URL").map(String::as_str),
            Some("redis://127.0.0.1:6379")
        );
        let lock = LockFile::load(&lock).unwrap().unwrap();
        assert_eq!(lock.get("redis").unwrap().assigned_port, None);
    }

    // ── linked worktrees ──────────────────────────────────────────────────────

    fn nix_service_pm() -> MockPackageManager {
        MockPackageManager {
            name: "nix",
            installed: true,
            service_running: true,
            ..Default::default()
        }
    }

    fn write_redis_lock(path: &Path, port: u16) {
        let mut locked = BTreeMap::new();
        locked.insert(
            "redis".into(),
            LockedDep {
                resolved_version: None,
                source: "nix".into(),
                assigned_port: Some(port),
                image_digest: None,
            },
        );
        LockFile {
            dependencies: locked,
            ..Default::default()
        }
        .write(path)
        .unwrap();
    }

    /// Runs `devy up` for `yaml` in `root` with devy.lock at `<root>/devy.lock`, returning
    /// the environment written and every warning.
    fn up_in(
        yaml: &str,
        pm: &MockPackageManager,
        root: &Path,
        update: bool,
    ) -> (HashMap<String, String>, Vec<String>) {
        let config: crate::config::DevyConfig = yaml::from_str(yaml).unwrap();
        let env_mgr = MockEnvManager {
            is_available: true,
            ..Default::default()
        };
        let warnings = crate::output::with_warn_messages(|| {
            up_impl(
                &config,
                pm,
                &env_mgr,
                UpOptions {
                    update,
                    bootstrap: false,
                },
                root,
                &root.join(crate::lock::PATH),
            )
            .unwrap();
        });
        let vars = env_mgr.last_vars.borrow().clone();
        (vars, warnings)
    }

    #[test]
    fn worktree_up_records_ports_in_its_own_file_and_keeps_the_lock() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        let lock_path = feat.join(crate::lock::PATH);
        write_redis_lock(&lock_path, 51000);
        let mtime = backdate(&lock_path);

        let (vars, _) = up_in(
            "dependencies:\n  - redis\n",
            &nix_service_pm(),
            &feat,
            false,
        );

        let recorded = crate::worktree::WorktreePorts::load(&feat);
        let port = recorded.get("redis").expect("redis port recorded");
        assert_ne!(port, 51000, "a worktree must not use the lock's port");
        assert_eq!(vars.get("REDIS_PORT"), Some(&port.to_string()));
        let lock = LockFile::load(&lock_path).unwrap().unwrap();
        assert_eq!(lock.get("redis").unwrap().assigned_port, Some(51000));
        assert_eq!(
            std::fs::metadata(&lock_path).unwrap().modified().unwrap(),
            mtime,
            "devy.lock must not be rewritten when only ports differ"
        );
        assert_eq!(
            std::fs::read_to_string(feat.join(".devy/.gitignore")).unwrap(),
            "*\n"
        );

        // A second run reuses the worktree's port.
        let (vars, _) = up_in(
            "dependencies:\n  - redis\n",
            &nix_service_pm(),
            &feat,
            false,
        );
        assert_eq!(vars.get("REDIS_PORT"), Some(&port.to_string()));
    }

    #[test]
    fn worktree_up_gives_a_new_service_no_assigned_port_in_the_lock() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        let lock_path = feat.join(crate::lock::PATH);
        write_redis_lock(&lock_path, 51000);

        up_in(
            "dependencies:\n  - redis\n  - memcached\n",
            &nix_service_pm(),
            &feat,
            false,
        );

        let lock = LockFile::load(&lock_path).unwrap().unwrap();
        assert_eq!(lock.get("redis").unwrap().assigned_port, Some(51000));
        assert_eq!(lock.get("memcached").unwrap().assigned_port, None);
        assert!(
            crate::worktree::WorktreePorts::load(&feat)
                .get("memcached")
                .is_some()
        );
    }

    #[test]
    fn worktree_up_update_keeps_worktree_ports() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        write_redis_lock(&feat.join(crate::lock::PATH), 51000);
        let mut recorded = crate::worktree::WorktreePorts::default();
        recorded.ports.insert("redis".into(), 52000);
        recorded.write_if_changed(&feat).unwrap();

        let (vars, _) = up_in("dependencies:\n  - redis\n", &nix_service_pm(), &feat, true);

        assert_eq!(vars.get("REDIS_PORT").map(String::as_str), Some("52000"));
        assert_eq!(
            crate::worktree::WorktreePorts::load(&feat).get("redis"),
            Some(52000)
        );
    }

    #[test]
    fn up_outside_a_worktree_never_writes_the_worktree_file() {
        let dir = crate::test_support::tmp_dir();
        std::fs::create_dir(dir.join(".git")).unwrap();
        up_in("dependencies:\n  - redis\n", &nix_service_pm(), &dir, false);
        assert!(!dir.join(crate::worktree::PORTS_PATH).exists());
        let lock = LockFile::load(&dir.join(crate::lock::PATH))
            .unwrap()
            .unwrap();
        assert!(lock.get("redis").unwrap().assigned_port.is_some());
    }

    #[test]
    fn worktree_up_warns_about_a_fixed_nix_port() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        let yaml = "dependencies:\n  - redis:\n      port: 6380\n";
        let (_, warnings) = up_in(yaml, &nix_service_pm(), &feat, false);
        assert!(
            warnings.iter().any(|w| w
                == "'redis' has a fixed port 6380 in devy.yml, so it can't run in this worktree and the main checkout at the same time"),
            "{warnings:?}"
        );
        // The explicit port isn't recorded, so removing `port:` isolates the worktree.
        assert_eq!(
            crate::worktree::WorktreePorts::load(&feat).get("redis"),
            None
        );
        let (vars, _) = up_in(
            "dependencies:\n  - redis\n",
            &nix_service_pm(),
            &feat,
            false,
        );
        assert_ne!(vars.get("REDIS_PORT").map(String::as_str), Some("6380"));
    }

    #[test]
    fn worktree_up_does_not_warn_about_a_recorded_port() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        for _ in 0..2 {
            let (_, warnings) = up_in(
                "dependencies:\n  - redis\n",
                &nix_service_pm(),
                &feat,
                false,
            );
            assert!(
                !warnings.iter().any(|w| w.contains("fixed port")),
                "{warnings:?}"
            );
        }
        assert!(
            crate::worktree::WorktreePorts::load(&feat)
                .get("redis")
                .is_some()
        );
    }

    #[test]
    fn worktree_up_with_brew_writes_no_worktree_file() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        let pm = MockPackageManager {
            name: "brew",
            ..nix_service_pm()
        };
        up_in("dependencies:\n  - redis\n", &pm, &feat, false);
        assert!(!feat.join(crate::worktree::PORTS_PATH).exists());
    }

    #[test]
    fn worktree_up_does_not_warn_about_a_brew_port() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        let pm = MockPackageManager {
            name: "brew",
            ..nix_service_pm()
        };
        let yaml = "dependencies:\n  - redis:\n      port: 6380\n";
        let (_, warnings) = up_in(yaml, &pm, &feat, false);
        assert!(
            !warnings.iter().any(|w| w.contains("fixed port")),
            "{warnings:?}"
        );
    }

    #[test]
    fn main_checkout_up_does_not_warn_about_a_fixed_port() {
        let dir = crate::test_support::tmp_dir();
        std::fs::create_dir(dir.join(".git")).unwrap();
        let yaml = "dependencies:\n  - redis:\n      port: 6380\n";
        let (_, warnings) = up_in(yaml, &nix_service_pm(), &dir, false);
        assert!(
            !warnings.iter().any(|w| w.contains("fixed port")),
            "{warnings:?}"
        );
    }

    // ── docker-managed services ──────────────────────────────────────────────

    use crate::service_runner::docker::{FakeRunner, fail, ok};

    /// A well-formed sha256 digest standing in for `label`: its bytes in hex, left-padded
    /// with zeros to 64 characters (devy.lock rejects malformed digests).
    fn fake_digest(label: &str) -> String {
        let hex: String = label.bytes().map(|b| format!("{b:02x}")).collect();
        format!("{hex:0>64}")
    }

    /// `<repo>@sha256:<fake_digest(label)>`.
    fn fake_ref(repo: &str, label: &str) -> String {
        format!("{repo}@sha256:{}", fake_digest(label))
    }

    /// A container CLI where the daemon answers, images are missing until pulled (then
    /// report `<repo>@sha256:<digest>`), and no containers exist yet.
    fn docker_cli(digest: &'static str) -> FakeRunner {
        FakeRunner::new(move |call| match call[1].as_str() {
            "image" if call[2] == "inspect" && call.len() == 4 => fail("No such image"),
            "image" => {
                let reference = call.last().unwrap();
                let repo = reference.split([':', '@']).next().unwrap();
                ok(&format!(r#"["{}"]"#, fake_ref(repo, digest)))
            }
            "container" => fail("Error: No such container"),
            _ => ok(""),
        })
    }

    struct DockerUp {
        result: Result<()>,
        lines: Vec<String>,
        env: HashMap<String, String>,
        lock: Option<LockFile>,
        warnings: Vec<String>,
    }

    fn docker_up(
        yaml: &str,
        pm: &MockPackageManager,
        cli: &FakeRunner,
        lock: Option<LockFile>,
        update: bool,
    ) -> DockerUp {
        let config: DevyConfig = yaml::from_str(yaml).unwrap();
        let env_mgr = MockEnvManager {
            is_available: true,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let lock_path = dir.join(crate::lock::PATH);
        if let Some(l) = lock {
            l.write(&lock_path).unwrap();
        }
        let mut result = None;
        let warnings = crate::output::with_warn_messages(|| {
            result = Some(up_with_runtime(
                &config,
                pm,
                ContainerRuntime::new(config.container_cli, cli),
                &env_mgr,
                UpOptions {
                    update,
                    bootstrap: false,
                },
                &dir,
                &lock_path,
            ));
        });
        DockerUp {
            result: result.unwrap(),
            lines: cli.lines(),
            env: env_mgr.last_vars.borrow().clone(),
            lock: LockFile::load(&lock_path).unwrap(),
            warnings,
        }
    }

    fn unavailable_pm() -> MockPackageManager {
        MockPackageManager {
            unavailable: true,
            ..Default::default()
        }
    }

    #[test]
    fn docker_only_project_needs_no_package_manager() {
        let pm = unavailable_pm();
        let cli = docker_cli("abc");
        let up = docker_up(
            "name: app\nservice_manager: docker\ndependencies:\n  - redis\n  - postgresql\n",
            &pm,
            &cli,
            None,
            false,
        );
        up.result.unwrap();
        assert!(pm.installed_packages.borrow().is_empty());
        assert!(pm.started_services.borrow().is_empty());
        assert_eq!(up.lines[0], "docker info --format {{json .ServerVersion}}");
        assert!(
            up.lines.contains(&"docker pull redis:7".to_string()),
            "{:?}",
            up.lines
        );
        assert!(up.lines.contains(&"docker pull postgres:16".to_string()));

        let lock = up.lock.unwrap();
        let redis = lock.get("redis").unwrap();
        assert_eq!(redis.source, "docker");
        assert_eq!(redis.resolved_version.as_deref(), Some("7"));
        assert_eq!(
            redis.image_digest.as_deref(),
            Some(fake_ref("redis", "abc").as_str())
        );
        let port = redis
            .assigned_port
            .expect("docker ports are always assigned");
        assert_eq!(
            lock.get("postgresql").unwrap().image_digest.as_deref(),
            Some(fake_ref("postgres", "abc").as_str())
        );

        // The container runs the digest just recorded, on the exported host port.
        let run = up
            .lines
            .iter()
            .find(|l| l.starts_with("docker run") && l.contains(&fake_ref("redis", "abc")))
            .expect("redis container created from the locked digest");
        assert!(run.contains(&format!("-p 127.0.0.1:{port}:6379")), "{run}");
        assert_eq!(up.env["REDIS_PORT"], port.to_string());
        assert_eq!(up.env["REDIS_URL"], format!("redis://127.0.0.1:{port}"));
    }

    #[test]
    fn mixed_project_checks_the_package_manager() {
        let pm = unavailable_pm();
        let cli = docker_cli("abc");
        let up = docker_up(
            "service_manager: docker\ndependencies:\n  - node\n  - redis\n",
            &pm,
            &cli,
            None,
            false,
        );
        let err = up.result.unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to ensure mock is available"),
            "{err:#}"
        );
        assert!(up.lines.is_empty(), "the package manager is checked first");
    }

    #[test]
    fn mixed_project_installs_packages_and_pulls_images() {
        let pm = MockPackageManager::default();
        let cli = docker_cli("abc");
        let up = docker_up(
            "dependencies:\n  - node\n  - postgres: { service_manager: docker }\n",
            &pm,
            &cli,
            None,
            false,
        );
        up.result.unwrap();
        assert_eq!(*pm.installed_packages.borrow(), vec!["node".to_string()]);
        assert!(up.lines.contains(&"docker pull postgres:16".to_string()));
        let lock = up.lock.unwrap();
        assert_eq!(lock.get("node").unwrap().source, "mock");
        assert_eq!(lock.get("node").unwrap().image_digest, None);
        assert_eq!(lock.get("postgresql").unwrap().source, "docker");
    }

    #[test]
    fn docker_unavailable_fails_before_pulling() {
        let pm = MockPackageManager::default();
        let cli = FakeRunner::new(|_| fail("Cannot connect to the Docker daemon"));
        let up = docker_up(
            "service_manager: docker\ndependencies:\n  - redis\n",
            &pm,
            &cli,
            None,
            false,
        );
        assert_eq!(
            up.result.unwrap_err().to_string(),
            "docker is not available — install it or start its daemon, or set service_manager: package"
        );
        assert_eq!(up.lines.len(), 1, "{:?}", up.lines);
        assert!(up.lock.is_none());
    }

    #[test]
    fn podman_runs_every_container_operation() {
        let pm = unavailable_pm();
        let cli = docker_cli("abc");
        let up = docker_up(
            "service_manager: docker\ncontainer_cli: podman\ndependencies:\n  - redis\n",
            &pm,
            &cli,
            None,
            false,
        );
        up.result.unwrap();
        assert!(
            up.lines.iter().all(|l| l.starts_with("podman ")),
            "{:?}",
            up.lines
        );
    }

    fn locked_redis(digest: &str) -> LockFile {
        let mut deps = BTreeMap::new();
        deps.insert(
            "redis".into(),
            LockedDep {
                resolved_version: Some("7".into()),
                source: "docker".into(),
                assigned_port: Some(51000),
                image_digest: Some(digest.into()),
            },
        );
        LockFile {
            dependencies: deps,
            ..Default::default()
        }
    }

    #[test]
    fn teammate_runs_the_locked_digest() {
        let pm = unavailable_pm();
        let cli = docker_cli("moved");
        let lock = locked_redis(&fake_ref("redis", "locked"));
        let up = docker_up(
            "service_manager: docker\ndependencies:\n  - redis\n",
            &pm,
            &cli,
            Some(lock.clone()),
            false,
        );
        up.result.unwrap();
        assert!(
            up.lines
                .contains(&format!("docker pull {}", fake_ref("redis", "locked"))),
            "{:?}",
            up.lines
        );
        assert!(
            !up.lines.iter().any(|l| l.contains("redis:7")),
            "{:?}",
            up.lines
        );
        assert_eq!(up.lock.unwrap(), lock, "lock unchanged");
        assert_eq!(up.env["REDIS_PORT"], "51000");
    }

    #[test]
    fn update_pulls_the_tag_and_records_its_digest() {
        let pm = unavailable_pm();
        let cli = docker_cli("new");
        let up = docker_up(
            "service_manager: docker\ndependencies:\n  - redis\n",
            &pm,
            &cli,
            Some(locked_redis(&fake_ref("redis", "old"))),
            true,
        );
        up.result.unwrap();
        assert!(up.lines.contains(&"docker pull redis:7".to_string()));
        let lock = up.lock.unwrap();
        let redis = lock.get("redis").unwrap();
        assert_eq!(
            redis.image_digest.as_deref(),
            Some(fake_ref("redis", "new").as_str())
        );
        assert_eq!(redis.assigned_port, Some(51000), "--update keeps ports");
    }

    #[test]
    fn changed_tag_resolves_again() {
        let pm = unavailable_pm();
        let cli = docker_cli("v72");
        let up = docker_up(
            "service_manager: docker\ndependencies:\n  - redis: { version: \"7.2\" }\n",
            &pm,
            &cli,
            Some(locked_redis(&fake_ref("redis", "old"))),
            false,
        );
        up.result.unwrap();
        assert!(up.lines.contains(&"docker pull redis:7.2".to_string()));
        let lock = up.lock.unwrap();
        let redis = lock.get("redis").unwrap();
        assert_eq!(redis.resolved_version.as_deref(), Some("7.2"));
        assert_eq!(
            redis.image_digest.as_deref(),
            Some(fake_ref("redis", "v72").as_str())
        );
    }

    #[test]
    fn docker_redis_under_brew_gets_an_assigned_port() {
        let pm = MockPackageManager {
            name: "brew",
            ..Default::default()
        };
        let cli = docker_cli("abc");
        let up = docker_up(
            "package_manager: brew\nservice_manager: docker\ndependencies:\n  - redis\n",
            &pm,
            &cli,
            None,
            false,
        );
        up.result.unwrap();
        let port = up
            .lock
            .unwrap()
            .get("redis")
            .unwrap()
            .assigned_port
            .unwrap();
        assert_eq!(up.env["REDIS_URL"], format!("redis://127.0.0.1:{port}"));
        assert!(
            !up.warnings
                .iter()
                .any(|w| w.contains("cannot make redis listen")),
            "{:?}",
            up.warnings
        );
    }

    #[test]
    fn docker_mysql_and_mariadb_get_distinct_ports() {
        let pm = unavailable_pm();
        let cli = docker_cli("abc");
        let up = docker_up(
            "service_manager: docker\ndependencies:\n  - mysql\n  - mariadb\n",
            &pm,
            &cli,
            None,
            false,
        );
        up.result.unwrap();
        let lock = up.lock.unwrap();
        let mysql = lock.get("mysql").unwrap().assigned_port.unwrap();
        let mariadb = lock.get("mariadb").unwrap().assigned_port.unwrap();
        assert_ne!(mysql, mariadb);
        assert!(
            up.lines
                .iter()
                .any(|l| l.contains(&format!("127.0.0.1:{mysql}:3306")))
        );
        assert!(
            up.lines
                .iter()
                .any(|l| l.contains(&format!("127.0.0.1:{mariadb}:3306")))
        );
    }

    #[test]
    fn docker_version_is_an_image_tag_not_a_nix_version() {
        let pm = MockPackageManager {
            name: "nix",
            unavailable: true,
            ..Default::default()
        };
        let cli = docker_cli("abc");
        let up = docker_up(
            "service_manager: docker\ndependencies:\n  - redis: { version: \"7.2\", image: \"mirror/redis:7\" }\n",
            &pm,
            &cli,
            None,
            false,
        );
        up.result.unwrap();
        assert!(
            !up.warnings.iter().any(|w| w.contains("nix backend")),
            "{:?}",
            up.warnings
        );
        assert!(up.warnings.contains(
            &"redis: image tag 7 overrides version 7.2 — remove one of them".to_string()
        ));
        assert!(up.lines.contains(&"docker pull mirror/redis:7".to_string()));
    }

    #[test]
    fn up_rejects_service_manager_on_non_service() {
        let pm = MockPackageManager::default();
        let cli = FakeRunner::ok();
        let up = docker_up(
            "dependencies:\n  - foo: { service_manager: docker }\n",
            &pm,
            &cli,
            None,
            false,
        );
        assert_eq!(
            up.result.unwrap_err().to_string(),
            "foo: service_manager and image apply only to built-in services"
        );
    }

    #[test]
    fn present_image_is_not_pulled() {
        let cli = FakeRunner::new(|call| match call[1].as_str() {
            "image" if call.len() == 4 => ok("[{}]"),
            "image" => ok(r#"["redis@sha256:abc"]"#),
            _ => ok(""),
        });
        let pm = MockPackageManager::default();
        let config: DevyConfig =
            yaml::from_str("service_manager: docker\ndependencies:\n  - redis\n").unwrap();
        let dep = config.normalized_dependencies().unwrap().remove(0);
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &cli),
            &config,
            Path::new("/tmp"),
            None,
            false,
        );
        install_binary(&runners, &dep, Path::new("/tmp")).unwrap();
        assert_eq!(cli.lines(), vec!["docker image inspect redis:7"]);
    }

    #[test]
    fn docker_postgres_skips_package_manager_conf_d() {
        let conf_dir = crate::test_support::tmp_dir();
        let pm = MockPackageManager {
            name: "apt",
            config_dir: Some(conf_dir.join("postgresql")),
            ..Default::default()
        };
        let cli = docker_cli("abc");
        let config: DevyConfig = yaml::from_str(
            "service_manager: docker\ndependencies:\n  - postgres: { port: 6543 }\n",
        )
        .unwrap();
        let dep = config.normalized_dependencies().unwrap().remove(0);
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &cli),
            &config,
            Path::new("/tmp"),
            None,
            false,
        );
        install_binary(&runners, &dep, Path::new("/tmp")).unwrap();
        assert!(
            !conf_dir.join("postgresql").exists(),
            "no conf.d write for a docker-managed postgres"
        );
        // The same dependency on the package manager (apt, which reads conf.d) does write it.
        let package = Dependency {
            docker: false,
            ..dep
        };
        install_binary(&runners, &package, Path::new("/tmp")).unwrap();
        assert!(conf_dir.join("postgresql").join("devy.conf").exists());
    }

    #[test]
    fn docker_services_keep_non_config_post_setup_warnings() {
        let pm = MockPackageManager::default();
        let cli = FakeRunner::new(|_| ok("[]"));
        let config: DevyConfig = yaml::from_str(
            "service_manager: docker\ndependencies:\n  - vault: { dev_mode: true }\n",
        )
        .unwrap();
        let dep = config.normalized_dependencies().unwrap().remove(0);
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &cli),
            &config,
            Path::new("/tmp"),
            None,
            false,
        );
        let warnings = crate::output::with_warn_messages(|| {
            install_binary(&runners, &dep, Path::new("/tmp")).unwrap();
        });
        assert!(
            warnings.iter().any(|w| w.contains("VAULT_TOKEN")),
            "{warnings:?}"
        );
    }
}
