//! devy's per-user state directory, outside every project: `$XDG_STATE_HOME/devy/`
//! (default `~/.local/state/devy/`) on Unix and `%LOCALAPPDATA%\devy\` on Windows, with
//! 0700 directories. It holds the copies of the `500_devy.lisp` files devy wrote (see
//! `shadowenv::COPY_SUBDIR`), which the shell hook's guard compares against.
//!
//! A state directory inside the project is refused: the repository could supply the
//! copies the guard trusts (for example with `XDG_STATE_HOME` pointed into the project by
//! its own environment, or shipped in a tarball).

use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The per-user state directory.
#[derive(Debug, Clone)]
pub struct StateDir {
    /// `<state>/devy`.
    base: PathBuf,
    /// The directory resolves to the platform default (`~/.local/state/devy`, or
    /// `%LOCALAPPDATA%\devy`), whichever variable named it.
    default_location: bool,
}

fn absolute_var(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// `$XDG_STATE_HOME` (when absolute) or `~/.local/state` on Unix; `%LOCALAPPDATA%` on
/// Windows.
fn state_home() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        absolute_var("LOCALAPPDATA").ok_or_else(|| {
            anyhow!("Failed to locate devy's state directory: %LOCALAPPDATA% is not set")
        })
    }
    #[cfg(not(windows))]
    {
        absolute_var("XDG_STATE_HOME")
            .or_else(|| absolute_var("HOME").map(|home| home.join(".local").join("state")))
            .ok_or_else(|| {
                anyhow!(
                    "Failed to locate devy's state directory: neither XDG_STATE_HOME nor HOME is set"
                )
            })
    }
}

/// Where a directory created at `path` (with `create_dir_all`) would really be: each
/// component that exists is resolved as the kernel resolves it (symlinks followed, `..`
/// taken physically), and once one is missing the rest would be created fresh, so a
/// `..` there only removes the missing component before it.
fn resolve_planned(path: &Path) -> PathBuf {
    use std::path::Component;
    let Ok(absolute) = std::path::absolute(path) else {
        return path.to_path_buf();
    };
    let mut prefix = PathBuf::new();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => prefix.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if missing.pop().is_none() {
                    // `prefix` is canonical, so its lexical parent is its real parent.
                    prefix.pop();
                }
            }
            Component::Normal(part) => {
                if missing.is_empty() {
                    match fs::canonicalize(prefix.join(part)) {
                        Ok(real) => prefix = real,
                        Err(_) => missing.push(part.to_os_string()),
                    }
                } else {
                    missing.push(part.to_os_string());
                }
            }
        }
    }
    missing.iter().fold(prefix, |acc, part| acc.join(part))
}

/// Whether `base` (`<state>/devy`) is the platform default: on Unix, where
/// `$HOME/.local/state/devy` resolves to, whether `XDG_STATE_HOME` named it or not.
fn is_default_base(base: &Path, home: Option<&Path>) -> bool {
    if cfg!(windows) {
        return true;
    }
    home.is_some_and(|home| {
        resolve_planned(base) == resolve_planned(&home.join(".local").join("state").join("devy"))
    })
}

impl StateDir {
    #[cfg(test)]
    fn env_copy_dir(&self) -> PathBuf {
        self.base.join(crate::env_manager::shadowenv::COPY_SUBDIR)
    }

    /// The state directory at its standard location. Nothing is created until needed.
    #[cfg_attr(test, allow(dead_code))]
    pub fn locate() -> Result<Self> {
        let dir = Self::in_state_dir(&state_home()?);
        Ok(Self {
            default_location: is_default_base(&dir.base, absolute_var("HOME").as_deref()),
            ..dir
        })
    }

    /// The state directory under `state` (the `$XDG_STATE_HOME` equivalent).
    pub fn in_state_dir(state: &Path) -> Self {
        Self {
            base: state.join("devy"),
            default_location: false,
        }
    }

