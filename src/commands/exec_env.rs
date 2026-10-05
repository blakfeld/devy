//! `devy exec`: runs a program directly, without a shell, in the project environment.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::commands::ports::{self, PortMode};
use crate::commands::up::apply_lock_from_source;
use crate::config::DevyConfig;
use crate::error::SilentExit;
use crate::package_manager::{self, PackageManager};
use crate::project_env::{self, ProjectEnv};

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — spawns the program; covered by tests/cli.rs
pub fn run(argv: &[String]) -> Result<()> {
    let (config, project_root) = crate::trust::load_gated()?;
    // The project environment (PATH, NODE_OPTIONS, LD_PRELOAD, …) can run the
    // repository's code in whatever program is started, so it needs trust like `up`.
    crate::trust::require(&config, &project_root, crate::trust::Gate::Other)?;
    let pm = package_manager::detect(&config, &project_root)?;
    let env = project_environment(&config, pm.as_ref(), &project_root)?;
    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    let mut cmd = command(argv, &env, std::env::var_os("PATH"), &cwd)?;
    let status = cmd
        .status()
        .with_context(|| format!("failed to run '{}'", argv[0]))?;
    exit_result(status)
}

/// The environment `devy up` writes, computed from devy.yml and devy.lock. Ports are
/// resolved read-only; nothing is written.
pub(crate) fn project_environment(
    config: &DevyConfig,
    pm: &dyn PackageManager,
    project_root: &Path,
) -> Result<ProjectEnv> {
    let normalized = config.normalized_dependencies()?;
    // The environment puts `.devy/nix-profile/bin` and each virtualenv's `bin` first on
    // PATH, so a repository that commits `.venv/bin/sudo` (or symlinks the profile
    // elsewhere) would choose what runs. Every consumer gets the same check as `up`.
    crate::fs_safe::check_managed_paths(project_root, &crate::modules::managed_venvs(&normalized))?;
    let lock = ports::load_lock(project_root)?;
    let mut deps: Vec<_> = normalized
        .iter()
        .map(|dep| apply_lock_from_source(dep, lock.as_ref(), pm))
        .collect();
    ports::resolve_ports(&mut deps, lock.as_ref(), pm, PortMode::ReadOnly)?;
    Ok(project_env::resolve(
        config,
        &deps,
        pm,
        project_root,
        PortMode::ReadOnly,
    ))
}

/// The command for `argv` with the project environment applied: its variables set, and
/// its PATH entries ahead of `inherited_path`. A bare program name is looked up on that
/// PATH, so project-local binaries win over system ones.
///
/// A `PATH` set in devy.yml `environment` replaces the whole PATH, as it does in the
/// shadowenv file, where `env/set` runs after the prepends.
pub(crate) fn command(
    argv: &[String],
    env: &ProjectEnv,
    inherited_path: Option<OsString>,
    cwd: &Path,
) -> Result<Command> {
    let (program, args) = argv
        .split_first()
        .context("devy exec needs a program to run")?;
    let path = match env.vars.get("PATH") {
        Some(configured) => OsString::from(configured),
        None => joined_path(&env.path_prepends, inherited_path)?,
    };
    let mut cmd = Command::new(locate(program, &path, cwd)?);
    cmd.args(args).envs(&env.vars).env("PATH", path);
    Ok(cmd)
}

/// `prepends` followed by the entries of `inherited`.
fn joined_path(prepends: &[String], inherited: Option<OsString>) -> Result<OsString> {
    let inherited: Vec<PathBuf> = inherited
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let entries = prepends.iter().map(PathBuf::from).chain(inherited);
    std::env::join_paths(entries).context("a PATH entry contains an invalid character")
}

/// `program` itself when it names a path, otherwise its location on `path`.
fn locate(program: &str, path: &OsString, cwd: &Path) -> Result<PathBuf> {
    if Path::new(program).components().count() > 1 {
        return Ok(PathBuf::from(program));
    }
    which::which_in(program, Some(path), cwd).map_err(|_| {
        // which skips files that exist but can't be executed; name the one found.
        match std::env::split_paths(path)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
        {
            Some(file) => anyhow::anyhow!(
                "failed to run '{program}': {} is not executable",
                file.display()
            ),
            None => anyhow::anyhow!("failed to run '{program}': not found on PATH"),
        }
    })
}

