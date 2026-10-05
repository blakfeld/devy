use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

use crate::config::Dependency;
use crate::installers;
use crate::output;
use crate::package_manager::PackageManager;

use super::helpers::{stamp_matches, write_stamp};
use super::{Module, extra_list, run_cmd};

pub struct RustModule;

impl RustModule {
    fn toolchain(dep: &Dependency) -> &str {
        dep.extra
            .get("toolchain")
            .and_then(|v| v.as_str())
            .unwrap_or("stable")
    }
}

/// Builds `rustup <kind> add --toolchain=<toolchain> -- <item>` for `kind` = `target` or
/// `component`. The toolchain is bound to its option with `=` and the item follows `--`,
/// so neither configured value can be parsed as a rustup option.
fn rustup_add_args(kind: &str, toolchain: &str, item: &str) -> Vec<String> {
    vec![
        kind.to_string(),
        "add".into(),
        format!("--toolchain={toolchain}"),
        "--".into(),
        item.to_string(),
    ]
}

/// Rejects a toolchain name that is not a plain channel/version name (an ASCII letter or
/// digit followed by letters, digits, `.`, `_` or `-`). rustup accepts path toolchains for
/// `default`, so a path here could point the user's global toolchain at the repository.
fn checked_toolchain(toolchain: &str) -> Result<&str> {
    if !crate::validate::toolchain(toolchain) {
        bail!("invalid Rust toolchain '{toolchain}'");
    }
    Ok(toolchain)
}

/// The value of the environment variable `key` when it is an absolute directory outside
/// the project (a repository's environment can't point rustup at its own directory).
fn home_var(key: &str) -> Option<String> {
    let value = std::env::var(key).ok()?;
    crate::fs_safe::dir_outside_project(value.as_ref()).then_some(value)
}

/// `CARGO_HOME`, when set to an absolute directory outside the project.
fn cargo_home() -> Option<String> {
    home_var("CARGO_HOME")
}

/// The user's `CARGO_HOME` / `RUSTUP_HOME` (see `home_var`), for the rustup installer,
/// which otherwise gets a scrubbed environment.
fn rust_homes() -> Vec<(&'static str, String)> {
    ["CARGO_HOME", "RUSTUP_HOME"]
        .into_iter()
        .filter_map(|key| Some((key, home_var(key)?)))
        .collect()
}

/// Environment for the rustup installer: the pinned `RUSTUP_VERSION` plus `rust_homes`.
fn rustup_installer_env() -> Vec<(&'static str, String)> {
    let mut env = vec![("RUSTUP_VERSION", installers::RUSTUP_VERSION.to_string())];
    env.extend(rust_homes());
    env
}

/// `$CARGO_HOME/bin/rustup` (default `~/.cargo/bin/rustup`), used when rustup is not yet
/// on PATH after a fresh install. Returns None when neither CARGO_HOME (absolute, outside
/// the project) nor HOME is set (CI/sudo/Docker environments).
fn rustup_bin() -> Option<String> {
    let exe = std::env::consts::EXE_SUFFIX;
    if let Some(cargo_home) = cargo_home() {
        return Some(format!("{cargo_home}/bin/rustup{exe}"));
    }
    // The same home the rustup installer runs with (see `installers::user_home`).
    installers::user_home().map(|home| format!("{}/.cargo/bin/rustup{exe}", home.to_string_lossy()))
}

/// The binary `tool` (`rustc`, `cargo`) next to the rustup at `rustup` (as `rustup_bin`
/// names it, with the platform's executable suffix).
fn sibling_of_rustup(rustup: &str, tool: &str) -> Option<String> {
    let exe = std::env::consts::EXE_SUFFIX;
    rustup
        .strip_suffix(&format!("rustup{exe}"))
        .map(|dir| format!("{dir}{tool}{exe}"))
}

/// The `rustc` to ask for a version: the one on PATH outside the project (a planted
/// project-local `rustc` is never run), else the one next to `rustup()` (the rustup
/// devy would have installed), else none — never a bare `rustc` resolved from the full
/// PATH. `rustup` is only called when rustc is not on PATH.
fn rustc_program(rustup: impl FnOnce() -> Option<String>) -> Option<String> {
    rustc_program_from(crate::fs_safe::which_outside_project("rustc"), rustup)
}

/// `rustc_program` with the PATH lookup result injected.
fn rustc_program_from(
    on_path: Option<std::path::PathBuf>,
    rustup: impl FnOnce() -> Option<String>,
) -> Option<String> {
    on_path
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| rustup().and_then(|ru| sibling_of_rustup(&ru, "rustc")))
}

