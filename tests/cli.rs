//! Integration tests — invoke the compiled `devy` binary as a subprocess.
//!
//! These tests exercise the CLI end-to-end on the real filesystem without
//! calling any real package manager. Safe to run on every platform.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

// ── binary path ───────────────────────────────────────────────────────────────

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_devy"))
}

// ── TempProject ───────────────────────────────────────────────────────────────

/// A temporary project directory with a `.git` marker so `find_config` stops
/// its upward walk here. Deleted on drop.
struct TempProject {
    dir: PathBuf,
}

static N: AtomicU64 = AtomicU64::new(0);

impl TempProject {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "devy_itest_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        TempProject { dir }
    }

    fn with_yaml(content: &str) -> Self {
        let proj = Self::new();
        std::fs::write(proj.dir.join("devy.yml"), content).unwrap();
        proj
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(binary())
            .args(args)
            .current_dir(&self.dir)
            .output()
            .expect("failed to execute devy binary")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// init
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn init_creates_devy_yml() {
    let proj = TempProject::new();
    let out = proj.run(&["init"]);
    assert!(
        out.status.success(),
        "devy init must succeed in an empty project"
    );
    assert!(
        proj.file("devy.yml").exists(),
        "devy init must create devy.yml"
    );
}

#[test]
fn init_output_mentions_created_file() {
    let proj = TempProject::new();
    let out = proj.run(&["init"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("devy.yml"),
        "init output must mention the created file; got: {stdout}"
    );
}

#[test]
fn init_fails_when_config_exists_without_force() {
    let proj = TempProject::with_yaml("name: existing\n");
    let out = proj.run(&["init"]);
    assert!(
        !out.status.success(),
        "init must fail when devy.yml already exists"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already exists") || stderr.contains("--force"),
        "error must mention --force or 'already exists'; got: {stderr}"
    );
}

#[test]
fn init_force_overwrites_existing_config() {
    let proj = TempProject::with_yaml("name: old\n");
    let out = proj.run(&["init", "--force"]);
    assert!(out.status.success(), "init --force must succeed");
    let content = std::fs::read_to_string(proj.file("devy.yml")).unwrap();
    assert!(
        !content.contains("name: old"),
        "init --force must overwrite the old config"
    );
    assert!(
        content.contains("dependencies"),
        "new config must contain a dependencies key"
    );
}

#[test]
fn init_output_is_parseable_yaml() {
    let proj = TempProject::new();
    proj.run(&["init"]);
    let content = std::fs::read_to_string(proj.file("devy.yml")).unwrap();
    assert!(!content.trim().is_empty(), "init must write non-empty YAML");
    assert!(
        content.contains("dependencies"),
        "output YAML must have a dependencies key"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// hook
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn hook_zsh_exits_zero() {
    let proj = TempProject::new();
    assert!(proj.run(&["hook", "zsh"]).status.success());
}

#[test]
fn hook_bash_exits_zero() {
    let proj = TempProject::new();
    assert!(proj.run(&["hook", "bash"]).status.success());
}

#[test]
fn hook_fish_exits_zero() {
    let proj = TempProject::new();
    assert!(proj.run(&["hook", "fish"]).status.success());
}

#[test]
fn hook_unsupported_shell_exits_nonzero() {
    let proj = TempProject::new();
    let out = proj.run(&["hook", "powershell"]);
    assert!(
        !out.status.success(),
        "hook with unsupported shell must exit non-zero"
    );
}

#[test]
fn hook_unsupported_shell_error_names_the_shell() {
    let proj = TempProject::new();
    let out = proj.run(&["hook", "powershell"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("powershell"),
        "error must name the unsupported shell; got: {stderr}"
    );
}

#[test]
fn hook_zsh_output_references_shadowenv() {
    let proj = TempProject::new();
    let out = proj.run(&["hook", "zsh"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("shadowenv"),
        "zsh snippet must reference shadowenv"
    );
}

#[test]
fn hook_bash_output_references_shadowenv() {
    let proj = TempProject::new();
    let out = proj.run(&["hook", "bash"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("shadowenv"),
        "bash snippet must reference shadowenv"
    );
}

#[test]
fn hook_fish_output_references_shadowenv() {
    let proj = TempProject::new();
    let out = proj.run(&["hook", "fish"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("shadowenv"),
        "fish snippet must reference shadowenv"
    );
}

#[test]
fn hook_output_does_not_contain_unsubstituted_placeholder() {
    // Verifies make_snippet replaced {bin} in every snippet.
    for shell in &["zsh", "bash", "fish"] {
        let proj = TempProject::new();
        let out = proj.run(&["hook", shell]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("{bin}"),
            "hook {shell} output must not contain the literal {{bin}} placeholder"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// _commands
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn commands_exits_zero_with_user_defined_commands() {
    let proj =
        TempProject::with_yaml("name: test\ncommands:\n  dev: npm run dev\n  build: cargo build\n");
    let out = proj.run(&["_commands"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dev"), "_commands must list 'dev'");
    assert!(stdout.contains("build"), "_commands must list 'build'");
}

#[test]
fn commands_exits_zero_with_no_commands_defined() {
    let proj = TempProject::with_yaml("name: test\ndependencies: []\n");
    assert!(proj.run(&["_commands"]).status.success());
}

#[test]
fn commands_exits_zero_without_config() {
    // _commands is called by shell completions — it must never error, even outside a project.
    let proj = TempProject::new();
    let out = proj.run(&["_commands"]);
    assert!(
        out.status.success(),
        "_commands must exit 0 even when devy.yml is missing"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "_commands must produce empty output when there is no config"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// check
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn check_exits_zero_with_empty_dependencies() {
    // No deps, no env — nothing to verify, must pass. No PM methods are called.
    let proj = TempProject::with_yaml("name: test\ndependencies: []\n");
    assert!(
        proj.run(&["check"]).status.success(),
        "check must succeed when there is nothing to verify"
    );
}

#[test]
fn check_exits_nonzero_without_config() {
    let proj = TempProject::new();
    assert!(
        !proj.run(&["check"]).status.success(),
        "check must fail when devy.yml is missing"
    );
}

#[test]
fn check_missing_config_error_mentions_devy_yml() {
    let proj = TempProject::new();
    let out = proj.run(&["check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("devy.yml"),
        "error must mention devy.yml when config is missing; got: {stderr}"
    );
}

#[test]
fn check_exits_nonzero_with_malformed_yaml() {
    let proj = TempProject::with_yaml("dependencies: [unclosed bracket\n");
    assert!(
        !proj.run(&["check"]).status.success(),
        "check must fail on malformed YAML"
    );
}

#[test]
fn check_exits_nonzero_with_unknown_top_level_key() {
    // serde deny_unknown_fields catches typos like "dependecies".
    let proj = TempProject::with_yaml("dependecies:\n  - node\n");
    assert!(
        !proj.run(&["check"]).status.success(),
        "check must fail when the config has an unknown top-level key"
    );
}

#[test]
fn check_exits_nonzero_with_port_conflict() {
    // Explicit identical ports conflict on every backend. Explicit ports are resolved
    // before any package manager method is called, so this is safe on all platforms.
    let proj = TempProject::with_yaml(
        "name: test\ndependencies:\n  - mysql:\n      port: 3307\n  - mariadb:\n      port: 3307\n",
    );
    let out = proj.run(&["check"]);
    assert!(
        !out.status.success(),
        "check must fail when two services share a port"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("port conflict: 'mysql' and 'mariadb' both use port 3307"),
        "error must name both services and the port; got: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn check_no_port_conflict_for_unassigned_ports_under_nix() {
    // Under nix, `up` would give mysql and mariadb distinct random ports, so `check`
    // must not report a conflict. The project profile doesn't exist, so nothing is
    // reported installed and no nix command runs.
    let proj = TempProject::with_yaml(
        "name: test\npackage_manager: nix\ndependencies:\n  - mysql\n  - mariadb\n",
    );
    let out = proj.run(&["check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("port conflict"),
        "check must not report a conflict for unassigned ports; got: {stderr}"
    );
    assert!(
        !proj.file("devy.lock").exists(),
        "check must not write devy.lock"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// up --dry-run
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn up_dry_run_exits_zero_with_empty_dependencies() {
    let proj = TempProject::with_yaml("name: test\ndependencies: []\n");
    assert!(
        proj.run(&["up", "--dry-run"]).status.success(),
        "up --dry-run must succeed when there is nothing to verify"
    );
}

#[test]
fn up_dry_run_does_not_write_lock_file() {
    let proj = TempProject::with_yaml("name: test\ndependencies: []\n");
    proj.run(&["up", "--dry-run"]);
    assert!(
        !proj.file("devy.lock").exists(),
        "up --dry-run must not write devy.lock"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// general CLI
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn help_flag_exits_zero() {
    assert!(
        Command::new(binary())
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn help_output_lists_key_subcommands() {
    let out = Command::new(binary()).arg("--help").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    for cmd in &["up", "down", "check", "init", "hook", "status"] {
        assert!(
            stdout.contains(cmd),
            "--help output must list the '{cmd}' subcommand"
        );
    }
}