    /// Refuses a state directory the repository could supply. The one exception is the
    /// platform default location below a project at `$HOME` (a dotfiles repository), as
    /// long as git does not track it. A directory not created yet is judged by where it
    /// would be created (its nearest existing ancestor, resolved), so it is refused
    /// before devy writes there.
    fn refuse_inside(&self, canonical_root: &Path) -> Result<()> {
        let planned = resolve_planned(&self.base);
        let Ok(rel) = planned.strip_prefix(canonical_root) else {
            return Ok(());
        };
        if !self.default_location {
            bail!(
                "devy's state directory {} is inside this project; set XDG_STATE_HOME (or LOCALAPPDATA) to a directory outside it",
                self.base.display()
            );
        }
        if crate::fs_safe::git_tracked(canonical_root, &[rel.to_path_buf()])?.is_some() {
            bail!(
                "devy's state directory {} is tracked by git in this project; set XDG_STATE_HOME (or LOCALAPPDATA) to a directory outside it",
                self.base.display()
            );
        }
        Ok(())
    }

    /// Where devy keeps the copies of the `500_devy.lisp` files it wrote (see
    /// `shadowenv::COPY_SUBDIR`), `<state>/devy/shadowenv`, created (0700,
    /// owner-checked) when missing, for the project at `project_root`. Refused when it
    /// is inside the project.
    pub(crate) fn ensure_env_copy_dir(&self, project_root: &Path) -> Result<PathBuf> {
        let canonical = fs::canonicalize(project_root)
            .with_context(|| format!("Failed to resolve {}", project_root.display()))?;
        self.refuse_inside(&canonical)?;
        self.ensure_subdir(crate::env_manager::shadowenv::COPY_SUBDIR)
    }