/// The rustup to run: the one the installer puts in `$CARGO_HOME/bin` / `~/.cargo/bin`
/// when it exists, else one on PATH outside the project.
fn rustup_program() -> Option<String> {
    rustup_bin().filter(|p| Path::new(p).is_file()).or_else(|| {
        crate::fs_safe::which_outside_project("rustup").map(|p| p.to_string_lossy().into_owned())
    })
}

/// Files that make cargo and rustc run code the repository chooses: a toolchain file can
/// name a `path` toolchain (rustup's proxies then run binaries from that directory) and a
/// cargo config can set `rustc`, `rustc-wrapper`, linkers, runners and aliases.
const REPO_TOOLCHAIN_FILES: &[&str] = &[
    "rust-toolchain",
    "rust-toolchain.toml",
    ".cargo/config",
    ".cargo/config.toml",
];

/// The `REPO_TOOLCHAIN_FILES` cargo and rustup would read when run in `project_root`:
/// those in it and in every parent directory (both tools walk up), shown relative to the
/// project (`../.cargo/config.toml`). The walk stops below `stop` (the user's home
/// directory, whose `~/.cargo/config.toml` is the user's own), except that the project
/// root itself is always scanned: a project at the home directory (a dotfiles
/// repository) still has its own files listed, and only those. Dangling symlinks count.
fn repo_toolchain_files(project_root: &Path, stop: Option<&Path>) -> Vec<String> {
    let mut found = Vec::new();
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let stop = stop.map(canonical);
    let root = canonical(project_root);
    // A project at the stop directory: only its own files, never its parents'.
    let at_stop = stop.as_deref() == Some(root.as_path());
    for (depth, dir) in root.ancestors().enumerate() {
        if depth > 0 && (at_stop || stop.as_deref().is_some_and(|h| dir == h)) {
            break;
        }
        let prefix = "../".repeat(depth);
        for f in REPO_TOOLCHAIN_FILES {
            if std::fs::symlink_metadata(dir.join(f)).is_ok() {
                found.push(format!("{prefix}{f}"));
            }
        }
    }
    found
}

