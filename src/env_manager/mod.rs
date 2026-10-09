pub mod shadowenv;

pub use shadowenv::Shadowenv;

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub trait EnvManager {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    /// Writes the environment for `dir` and tells the environment tool to load it.
    fn setup(
        &self,
        dir: &Path,
        vars: &HashMap<String, String>,
        path_prepends: &[String],
    ) -> Result<()>;
    /// Reads back the variables previously written by `setup`. Returns `None` if the
    /// env file has not been created yet.
    fn read_vars(&self, project_root: &Path) -> Option<HashMap<String, String>>;
    /// Reads back PATH prepend entries previously written by `setup`. Returns `None` if the
    /// env file has not been created yet.
    fn read_path_prepends(&self, project_root: &Path) -> Option<Vec<String>>;
}

#[cfg(test)]
pub struct MockEnvManager {
    pub is_available: bool,
    pub setup_fails: bool,
    pub setup_called: std::cell::Cell<bool>,
    pub read_vars_returns_some: bool,
    /// Variables passed to the last `setup` call.
    pub last_vars: std::cell::RefCell<HashMap<String, String>>,
    /// PATH entries passed to the last `setup` call.
    pub last_path_prepends: std::cell::RefCell<Vec<String>>,
}

#[cfg(test)]
impl Default for MockEnvManager {
    fn default() -> Self {
        Self {
            is_available: true,
            setup_fails: false,
            setup_called: std::cell::Cell::new(false),
            read_vars_returns_some: false,
            last_vars: std::cell::RefCell::new(HashMap::new()),
            last_path_prepends: std::cell::RefCell::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl EnvManager for MockEnvManager {
    fn name(&self) -> &str {
        "mock-env"
    }
    fn is_available(&self) -> bool {
        self.is_available
    }
    fn setup(
        &self,
        _dir: &Path,
        vars: &HashMap<String, String>,
        path_prepends: &[String],
    ) -> Result<()> {
        self.setup_called.set(true);
        *self.last_vars.borrow_mut() = vars.clone();
        *self.last_path_prepends.borrow_mut() = path_prepends.to_vec();
        if self.setup_fails {
            anyhow::bail!("mock env setup failure")
        } else {
            Ok(())
        }
    }
    fn read_vars(&self, _project_root: &Path) -> Option<HashMap<String, String>> {
        if self.read_vars_returns_some {
            Some(HashMap::new())
        } else {
            None
        }
    }
    /// The entries of the last `setup`, as the written file would hold them.
    fn read_path_prepends(&self, _project_root: &Path) -> Option<Vec<String>> {
        self.setup_called
            .get()
            .then(|| self.last_path_prepends.borrow().clone())
    }
}