    /// Creates `<base>/<name>` (0700, owner-checked, as is `<base>`) when missing and
    /// returns it.
    fn ensure_subdir(&self, name: &str) -> Result<PathBuf> {
        let state = self
            .base
            .parent()
            .ok_or_else(|| anyhow!("Failed to locate devy's state directory"))?;
        fs::create_dir_all(state)
            .with_context(|| format!("Failed to create {}", state.display()))?;
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        match builder.create(&self.base) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to create {}", self.base.display()));
            }
        }
        let meta = fs::symlink_metadata(&self.base)
            .with_context(|| format!("Failed to inspect {}", self.base.display()))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            bail!(
                "{} is not a directory — remove it so devy can keep its state there",
                self.base.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != crate::fs_safe::current_uid() {
                bail!(
                    "{} is not owned by the current user — remove it so devy can keep its state there",
                    self.base.display()
                );
            }
        }
        crate::fs_safe::private_dir(&self.base, name, false).map_err(|e| {
            anyhow!(
                "{e:#} (or run chmod 700 {} if you created it yourself)",
                self.base.join(name).display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> crate::test_support::TempDir {
        let project = crate::test_support::tmp_dir();
        std::fs::write(project.join("devy.yml"), "name: x\n").unwrap();
        project
    }

    #[cfg(unix)]
    #[test]
    fn copy_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let state = crate::test_support::tmp_dir();
        let dir = StateDir::in_state_dir(&state);
        let copies = dir.ensure_env_copy_dir(&project()).unwrap();
        assert_eq!(copies, dir.env_copy_dir());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&copies), 0o700);
        assert_eq!(mode(&dir.base), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_copy_directory_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let state = crate::test_support::tmp_dir();
        let dir = StateDir::in_state_dir(&state);
        let project = project();
        dir.ensure_env_copy_dir(&project).unwrap();
        std::fs::set_permissions(dir.env_copy_dir(), std::fs::Permissions::from_mode(0o777))
            .unwrap();
        let err = dir.ensure_env_copy_dir(&project).unwrap_err();
        assert!(format!("{err:#}").contains("expected 700"), "{err:#}");
    }

    #[test]
    fn untracked_state_dir_below_the_project_root_is_accepted() {
        // A project at $HOME has ~/.local/state below its root.
        let project = project();
        let dir = StateDir {
            default_location: true,
            ..StateDir::in_state_dir(&project.join(".local").join("state"))
        };
        dir.ensure_env_copy_dir(&project).unwrap();
    }

    #[test]
    fn planned_state_dir_inside_the_project_is_refused_before_it_exists() {
        let project = project();
        let dir = StateDir::in_state_dir(&project.join("state").join("deeper"));
        let err = dir.ensure_env_copy_dir(&project).unwrap_err();
        assert!(
            format!("{err:#}").contains("inside this project"),
            "{err:#}"
        );
        assert!(!project.join("state").exists(), "nothing was created");
        // `..` past a directory that does not exist yet still lands in the project.
        let sneaky = StateDir::in_state_dir(&project.join("nope").join("..").join("st"));
        assert!(sneaky.ensure_env_copy_dir(&project).is_err());
    }

    /// `link/..` is taken physically (the link's target's parent), as `create_dir_all`
    /// takes it, not lexically.
    #[cfg(unix)]
    #[test]
    fn planned_state_dir_through_link_dotdot_is_resolved_physically() {
        let project = project();
        std::fs::create_dir(project.join("inside")).unwrap();
        let elsewhere = crate::test_support::tmp_dir();
        std::os::unix::fs::symlink(project.join("inside"), elsewhere.join("link")).unwrap();
        let state = elsewhere.join("link").join("..").join("missing").join("..");
        assert_eq!(
            resolve_planned(&state.join("devy")),
            std::fs::canonicalize(&*project).unwrap().join("devy")
        );
        let err = StateDir::in_state_dir(&state)
            .ensure_env_copy_dir(&project)
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("inside this project"),
            "{err:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn planned_state_dir_reached_through_a_symlink_into_the_project_is_refused() {
        let project = project();
        std::fs::create_dir(project.join("inside")).unwrap();
        let elsewhere = crate::test_support::tmp_dir();
        std::os::unix::fs::symlink(project.join("inside"), elsewhere.join("link")).unwrap();
        let dir = StateDir::in_state_dir(&elsewhere.join("link").join("state"));
        let err = dir.ensure_env_copy_dir(&project).unwrap_err();
        assert!(
            format!("{err:#}").contains("inside this project"),
            "{err:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn default_location_is_decided_by_the_resolved_path() {
        let home = crate::test_support::tmp_dir();
        let default_state = home.join(".local").join("state");
        // XDG_STATE_HOME set to the default (or a path resolving to it) is the default.
        assert!(is_default_base(&default_state.join("devy"), Some(&home)));
        std::fs::create_dir_all(&default_state).unwrap();
        let alias = crate::test_support::tmp_dir();
        std::os::unix::fs::symlink(&default_state, alias.join("st")).unwrap();
        assert!(is_default_base(&alias.join("st").join("devy"), Some(&home)));
        // Anywhere else is not, nor is anything without HOME.
        assert!(!is_default_base(
            &home.join("state").join("devy"),
            Some(&home)
        ));
        assert!(!is_default_base(&default_state.join("devy"), None));
    }

    #[test]
    fn state_dir_inside_the_project_at_a_chosen_location_is_refused() {
        let project = project();
        std::fs::create_dir_all(project.join("state").join("devy")).unwrap();
        let dir = StateDir::in_state_dir(&project.join("state"));
        let err = dir.ensure_env_copy_dir(&project).unwrap_err();
        assert!(
            format!("{err:#}").contains("inside this project"),
            "{err:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_dir_tracked_by_git_in_the_project_is_refused() {
        let Some(git) = [
            "/usr/bin/git",
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
        ]
        .into_iter()
        .find(|p| Path::new(p).exists()) else {
            return;
        };
        let project = project();
        let dir = StateDir {
            default_location: true,
            ..StateDir::in_state_dir(&project.join("state"))
        };
        dir.ensure_env_copy_dir(&project).unwrap();
        std::fs::write(dir.env_copy_dir().join("x.lisp"), "x").unwrap();
        let git_ok = |args: &[&str]| {
            std::process::Command::new(git)
                .args(args)
                .current_dir(&*project)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        assert!(git_ok(&["init", "-q"]));
        assert!(git_ok(&["add", "-f", "--", "state"]));
        let err = dir.ensure_env_copy_dir(&project).unwrap_err();
        assert!(format!("{err:#}").contains("tracked by git"), "{err:#}");
    }
}
