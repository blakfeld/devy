use anyhow::{Context, Result};
use fs2::FileExt;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::commands::exec::{run_hook, spawn_cmd};
use crate::commands::ports;
use crate::config::{Dependency, DevyCommand, DevyConfig};
use crate::env_manager::{EnvManager, Shadowenv};
use crate::lock::{LockFile, LockedDep};
use crate::modules;
use crate::output;
use crate::package_manager;

#[cfg_attr(test, mutants::skip)] // thin delegation — reads process env and disk; not unit-testable
pub fn run(update: bool, bootstrap: bool) -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;
    let pm = package_manager::detect(config.package_manager, &project_root)?;

    // Acquire an exclusive advisory lock so concurrent `devy up` invocations
    // (e.g. two devs on the same machine, parallel CI jobs) queue rather than race.
    // Uses a dedicated guard file to avoid inode-swap conflicts with write_lock's
    // rename strategy on devy.lock itself.
    let guard_path = project_root.join(".devy-lock");
    let _guard = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&guard_path)
        .context("Failed to open process guard file")?;
    _guard
        .lock_exclusive()
        .context("Failed to acquire process lock (is another devy process running?)")?;

    up_impl(
        &config,
        pm.as_ref(),
        &Shadowenv,
        UpOptions { update, bootstrap },
        &project_root,
        &project_root.join(crate::lock::PATH),
    )
    // _guard dropped here → lock released
}

pub(crate) struct UpOptions {
    pub update: bool,
    pub bootstrap: bool,
}

