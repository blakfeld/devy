// Test port allocation:
// Tests that probe for a closed port pick a port in the 19980–19999 range, which is
// unlikely to be in use on developer machines. Each service module uses a distinct port
// within that range to prevent cross-test interference when the test suite runs in
// parallel. The ranges were shifted from 19994–19997 to 19981–19984 to avoid conflicts
// introduced by the Nix backend's health-check tests, which bind their own ports.

/// Process-wide mutex for tests that mutate environment variables.
///
/// Rust's test harness runs tests in parallel threads. Any test that calls
/// `std::env::set_var` or `std::env::remove_var` must hold this lock for
/// the duration of the mutation *and* the code under test that reads the
/// variable. Tests in other modules that read the same variables (e.g.
/// `rustup_bin()` reads `$HOME`) must also hold this lock.
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let dir = std::env::temp_dir().join(format!(
        "devy_test_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// A temporary file path that is deleted automatically when dropped.
/// The file is not created by `tmp_path`; creation happens when the test writes to it.
pub struct TempFile(std::path::PathBuf);

impl std::ops::Deref for TempFile {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::path::Path> for TempFile {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Returns a unique temporary file path that is deleted automatically when the returned
/// `TempFile` is dropped. The file itself is not created until the test writes to it.
pub fn tmp_path(suffix: &str) -> TempFile {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    TempFile(std::env::temp_dir().join(format!(
        "devy_test_{}_{}{suffix}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )))
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