impl Module for RustModule {
    fn known_extra_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["toolchain", "targets", "components"])
    }

    fn nix_attr(&self, _dep: &Dependency) -> Option<String> {
        Some("rustup".to_string())
    }

    fn is_installed(&self, _pm: &dyn PackageManager, _dep: &Dependency) -> Result<bool> {
        Ok(rustup_program().is_some())
    }

    fn install(&self, _pm: &dyn PackageManager, dep: &Dependency) -> Result<()> {
        if rustup_program().is_none() {
            let status = installers::run_script(
                &installers::RUSTUP,
                installers::Interpreter::Sh,
                &["-y", "--no-modify-path"].map(Into::into),
                // Fetch the rustup-init archived for the pinned script's release rather
                // than whatever is latest.
                &rustup_installer_env()
                    .iter()
                    .map(|(k, v)| (*k, v.as_str()))
                    .collect::<Vec<_>>(),
            )?;
            if !status.success() {
                bail!("rustup installation failed — check the output above for details");
            }
        }

        let ru = rustup_program()
            .context("rustup is not installed in ~/.cargo/bin or on PATH outside the project")?;
        let toolchain = checked_toolchain(Self::toolchain(dep))?;

        run_cmd(&ru, &["toolchain", "install", "--", toolchain])?;
        run_cmd(&ru, &["default", "--", toolchain])?;

        for target in extra_list(dep, "targets")? {
            let args = rustup_add_args("target", toolchain, &target);
            run_cmd(&ru, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
        }
        for component in extra_list(dep, "components")? {
            let args = rustup_add_args("component", toolchain, &component);
            run_cmd(&ru, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
        }

        Ok(())
    }

    fn source(&self) -> Option<&'static str> {
        Some("rustup")
    }

    fn setup_steps(&self, _dep: &Dependency, project_root: &Path) -> Vec<String> {
        let mut steps =
            super::helpers::step_if_exists(project_root, "Cargo.toml", "cargo fetch (Cargo.toml)");
        let home = installers::user_home().map(std::path::PathBuf::from);
        let present = repo_toolchain_files(project_root, home.as_deref());
        if !present.is_empty() {
            steps.push(format!(
                "cargo and rustc use toolchain and cargo settings from the project or its parent directories ({})",
                present.join(", ")
            ));
        }
        steps
    }

    fn post_setup(
        &self,
        _dep: &Dependency,
        _pm: &dyn PackageManager,
        project_root: &Path,
    ) -> Result<()> {
        let cargo_toml = project_root.join("Cargo.toml");
        if !cargo_toml.exists() {
            return Ok(());
        }
        let lock = project_root.join("Cargo.lock");
        let manifest = if lock.exists() { lock } else { cargo_toml };
        let stamp_path = project_root.join(".devy_rust_stamp");
        if stamp_matches(&stamp_path, &manifest) {
            output::skip("Rust dependencies up to date");
            return Ok(());
        }
        output::step("Running cargo fetch");
        let cargo = crate::fs_safe::which_outside_project("cargo")
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| {
                rustup_bin().and_then(|ru| sibling_of_rustup(&ru, "cargo"))
            })
            .filter(|p| Path::new(p).is_file())
            .context(
                "cargo was not found in $CARGO_HOME/bin (~/.cargo/bin) or on PATH outside the project",
            )?;
        let status = Command::new(cargo)
            .arg("fetch")
            .current_dir(project_root)
            .status()
            .context("Failed to run `cargo fetch`")?;
        if !status.success() {
            anyhow::bail!("`cargo fetch` failed — check the output above for details");
        }
        write_stamp(&stamp_path, &manifest)?;
        output::success("Rust dependencies fetched");
        Ok(())
    }

    fn resolved_version(
        &self,
        _pm: &dyn PackageManager,
        _dep: &Dependency,
    ) -> Result<Option<String>> {
        let Some(rustc) = rustc_program(rustup_bin) else {
            return Ok(None);
        };
        // From `/`, so a rustup proxy doesn't pick up the project's `rust-toolchain(.toml)`
        // (which can name a `path` toolchain) and run a repository binary for `--version`.
        // `RUSTUP_TOOLCHAIN` can name a path toolchain too, so it is dropped for the probe.
        let out = Command::new(rustc)
            .arg("--version")
            .current_dir("/")
            .env_remove("RUSTUP_TOOLCHAIN")
            .output();
        // "rustc 1.78.0 (9b00956e5 2024-04-29)" — take second token
        Ok(out.ok().and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()?
                .split_whitespace()
                .nth(1)
                .map(String::from)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn checked_toolchain_accepts_channels_and_versions() {
        for t in [
            "stable",
            "nightly-2024-01-01",
            "1.78.0",
            "stable-aarch64-apple-darwin",
        ] {
            assert_eq!(checked_toolchain(t).unwrap(), t);
        }
    }

    #[test]
    fn checked_toolchain_rejects_paths_and_options() {
        for t in ["", "-v", "./tc", "/opt/tc", "..", "a\\b", "a b"] {
            assert!(checked_toolchain(t).is_err(), "{t} must be rejected");
        }
    }

    #[test]
    fn rustup_add_args_bind_toolchain_and_use_separator() {
        assert_eq!(
            rustup_add_args("target", "stable", "wasm32-unknown-unknown"),
            [
                "target",
                "add",
                "--toolchain=stable",
                "--",
                "wasm32-unknown-unknown"
            ]
        );
        assert_eq!(
            rustup_add_args("component", "nightly", "clippy"),
            ["component", "add", "--toolchain=nightly", "--", "clippy"]
        );
    }

    fn dep_with_toolchain(toolchain: &str) -> Dependency {
        let mut extra = HashMap::new();
        extra.insert(
            "toolchain".into(),
            crate::config::ExtraValue::String(toolchain.into()),
        );
        Dependency::with_extra("rust", extra)
    }

    // ── toolchain ─────────────────────────────────────────────────────────────

    #[test]
    fn toolchain_defaults_to_stable() {
        let dep = Dependency::simple("rust");
        assert_eq!(RustModule::toolchain(&dep), "stable");
    }

    #[test]
    fn toolchain_reads_custom_value() {
        let dep = dep_with_toolchain("nightly");
        assert_eq!(RustModule::toolchain(&dep), "nightly");
    }

    #[test]
    fn toolchain_reads_pinned_version() {
        let dep = dep_with_toolchain("1.78.0");
        assert_eq!(RustModule::toolchain(&dep), "1.78.0");
    }

    // ── source ────────────────────────────────────────────────────────────────

    #[test]
    fn rust_module_source_is_rustup() {
        assert_eq!(RustModule.source(), Some("rustup"));
    }

    // ── rustup_bin ────────────────────────────────────────────────────────────

    #[test]
    fn rustup_bin_returns_some_with_non_empty_home() {
        if cargo_home().is_none() && installers::user_home().is_some() {
            let bin = rustup_bin().unwrap();
            assert!(
                bin.contains(".cargo/bin/rustup"),
                "Expected ~/.cargo/bin/rustup, got {bin}"
            );
        }
    }

    #[test]
    fn rustup_bin_path_contains_home() {
        if let Some(cargo_home) = cargo_home() {
            assert_eq!(
                rustup_bin().unwrap(),
                format!("{cargo_home}/bin/rustup{}", std::env::consts::EXE_SUFFIX)
            );
        } else if let Some(home) = installers::user_home() {
            let bin = rustup_bin().unwrap();
            assert!(bin.starts_with(&*home.to_string_lossy()));
        }
    }

    #[test]
    fn rustup_installer_env_pins_version() {
        let env = rustup_installer_env();
        assert_eq!(
            env[0],
            ("RUSTUP_VERSION", installers::RUSTUP_VERSION.to_string())
        );
        assert!(env[1..].iter().all(|(k, v)| {
            ["CARGO_HOME", "RUSTUP_HOME"].contains(k) && std::path::Path::new(v).is_absolute()
        }));
    }

    // ── is_installed ─────────────────────────────────────────────────────────

    #[test]
    fn rust_is_installed_does_not_panic() {
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        let _ = RustModule.is_installed(&pm, &dep);
    }

    #[test]
    fn rust_is_installed_true_when_cargo_on_path() {
        // cargo is always on PATH in a Rust development environment.
        if which::which("cargo").is_err() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        assert!(RustModule.is_installed(&pm, &dep).unwrap());
    }

    // ── resolved_version ─────────────────────────────────────────────────────

    #[test]
    fn rust_resolved_version_returns_ok() {
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        assert!(RustModule.resolved_version(&pm, &dep).is_ok());
    }

    #[test]
    fn rust_resolved_version_is_some_when_rustc_installed() {
        if which::which("rustc").is_err() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        let ver = RustModule.resolved_version(&pm, &dep).unwrap();
        assert!(
            ver.is_some(),
            "Expected Some version since rustc is on PATH"
        );
        let v = ver.unwrap();
        assert!(!v.is_empty(), "Expected non-empty version string");
        assert_ne!(v, "xyzzy");
        assert!(v.contains('.'), "Expected version like '1.78.0', got {v}");
    }

    #[test]
    fn rust_is_installed_true_when_rustup_on_path() {
        if which::which("rustup").is_err() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        assert!(
            RustModule.is_installed(&pm, &dep).unwrap(),
            "is_installed must be true when rustup is on PATH"
        );
    }

    #[test]
    fn rust_is_installed_false_when_neither_on_path() {
        // Skip if either rustup or cargo is available (realistic dev environment).
        if which::which("rustup").is_ok() || which::which("cargo").is_ok() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        assert!(
            !RustModule.is_installed(&pm, &dep).unwrap(),
            "is_installed must be false when neither rustup nor cargo is on PATH"
        );
    }

    #[test]
    fn rust_resolved_version_is_none_when_rustc_absent() {
        if which::which("rustc").is_ok() {
            return;
        }
        if which::which("cargo").is_ok() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        let ver = RustModule.resolved_version(&pm, &dep).unwrap();
        assert!(
            ver.is_none(),
            "Expected None version when rustc is not installed"
        );
    }

    #[test]
    fn rust_install_fails_for_invalid_toolchain() {
        // When rustup IS on PATH, calling install with a bad toolchain causes rustup to fail.
        // This kills `replace install -> Ok(())` because the mutation returns Ok((()) unconditionally.
        if which::which("rustup").is_err() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "toolchain".into(),
            crate::config::ExtraValue::String("invalid-toolchain-devy-test-xyz-12345".into()),
        );
        let dep = Dependency::with_extra("rust", extra);
        let result = RustModule.install(&pm, &dep);
        assert!(
            result.is_err(),
            "install must fail for an invalid toolchain name"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rust_install_rustup_script_failure_propagated() {
        // When rustup is NOT on PATH and the installer script fails, install must return Err.
        // Serves a failing script in place of the pinned rustup installer.
        if which::which("rustup").is_ok() {
            return;
        }
        use crate::installers::test_hooks;
        test_hooks::clear();
        test_hooks::serve(&installers::RUSTUP, b"exit 1\n");
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        let result = RustModule.install(&pm, &dep);
        test_hooks::clear();
        assert!(
            result.is_err(),
            "install must fail when rustup installer exits non-zero"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rust_install_rustup_script_success_does_not_bail() {
        // Kills `delete ! in install` — with mutation, `if status.success()` bails on success.
        // We inject a script that exits 0 (success) and then verify the function
        // proceeds past the bail point (it may still fail at `rustup toolchain install`
        // if rustup is not available, but it must NOT bail with "rustup installation failed").
        if which::which("rustup").is_ok() {
            return;
        }
        use crate::installers::{Interpreter, test_hooks};
        test_hooks::clear();
        test_hooks::serve(&installers::RUSTUP, b"exit 0\n");
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        let result = RustModule.install(&pm, &dep);
        let runs = test_hooks::runs();
        test_hooks::clear();
        assert_eq!(
            runs,
            vec![(
                installers::RUSTUP.name,
                Interpreter::Sh,
                vec!["-y".into(), "--no-modify-path".into()]
            )]
        );
        // With the `!` deleted, a successful script causes bail! — the error message
        // is "rustup installation failed". A correct implementation proceeds past the
        // bail and fails at the `rustup toolchain install` step (different error).
        if let Err(ref e) = result {
            assert!(
                !e.to_string().contains("rustup installation failed"),
                "install must not bail when the installer script succeeds; got: {e}"
            );
        }
    }

    #[test]
    fn rust_resolved_version_contains_dot_when_rustc_installed() {
        if which::which("rustc").is_err() {
            return;
        }
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        let ver = RustModule.resolved_version(&pm, &dep).unwrap();
        if let Some(v) = ver {
            assert!(
                v.contains('.'),
                "Expected semver-like version like '1.78.0', got: {v}"
            );
            assert_ne!(v, "xyzzy", "Version must not be placeholder");
            assert!(!v.is_empty());
        }
    }

    fn file_mtime_secs(path: &std::path::Path) -> u64 {
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn rust_post_setup_no_cargo_toml_is_noop() {
        let dir = crate::test_support::tmp_dir();
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            RustModule
                .post_setup(&Dependency::simple("rust"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn rust_post_setup_skips_cargo_fetch_when_stamp_matches() {
        let dir = crate::test_support::tmp_dir();
        let cargo_toml = dir.join("Cargo.toml");
        std::fs::write(&cargo_toml, "[package]\nname = \"test\"").unwrap();
        std::fs::write(
            dir.join(".devy_rust_stamp"),
            file_mtime_secs(&cargo_toml).to_string(),
        )
        .unwrap();
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            RustModule
                .post_setup(&Dependency::simple("rust"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn rust_post_setup_prefers_cargo_lock_as_stamp_target() {
        // When Cargo.lock exists, it should be used as the stamp target.
        // Verify by writing a stamp matching Cargo.lock mtime — cargo would be
        // invoked (and likely fail) if the stamp check used Cargo.toml instead.
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        let lock = dir.join("Cargo.lock");
        std::fs::write(&lock, "").unwrap();
        std::fs::write(
            dir.join(".devy_rust_stamp"),
            file_mtime_secs(&lock).to_string(),
        )
        .unwrap();
        let pm = crate::package_manager::MockPackageManager::default();
        assert!(
            RustModule
                .post_setup(&Dependency::simple("rust"), &pm, &dir)
                .is_ok()
        );
    }

    #[test]
    fn setup_steps_list_repository_toolchain_and_cargo_config() {
        let dir = crate::test_support::tmp_dir();
        assert!(
            RustModule
                .setup_steps(&Dependency::simple("rust"), &dir)
                .is_empty()
        );
        std::fs::write(
            dir.join("rust-toolchain.toml"),
            "[toolchain]\npath = \"./tc\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.join(".cargo")).unwrap();
        std::fs::write(
            dir.join(".cargo/config.toml"),
            "[build]\nrustc-wrapper = \"x\"\n",
        )
        .unwrap();
        let steps = RustModule.setup_steps(&Dependency::simple("rust"), &dir);
        assert_eq!(steps.len(), 1, "{steps:?}");
        assert!(steps[0].contains("rust-toolchain.toml"), "{steps:?}");
        assert!(steps[0].contains(".cargo/config.toml"), "{steps:?}");
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("rust-toolchain"), "stable\n").unwrap();
        std::fs::write(dir.join(".cargo/config"), "").unwrap();
        let steps = RustModule.setup_steps(&Dependency::simple("rust"), &dir);
        assert_eq!(steps[0], "cargo fetch (Cargo.toml)");
        // Files in parent directories count too (cargo and rustup walk up), but the walk
        // stops at the user's home.
        let sub = dir.join("app");
        std::fs::create_dir(&sub).unwrap();
        let found = repo_toolchain_files(&sub, Some(&dir));
        assert!(found.is_empty(), "{found:?}");
        // Hermetic: stop above `dir` rather than walking the real temp dir's parents.
        let found = repo_toolchain_files(&sub, dir.parent());
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(
            found.contains(&"../rust-toolchain.toml".to_string()),
            "{found:?}"
        );
        assert!(
            found.contains(&"../.cargo/config.toml".to_string()),
            "{found:?}"
        );
        assert!(
            steps[1].ends_with(
                "(rust-toolchain, rust-toolchain.toml, .cargo/config, .cargo/config.toml)"
            ),
            "{steps:?}"
        );
    }

    #[test]
    fn project_at_the_stop_dir_still_lists_its_own_files() {
        // A parent with its own files: a project at the stop dir never walks up to them,
        // so the result does not depend on what lies above the temp dir either.
        let parent = crate::test_support::tmp_dir();
        std::fs::create_dir_all(parent.join(".cargo")).unwrap();
        std::fs::write(parent.join(".cargo/config.toml"), "").unwrap();
        let dir = parent.join("home");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("rust-toolchain.toml"), "").unwrap();
        assert_eq!(
            repo_toolchain_files(&dir, Some(&dir)),
            ["rust-toolchain.toml"]
        );
        // Without a stop the parent's file is listed.
        assert!(repo_toolchain_files(&dir, None).contains(&"../.cargo/config.toml".to_string()));
    }

    #[test]
    fn rustc_and_cargo_sit_next_to_rustup() {
        let exe = std::env::consts::EXE_SUFFIX;
        let rustup = format!("/h/.cargo/bin/rustup{exe}");
        assert_eq!(
            sibling_of_rustup(&rustup, "cargo"),
            Some(format!("/h/.cargo/bin/cargo{exe}"))
        );
        assert_eq!(sibling_of_rustup("/h/bin/other", "cargo"), None);
        if cfg!(windows) {
            // Without the suffix, `rustup.exe` would never match.
            assert_eq!(
                sibling_of_rustup("C:/h/.cargo/bin/rustup.exe", "rustc").as_deref(),
                Some("C:/h/.cargo/bin/rustc.exe")
            );
        }
    }

    /// A `rustc` planted in the project's PATH entries is never the one asked for a
    /// version: the lookup skips it and the rustup fallback is used.
    #[cfg(unix)]
    #[test]
    fn project_local_rustc_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_support::tmp_dir();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let planted = bin.join("rustc");
        std::fs::write(&planted, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        let found = crate::fs_safe::which_outside_project_in("rustc", &path, Some(&root));
        assert_eq!(found, None);
        assert_eq!(
            rustc_program_from(found.clone(), || Some("/home/u/.cargo/bin/rustup".into()))
                .as_deref(),
            Some("/home/u/.cargo/bin/rustc")
        );
        assert_eq!(
            rustc_program_from(Some("/usr/bin/rustc".into()), || panic!("not consulted"))
                .as_deref(),
            Some("/usr/bin/rustc")
        );
        // With neither, nothing is run (never a bare `rustc` from the full PATH).
        assert_eq!(rustc_program_from(found, || None), None);
    }

    #[test]
    fn rust_resolved_version_prefers_which_over_home_path() {
        // If rustc is on PATH, it is used and the rustup fallback is never consulted.
        // Kills `replace which -> Err` — mutation would skip which() and use the bogus
        // fallback. The fallback is injected rather than pointing $HOME elsewhere, which
        // every concurrently running test would see.
        let Some(on_path) = crate::fs_safe::which_outside_project("rustc") else {
            return;
        };
        let program = rustc_program(|| Some("/nonexistent-bogus-home-devy-test/rustup".into()));
        assert_eq!(program.as_deref(), Some(&*on_path.to_string_lossy()));
        let pm = crate::package_manager::MockPackageManager::default();
        let dep = Dependency::simple("rust");
        let result = RustModule.resolved_version(&pm, &dep);
        assert!(
            result.is_ok(),
            "resolved_version must succeed when rustc is on PATH"
        );
        assert!(
            result.unwrap().is_some(),
            "resolved_version must return Some version when rustc is on PATH"
        );
    }
}