pub(crate) fn up_impl(
    config: &DevyConfig,
    pm: &dyn package_manager::PackageManager,
    env_mgr: &dyn EnvManager,
    opts: UpOptions,
    project_root: &Path,
    lock_path: &Path,
) -> Result<()> {
    let project_name = config.name.as_deref().unwrap_or("project");
    output::header(&format!("devy up · {}", project_name));

    if let Some(ref hook) = config.hooks.before_up {
        output::header("Hooks");
        run_hook("before_up", hook)?;
    }

    output::step(&format!("Checking for {}", pm.name()));
    pm.ensure_available(opts.bootstrap)
        .with_context(|| format!("Failed to ensure {} is available", pm.name()))?;
    output::success(&format!("{} available", pm.name()));

    // Load the existing lock for orphan comparison regardless of --update.
    let existing_lock = LockFile::load(lock_path).context("Failed to read devy.lock")?;

    // For version pinning, ignore the lock when --update is passed.
    let lock = if opts.update {
        if lock_path.exists() {
            output::step("Ignoring devy.lock (--update)");
        }
        None
    } else {
        existing_lock.clone()
    };

    let deps = config.normalized_dependencies()?;

    for dep in &deps {
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
            .chain(modules::nix_version_warning(dep, pm));
        for warning in warnings {
            output::warn(&format!("{}: {}", dep.name, warning));
        }
    }

    // Assign stable ports to service deps before conflict detection.
    // Uses the existing lock (not the version-pinning lock) so ports survive --update.
    ports::resolve_and_check(
        &mut effective_deps,
        existing_lock.as_ref(),
        pm,
        ports::PortMode::Assign,
    )?;

    // Collect module-suggested env vars and PATH prepends after each dep installs.
    // This ensures failed installs don't pollute the environment config.
    let mut module_env: HashMap<String, String> = HashMap::new();
    // PM-level prepends (e.g. .devy/nix-profile/bin) go first so project-local
    // binaries shadow any system copies of the same tools.
    let mut module_path_prepends: Vec<String> = pm.path_prepends(project_root);

    // Phase 1: install all binaries (no services started yet).
    if !effective_deps.is_empty() {
        output::header("Dependencies");
        for effective in &effective_deps {
            install_binary(pm, effective, project_root)?;
            let m = modules::get(&effective.name);
            module_env.extend(m.env_vars(effective, project_root));
            module_path_prepends.extend(m.path_prepends(effective, project_root));
        }
    }

    // Emit <SERVICE>_HOST and <SERVICE>_PORT for every service dep.
    // These run after module env_vars so service-specific vars (REDIS_URL, etc.) are
    // already present; config.environment can still override everything via merge_env.
    for effective in &effective_deps {
        let m = modules::get(&effective.name);
        if !m.is_service() {
            continue;
        }
        let canonical = modules::canonical_name(&effective.name);
        let prefix = canonical.replace('-', "_").to_ascii_uppercase();
        let port_opt: Option<u16> = if let Some(key) = m.port_key() {
            effective
                .extra
                .get(key)
                .and_then(|v| v.as_u64())
                .and_then(|raw| u16::try_from(raw).ok())
                .or_else(|| m.default_port())
        } else {
            m.default_port()
        };
        module_env.insert(format!("{prefix}_HOST"), "127.0.0.1".into());
        if let Some(p) = port_opt {
            module_env.insert(format!("{prefix}_PORT"), p.to_string());
        }
    }

    // Write the lock immediately after all binaries are confirmed installed.
    // Doing this before service start means a service failure doesn't leave the
    // lock stale for the already-installed packages.
    write_lock(&effective_deps, pm, lock_path)?;

    let merged_env = merge_env(module_env, &config.environment);

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

            let shell = std::env::var("SHELL")
                .ok()
                .and_then(|s| s.rsplit('/').next().map(String::from))
                .filter(|s| matches!(s.as_str(), "sh" | "zsh" | "bash" | "fish" | "powershell"))
                .unwrap_or_else(|| {
                    if cfg!(target_os = "windows") {
                        "powershell".into()
                    } else {
                        "zsh".into()
                    }
                });
            output::info_code(
                "Activate with:",
                &format!("eval \"$(shadowenv hook {shell})\""),
            );
        } else {
            output::success("Environment configuration cleared");
        }
    }

    // Warn about deps that were in the lock file but are no longer in devy.yml.
    // Runs even in --update mode so removals are always surfaced.
    if let Some(ref old_lock) = existing_lock {
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
    for effective in &effective_deps {
        start_service_if_needed(pm, effective, project_root)?;
    }

    if let Some(ref hook) = config.hooks.after_up {
        output::header("Hooks");
        run_hook("after_up", hook)?;
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
pub(crate) fn install_binary(
    pm: &dyn package_manager::PackageManager,
    dep: &Dependency,
    project_root: &std::path::Path,
) -> Result<()> {
    let module = modules::get(&dep.name);
    let display = dep.versioned_name();

    if module.is_installed(pm, dep)? {
        output::skip(&format!(
            "{} already installed (via {})",
            display,
            pm.name()
        ));
    } else {
        output::step(&format!("Installing {}", display));
        module
            .install(pm, dep)
            .with_context(|| format!("Failed to install {}", display))?;
        output::success(&format!("Installed {}", display));

        if let Some(cmd) = &dep.after_install {
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
    }

    // post_setup runs even when already installed — it is idempotent by contract and
    // handles things like bundle install that must run regardless of install state.
    module
        .post_setup(dep, pm, project_root)
        .with_context(|| format!("post_setup failed for {}", dep.name))?;

    Ok(())
}

/// Starts a service dependency if it isn't already running. No-op for non-service deps.
/// Call this in Phase 2, after the lock has been written.
pub(crate) fn start_service_if_needed(
    pm: &dyn package_manager::PackageManager,
    dep: &Dependency,
    project_root: &Path,
) -> Result<()> {
    let module = modules::get(&dep.name);
    if !module.is_service() {
        return Ok(());
    }
    if module.is_running(pm, dep)? {
        output::skip(&format!("{} service already running", dep.name));
    } else {
        output::step(&format!("Starting {} service", dep.name));
        module
            .start(pm, dep, project_root)
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

pub(crate) fn write_lock(
    deps: &[Dependency],
    pm: &dyn package_manager::PackageManager,
    path: &Path,
) -> Result<()> {
    let mut locked = BTreeMap::new();
    for dep in deps {
        let module = modules::get(&dep.name);
        let resolved = module.resolved_version(pm, dep)?;
        // Only record ports the backend actually applies; others always use the default.
        let assigned_port = module
            .port_key()
            .filter(|_| module.port_applicable(pm))
            .and_then(|key| {
                dep.extra
                    .get(key)
                    .and_then(|v| v.as_u64())
                    .and_then(|raw| u16::try_from(raw).ok())
            });
        locked.insert(
            modules::canonical_name(&dep.name).to_string(),
            LockedDep {
                resolved_version: resolved,
                source: module.source().unwrap_or(pm.name()).to_string(),
                assigned_port,
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
    use crate::config::Dependency;
    use crate::lock::{LockFile, LockedDep};
    use crate::package_manager::MockPackageManager;
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
            install_binary(&pm, &dep, std::path::Path::new("/tmp")).is_err(),
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
        assert!(install_binary(&pm, &dep, std::path::Path::new("/tmp")).is_ok());
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
        };
        let result = install_binary(&pm, &dep, &dir);
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
        };
        install_binary(&pm, &dep, &dir).unwrap();
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
        assert!(start_service_if_needed(&pm, &dep, Path::new("/tmp")).is_ok());
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
            start_service_if_needed(&pm, &dep, Path::new("/tmp")).is_ok(),
            "must return Ok even though health check times out in test environment"
        );
        assert!(
            pm.started_services.borrow().is_empty(),
            "start must not be called when service is already running"
        );
    }

    #[test]
    fn start_service_if_needed_propagates_start_error() {
        let pm = MockPackageManager {
            start_service_fails: true,
            ..Default::default()
        };
        let dep = Dependency::simple("mysql");
        assert!(
            start_service_if_needed(&pm, &dep, Path::new("/tmp")).is_err(),
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
        write_lock(&deps, &pm, &path).unwrap();
        assert!(path.exists(), "write_lock must create the lock file");
    }

    #[test]
    fn write_lock_includes_dep_name_in_file() {
        let path = tmp_path();
        let pm = MockPackageManager::default();
        let deps = vec![Dependency::simple("redis")];
        write_lock(&deps, &pm, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("redis"), "lock file must contain dep name");
    }

    #[test]
    fn write_lock_skips_write_when_unchanged() {
        let path = tmp_path();
        let pm = MockPackageManager::default();
        let deps = vec![Dependency::simple("node")];
        write_lock(&deps, &pm, &path).unwrap();
        let content_after_first = std::fs::read_to_string(&path).unwrap();
        write_lock(&deps, &pm, &path).unwrap();
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
        write_lock(&deps, &pm, &path).unwrap();
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

    // ── merge_env ─────────────────────────────────────────────────────────────

    #[test]
    fn config_env_overrides_module_env_for_same_key() {
        let mut module_env = HashMap::new();
        module_env.insert("VAULT_TOKEN".into(), "root".into());
        let mut config_env = HashMap::new();
        config_env.insert("VAULT_TOKEN".into(), "my-token".into());
        let merged = merge_env(module_env, &config_env);
        assert_eq!(
            merged.get("VAULT_TOKEN").map(String::as_str),
            Some("my-token"),
            "config.environment must overwrite module env_vars on conflict"
        );
    }

    #[test]
    fn module_env_keys_absent_from_config_are_preserved() {
        let mut module_env = HashMap::new();
        module_env.insert("VAULT_ADDR".into(), "http://127.0.0.1:8200".into());
        let config_env = HashMap::new();
        let merged = merge_env(module_env, &config_env);
        assert_eq!(
            merged.get("VAULT_ADDR").map(String::as_str),
            Some("http://127.0.0.1:8200")
        );
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
        write_lock(&deps, &pm, &path).unwrap();
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
        write_lock(&deps, &pm, &path).unwrap();
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
        ports::resolve_ports(&mut deps, None, &pm, ports::PortMode::Assign).unwrap();
        let injected = deps[0].extra.get("port").and_then(|v| v.as_u64()).unwrap() as u16;
        write_lock(&deps, &pm, &path).unwrap();
        let lock = crate::lock::LockFile::load(&path).unwrap().unwrap();
        assert_eq!(
            lock.get("redis").unwrap().assigned_port,
            Some(injected),
            "randomly-injected port must be persisted in the lock"
        );
    }

    fn up_warnings(yaml: &str, lock_entries: &[(&str, &str)]) -> Vec<String> {
        let config: crate::config::DevyConfig = serde_yml::from_str(yaml).unwrap();
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
        write_lock(&deps, &pm, &path).unwrap();
        let lock = crate::lock::LockFile::load(&path).unwrap().unwrap();
        assert_eq!(lock.get("redis").unwrap().assigned_port, None);
    }

    #[test]
    fn up_impl_brew_redis_explicit_port_warns_and_uses_it() {
        let yaml = "dependencies:\n  - redis:\n      port: 6380\n";
        let config: crate::config::DevyConfig = serde_yml::from_str(yaml).unwrap();
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
}
