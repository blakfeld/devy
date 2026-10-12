// Test port allocation:
// Tests that probe for a closed port pick a port in the 19980–19999 range, which is
// unlikely to be in use on developer machines. Each service module uses a distinct port
// within that range to prevent cross-test interference when the test suite runs in
// parallel. The ranges were shifted from 19994–19997 to 19981–19984 to avoid conflicts
// introduced by the Nix backend's health-check tests, which bind their own ports.

// Process-global state:
// Rust's test harness runs tests on parallel threads that share the environment, the
// current directory and the temp directory. Unit tests must not call
// `std::env::set_var` / `remove_var`: a lock only orders the tests that take it, while
// any other test reading the variable (`installers::user_home()` reads `$HOME`) still
// sees the change. Give the code under test a `*_with` / `*_from` variant that takes the
// value instead (see `installers::installer_command_with`, `DevyConfig::discover_with_home`,
// `Homebrew::brew_bin_with`). Temp paths come from `tmp_dir` / `tmp_path`, never from a
// fixed name, and the recorded project root (`fs_safe::set_project_root`) is per thread
// under test.

/// A temporary directory that is deleted automatically when dropped.
/// Implements `Deref<Target = Path>` so it can be used wherever `&Path` is expected.
pub struct TempDir(std::path::PathBuf);

impl std::ops::Deref for TempDir {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::path::Path> for TempDir {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates a unique temporary directory that is deleted automatically when the returned
/// `TempDir` is dropped — even if the test panics.
pub fn tmp_dir() -> TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    // Created exclusively: a directory left behind by an earlier run whose pid has been
    // reused (an aborted run skips `Drop`) must never be handed out with its old
    // contents, so on a collision move on to the next name.
    loop {
        let dir = std::env::temp_dir().join(format!(
            "devy_test_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return TempDir(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("creating {}: {e}", dir.display()),
        }
    }
}

/// A [`tmp_dir`] private to the user (mode 0700 on Unix), as devy requires of the
/// directory it reads launchd service logs from.
pub fn private_tmp_dir() -> TempDir {
    let dir = tmp_dir();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

/// A [`tmp_dir`] the current user can't write to (mode 0555); its mode is restored on
/// drop so it can be removed.
#[cfg(unix)]
pub struct ReadOnlyDir(TempDir);

#[cfg(unix)]
impl std::ops::Deref for ReadOnlyDir {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

#[cfg(unix)]
impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&*self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// A [`ReadOnlyDir`] holding whatever `setup` puts in it first. `None` when the directory
/// is still writable after the mode change, as it is for root: callers skip the test.
#[cfg(unix)]
pub fn read_only_dir(setup: impl FnOnce(&std::path::Path)) -> Option<ReadOnlyDir> {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp_dir();
    setup(&dir);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let dir = ReadOnlyDir(dir);
    let probe = dir.join(".devy-write-probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return None;
    }
    Some(dir)
}

/// A temporary file path that is deleted automatically when dropped.
/// The file is not created by `tmp_path`; creation happens when the test writes to it.
pub struct TempFile {
    path: std::path::PathBuf,
    // The fresh directory holding `path`; removed (with the file) on drop.
    _dir: TempDir,
}

impl std::ops::Deref for TempFile {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

impl AsRef<std::path::Path> for TempFile {
    fn as_ref(&self) -> &std::path::Path {
        &self.path
    }
}

/// Returns a unique temporary file path that is deleted automatically when the returned
/// `TempFile` is dropped. The file itself is not created until the test writes to it.
///
/// The path lives in its own exclusively created `tmp_dir`, so it can neither collide
/// with another test's path nor with a leftover from an earlier run whose pid has been
/// reused, and nobody else can plant a file or symlink there before the test writes.
pub fn tmp_path(suffix: &str) -> TempFile {
    let dir = tmp_dir();
    TempFile {
        path: dir.join(format!("file{suffix}")),
        _dir: dir,
    }
}

/// Constructs a `DevyConfig` with the given dependency names and environment vars.
/// Shared across command test modules to avoid triplicating the same helper.
pub fn make_config(
    dep_names: &[&str],
    env: std::collections::HashMap<String, String>,
) -> crate::config::DevyConfig {
    crate::config::DevyConfig {
        name: Some("test".into()),
        dependencies: dep_names
            .iter()
            .map(|n| crate::config::RawDependency::Simple(n.to_string()))
            .collect(),
        environment: env,
        commands: std::collections::HashMap::new(),
        hooks: Default::default(),
        package_manager: Default::default(),
        service_manager: Default::default(),
        container_cli: Default::default(),
    }
}

/// A "billion laughs" YAML document: nine levels of anchored lists, each repeating the
/// previous level ten times (10^9 strings if expanded), as list items under `top_key`.
pub fn billion_laughs(top_key: &str) -> String {
    let lols = ["\"lol\""; 10].join(",");
    let mut doc = format!("{top_key}:\n  - &a [{lols}]\n");
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    for pair in names.windows(2) {
        let (prev, cur) = (pair[0], pair[1]);
        let refs = vec![format!("*{prev}"); 10].join(",");
        doc.push_str(&format!("  - &{cur} [{refs}]\n"));
    }
    doc
}

/// A flat alias-amplification YAML document: one anchored list of `k` scalars and `m`
/// aliases to it under `top_key`, so `m * k` nodes with no nesting.
pub fn flat_quadratic(top_key: &str, k: usize, m: usize) -> String {
    let items = vec!["x"; k].join(",");
    let mut doc = format!("{top_key}:\n  - &a [{items}]\n");
    for _ in 0..m {
        doc.push_str("  - *a\n");
    }
    doc
}

/// Lays out a git repository under `root` as `git worktree add` leaves it, without
/// running git: a main checkout `<root>/app` (a `.git` directory) and a linked worktree
/// `<root>/app-feat` (a `.git` file pointing at `app/.git/worktrees/app-feat`). Returns
/// both checkouts, canonicalized as worktree detection reports them.
pub fn fake_linked_worktree(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::fs;
    let main = root.join("app");
    let feat = root.join("app-feat");
    let admin = main.join(".git/worktrees/app-feat");
    fs::create_dir_all(&admin).unwrap();
    fs::create_dir_all(&feat).unwrap();
    fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(admin.join("HEAD"), "ref: refs/heads/feat\n").unwrap();
    fs::write(admin.join("commondir"), "../..\n").unwrap();
    fs::write(
        admin.join("gitdir"),
        format!("{}\n", feat.join(".git").display()),
    )
    .unwrap();
    fs::write(feat.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    (main.canonicalize().unwrap(), feat.canonicalize().unwrap())
}