/// Passes the program's exit code through; a signal termination becomes exit 1.
fn exit_result(status: ExitStatus) -> Result<()> {
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(SilentExit(code).into()),
        None => Err(SilentExit(1).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;
    use serde_norway as yaml;
    use std::collections::HashMap;

    fn env(vars: &[(&str, &str)], path_prepends: &[&str]) -> ProjectEnv {
        ProjectEnv {
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            path_prepends: path_prepends.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn envs(cmd: &Command) -> HashMap<String, String> {
        cmd.get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect()
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn joined_path_puts_prepends_first() {
        let inherited = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        let joined = joined_path(&["/p/a".into(), "/p/b".into()], Some(inherited)).unwrap();
        let entries: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(
            entries,
            ["/p/a", "/p/b", "/usr/bin", "/bin"]
                .iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn joined_path_without_inherited_path() {
        let joined = joined_path(&["/p/a".into()], None).unwrap();
        assert_eq!(joined, OsString::from("/p/a"));
    }

    #[test]
    fn command_sets_vars_and_path() {
        let dir = crate::test_support::tmp_dir();
        let program = if cfg!(windows) { "hello.cmd" } else { "hello" };
        std::fs::write(dir.join(program), "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join(program), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let bin = dir.display().to_string();
        let project = env(&[("LOG_LEVEL", "debug")], &[&bin]);
        let cmd = command(
            &argv(&["hello", "--flag", "$HOME; rm -rf x"]),
            &project,
            Some("/usr/bin".into()),
            &dir,
        )
        .unwrap();
        assert_eq!(
            Path::new(cmd.get_program()).canonicalize().unwrap(),
            dir.join(program).canonicalize().unwrap(),
            "the program is found on the computed PATH"
        );
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["--flag", "$HOME; rm -rf x"]);
        let vars = envs(&cmd);
        assert_eq!(vars["LOG_LEVEL"], "debug");
        let path: Vec<PathBuf> = std::env::split_paths(&vars["PATH"]).collect();
        assert_eq!(path, [PathBuf::from(&bin), PathBuf::from("/usr/bin")]);
    }

    #[test]
    fn configured_path_replaces_the_computed_path() {
        let dir = crate::test_support::tmp_dir();
        let program = if cfg!(windows) { "hello.cmd" } else { "hello" };
        std::fs::write(dir.join(program), "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join(program), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let configured = dir.display().to_string();
        let project = env(&[("PATH", &configured)], &["/p/bin"]);
        let cmd = command(&argv(&["hello"]), &project, Some("/usr/bin".into()), &dir).unwrap();
        assert_eq!(envs(&cmd)["PATH"], configured);
    }

    #[cfg(unix)]
    #[test]
    fn command_names_a_non_executable_match() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("notes"), "").unwrap();
        let err = command(
            &argv(&["notes"]),
            &env(&[], &[]),
            Some(dir.display().to_string().into()),
            &dir,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "failed to run 'notes': {} is not executable",
                dir.join("notes").display()
            )
        );
    }

    #[test]
    fn command_keeps_explicit_program_paths() {
        let dir = crate::test_support::tmp_dir();
        let cmd = command(&argv(&["./run.sh"]), &env(&[], &[]), None, &dir).unwrap();
        assert_eq!(cmd.get_program(), "./run.sh");
    }

    #[test]
    fn command_reports_missing_program() {
        let dir = crate::test_support::tmp_dir();
        let err = command(
            &argv(&["no-such-program"]),
            &env(&[], &[]),
            Some(dir.display().to_string().into()),
            &dir,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to run 'no-such-program': not found on PATH"
        );
    }

    #[test]
    fn project_environment_uses_locked_ports_and_writes_nothing() {
        let dir = crate::test_support::tmp_dir();
        let mut locked = std::collections::BTreeMap::new();
        locked.insert(
            "redis".to_string(),
            crate::lock::LockedDep {
                resolved_version: None,
                source: "nix".into(),
                assigned_port: Some(52113),
                image_digest: None,
            },
        );
        crate::lock::LockFile {
            dependencies: locked,
            ..Default::default()
        }
        .write(&dir.join(crate::lock::PATH))
        .unwrap();
        let config: DevyConfig = yaml::from_str(
            "dependencies:\n  - redis\n  - postgres\nenvironment:\n  LOG_LEVEL: debug\n",
        )
        .unwrap();
        let pm = MockPackageManager {
            name: "nix",
            path_prepends_result: vec!["/p/.devy/nix-profile/bin".into()],
            ..Default::default()
        };
        let lock_before = std::fs::read(dir.join(crate::lock::PATH)).unwrap();

        let env = project_environment(&config, &pm, &dir).unwrap();

        assert_eq!(env.vars["LOG_LEVEL"], "debug");
        assert_eq!(env.vars["REDIS_HOST"], "127.0.0.1");
        assert_eq!(env.vars["REDIS_PORT"], "52113");
        assert_eq!(env.vars["REDIS_URL"], "redis://127.0.0.1:52113");
        assert_eq!(env.vars["POSTGRESQL_HOST"], "127.0.0.1");
        assert!(
            !env.vars.contains_key("POSTGRESQL_PORT"),
            "postgres has no port in the lock yet: {:?}",
            env.vars
        );
        assert_eq!(env.path_prepends, ["/p/.devy/nix-profile/bin"]);
        assert_eq!(
            std::fs::read(dir.join(crate::lock::PATH)).unwrap(),
            lock_before
        );
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "only devy.lock exists");
    }

    #[cfg(unix)]
    #[test]
    fn exit_result_passes_codes_through() {
        use std::os::unix::process::ExitStatusExt;
        assert!(exit_result(ExitStatus::from_raw(0)).is_ok());
        let code = |status| {
            exit_result(status)
                .unwrap_err()
                .downcast_ref::<SilentExit>()
                .map(|e| e.0)
        };
        assert_eq!(code(ExitStatus::from_raw(3 << 8)), Some(3));
        // Killed by SIGKILL.
        assert_eq!(code(ExitStatus::from_raw(9)), Some(1));
    }

    #[cfg(windows)]
    #[test]
    fn exit_result_passes_codes_through() {
        use std::os::windows::process::ExitStatusExt;
        assert!(exit_result(ExitStatus::from_raw(0)).is_ok());
        let err = exit_result(ExitStatus::from_raw(3)).unwrap_err();
        assert_eq!(err.downcast_ref::<SilentExit>().map(|e| e.0), Some(3));
    }
}
