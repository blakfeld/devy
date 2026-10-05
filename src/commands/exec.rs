use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{DevyCommand, DevyConfig, HookAction};
use crate::output;

const ALLOWED_SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "cmd", "powershell"];

/// Returns the flag used to pass a command string to the given shell.
/// `cmd.exe` uses `/c`; all POSIX shells and PowerShell accept `-c`.
fn shell_flag(shell: &str) -> &'static str {
    match shell {
        "cmd" => "/c",
        _ => "-c",
    }
}

pub(crate) fn validate_shell(shell: &str) -> Result<()> {
    if shell.contains(['/', '\\']) {
        bail!(
            "shell '{}' must be a bare name, not a path; permitted shells: {}",
            shell,
            ALLOWED_SHELLS.join(", ")
        );
    }
    if !ALLOWED_SHELLS.contains(&shell) {
        bail!(
            "shell '{}' is not in the allowed list; permitted shells: {}",
            shell,
            ALLOWED_SHELLS.join(", ")
        );
    }
    Ok(())
}

pub(crate) fn spawn_cmd(cmd: &DevyCommand, label: &str) -> Result<()> {
    validate_shell(&cmd.shell).with_context(|| format!("'{}': invalid shell", label))?;
    let mut proc = Command::new(&cmd.shell);
    proc.arg(shell_flag(&cmd.shell)).arg(&cmd.cmd);
    if let Some(cwd) = &cmd.cwd {
        proc.current_dir(cwd);
    }
    let status = proc
        .status()
        .with_context(|| format!("Failed to spawn '{}' via {}", label, cmd.shell))?;
    if !status.success() {
        bail!(
            "'{}' command {:?} failed with exit status {} — check the output above for details",
            label,
            cmd.cmd,
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

/// Resolves a `cwd` from devy.yml against the project root. It must be relative and
/// must stay inside the root, both lexically and after following symlinks, so a
/// committed symlink cannot point a command or hook outside the project.
pub(crate) fn resolve_cwd(cwd: &str, project_root: &Path) -> Result<PathBuf> {
    crate::validate::require(crate::validate::rel_path_inside(cwd), "cwd", "cwd", cwd)?;
    let root = project_root
        .canonicalize()
        .with_context(|| format!("Failed to resolve project root {}", project_root.display()))?;
    let dir = root
        .join(cwd)
        .canonicalize()
        .with_context(|| format!("Failed to resolve cwd {cwd:?}"))?;
    if !dir.starts_with(&root) {
        bail!("cwd {cwd:?} resolves outside the project root");
    }
    // Run in the checked, canonical path so a symlink swapped in after the check cannot
    // move the process. On Windows `canonicalize` returns a verbatim `\\?\C:\...` path,
    // which `cmd.exe` replaces with C:\Windows, so the prefix is dropped; Win32 then
    // normalizes the path, so it is used only if it still names the checked directory.
    let run_dir = without_verbatim_prefix(dir.clone());
    if run_dir != dir && run_dir.canonicalize().ok().as_ref() != Some(&dir) {
        bail!("cwd {cwd:?} does not resolve to the same directory without its \\\\?\\ prefix");
    }
    Ok(run_dir)
}

/// `\\?\C:\x` as `C:\x`. Other paths, including verbatim UNC paths, are returned as is.
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path;
    };
    match s.strip_prefix(r"\\?\") {
        Some(rest)
            if rest.as_bytes().get(1) == Some(&b':')
                && rest.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) =>
        {
            PathBuf::from(rest)
        }
        _ => path,
    }
}

/// `cmd` with its `cwd` resolved against the project root (see [`resolve_cwd`]).
fn in_project(cmd: DevyCommand, project_root: &Path) -> Result<DevyCommand> {
    let cwd = match cmd.cwd.as_deref() {
        Some(cwd) => Some(
            resolve_cwd(cwd, project_root)?
                .to_string_lossy()
                .into_owned(),
        ),
        None => None,
    };
    Ok(DevyCommand { cwd, ..cmd })
}

/// Runs all commands in a hook. Aborts with an error if any command exits non-zero.
/// A hook's `cwd` is resolved against `project_root`.
pub fn run_hook(label: &str, action: &HookAction, project_root: &Path) -> Result<()> {
    let cmds = action.commands();
    let total = cmds.len();
    for (i, raw) in cmds.iter().enumerate() {
        let cmd = in_project(DevyCommand::from(raw.clone()), project_root)?;
        let tag = if total == 1 {
            format!("'{label}'")
        } else {
            format!("'{label}' ({}/{})", i + 1, total)
        };
        output::step(&format!("Running hook {tag}"));
        spawn_cmd(&cmd, &tag)?;
        output::success(&format!("Hook {tag} succeeded"));
    }
    Ok(())
}

/// Wraps `s` in POSIX single quotes, escaping embedded single quotes.
/// Safe for sh, bash and zsh. Not safe for fish (see [`fish_quote`]) or cmd/powershell.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Wraps `s` in fish single quotes. Inside them fish treats `\\` and `\'` as escapes,
/// so `\` is escaped before `'`.
pub(crate) fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn append_extra_args(cmd: DevyCommand, extra_args: &[String]) -> DevyCommand {
    if extra_args.is_empty() {
        return cmd;
    }
    if matches!(cmd.shell.as_str(), "cmd" | "powershell") {
        output::warn(&format!(
            "extra args are not supported with shell '{}' — args ignored",
            cmd.shell
        ));
        return cmd;
    }
    let quote: fn(&str) -> String = if cmd.shell == "fish" {
        fish_quote
    } else {
        sh_quote
    };
    let quoted: Vec<String> = extra_args.iter().map(|a| quote(a)).collect();
    DevyCommand {
        cmd: format!("{} {}", cmd.cmd, quoted.join(" ")),
        ..cmd
    }
}

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper — requires a real devy.yml on disk
pub fn run(name: &str, extra_args: &[String]) -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;

    let raw = config.commands.get(name).cloned();

    let cmd = match raw {
        Some(raw) => in_project(DevyCommand::from(raw), &project_root)?,
        None => {
            let mut available: Vec<&str> = config.commands.keys().map(|k| k.as_str()).collect();
            available.sort_unstable();
            if available.is_empty() {
                bail!("Unknown command '{}'. No commands are defined.", name);
            } else {
                bail!(
                    "Unknown command '{}'. Available: {}",
                    name,
                    available.join(", ")
                );
            }
        }
    };

    let cmd = append_extra_args(cmd, extra_args);
    output::step(&format!("Running '{}'", name));
    spawn_cmd(&cmd, name)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HookAction, RawCommand};

    #[test]
    fn run_hook_with_succeeding_command_returns_ok() {
        let action = HookAction::Single(RawCommand::Simple("true".into()));
        assert!(run_hook("test_hook", &action, Path::new(".")).is_ok());
    }

    #[test]
    fn run_hook_with_failing_command_returns_err() {
        let action = HookAction::Single(RawCommand::Simple("false".into()));
        let err = run_hook("test_hook", &action, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("test_hook"));
        // Note: error no longer says "hook" — just the label name
        assert!(!err.to_string().starts_with("hook"));
    }

    #[test]
    fn run_hook_with_custom_shell_uses_that_shell() {
        let action = HookAction::Single(RawCommand::Configured {
            cmd: "true".into(),
            cwd: None,
            shell: Some("sh".into()),
        });
        assert!(run_hook("test_hook", &action, Path::new(".")).is_ok());
    }

    #[test]
    fn run_hook_list_runs_all_commands() {
        let action = HookAction::List(vec![
            RawCommand::Simple("true".into()),
            RawCommand::Simple("true".into()),
        ]);
        assert!(run_hook("test_hook", &action, Path::new(".")).is_ok());
    }

    #[test]
    fn run_hook_list_stops_on_first_failure() {
        let action = HookAction::List(vec![
            RawCommand::Simple("false".into()),
            RawCommand::Simple("true".into()),
        ]);
        assert!(run_hook("test_hook", &action, Path::new(".")).is_err());
    }

    #[test]
    fn spawn_cmd_with_cwd_sets_working_directory() {
        let dir = crate::test_support::tmp_dir();
        let cmd = DevyCommand {
            // Use a no-op that exits 0 in the platform default shell.
            // sh/bash/zsh/fish: `true`; cmd.exe: `cd` (prints CWD, exits 0).
            cmd: if cfg!(target_os = "windows") {
                "cd"
            } else {
                "true"
            }
            .into(),
            cwd: Some(dir.to_string_lossy().into_owned()),
            shell: crate::config::default_shell(),
        };
        assert!(spawn_cmd(&cmd, "pwd_test").is_ok());
    }

    // ── shell_flag ────────────────────────────────────────────────────────────

    #[test]
    fn shell_flag_cmd_uses_slash_c() {
        assert_eq!(shell_flag("cmd"), "/c");
    }

    #[test]
    fn shell_flag_posix_shells_use_dash_c() {
        for shell in &["sh", "bash", "zsh", "fish", "powershell"] {
            assert_eq!(shell_flag(shell), "-c", "{shell} should use -c");
        }
    }

    // ── validate_shell ────────────────────────────────────────────────────────

    #[test]
    fn validate_shell_accepts_allowed_shells() {
        for shell in &["sh", "bash", "zsh", "fish", "cmd", "powershell"] {
            assert!(validate_shell(shell).is_ok(), "should allow '{shell}'");
        }
    }

    #[test]
    fn validate_shell_rejects_path_to_allowed_shell() {
        // Paths are rejected even when the basename is an allowed shell name,
        // because /tmp/sh is not the same as sh.
        assert!(validate_shell("/bin/sh").is_err());
        assert!(validate_shell("/usr/bin/bash").is_err());
        assert!(validate_shell("/usr/local/bin/zsh").is_err());
    }

    #[test]
    fn validate_shell_rejects_unknown_binary() {
        let err = validate_shell("evil-binary").unwrap_err();
        assert!(err.to_string().contains("evil-binary"));
        assert!(err.to_string().contains("not in the allowed list"));
    }

    #[test]
    fn validate_shell_rejects_absolute_path_to_unknown_binary() {
        let err = validate_shell("/tmp/evil").unwrap_err();
        assert!(err.to_string().contains("not a path"));
    }

    // ── append_extra_args ─────────────────────────────────────────────────────

    fn base_cmd(s: &str) -> DevyCommand {
        DevyCommand {
            cmd: s.into(),
            cwd: None,
            shell: "sh".into(),
        }
    }

    #[test]
    fn append_extra_args_empty_leaves_cmd_unchanged() {
        let cmd = append_extra_args(base_cmd("cargo build"), &[]);
        assert_eq!(cmd.cmd, "cargo build");
    }

    #[test]
    fn append_extra_args_appends_space_separated() {
        let extra = vec!["--release".into(), "--target".into(), "x86_64".into()];
        let cmd = append_extra_args(base_cmd("cargo build"), &extra);
        assert_eq!(cmd.cmd, "cargo build '--release' '--target' 'x86_64'");
    }

    #[test]
    fn append_extra_args_quotes_arg_with_space() {
        let extra = vec!["my arg".into()];
        let cmd = append_extra_args(base_cmd("pytest"), &extra);
        assert_eq!(cmd.cmd, "pytest 'my arg'");
    }

    #[test]
    fn append_extra_args_escapes_single_quote() {
        let extra = vec!["it's".into()];
        let cmd = append_extra_args(base_cmd("echo"), &extra);
        assert_eq!(cmd.cmd, "echo 'it'\\''s'");
    }

    fn fish_cmd(s: &str) -> DevyCommand {
        DevyCommand {
            shell: "fish".into(),
            ..base_cmd(s)
        }
    }

    #[test]
    fn fish_quote_escapes_backslash_before_quote() {
        assert_eq!(fish_quote("plain"), "'plain'");
        assert_eq!(fish_quote("it's"), r"'it\'s'");
        assert_eq!(fish_quote(r"dir\"), r"'dir\\'");
        assert_eq!(fish_quote(r"a\'b"), r"'a\\\'b'");
        assert_eq!(fish_quote("$HOME (x)"), "'$HOME (x)'");
    }

    #[test]
    fn append_extra_args_fish_arg_ending_in_backslash() {
        // Unescaped, the trailing `\'` would escape the closing quote.
        let cmd = append_extra_args(fish_cmd("echo"), &[r"C:\".into(), "next".into()]);
        assert_eq!(cmd.cmd, r"echo 'C:\\' 'next'");
    }

    #[test]
    fn append_extra_args_fish_arg_with_single_quote() {
        let cmd = append_extra_args(fish_cmd("echo"), &["it's".into()]);
        assert_eq!(cmd.cmd, r"echo 'it\'s'");
    }

    #[cfg(unix)]
    #[test]
    fn append_extra_args_fish_round_trips_when_fish_installed() {
        if which::which("fish").is_err() {
            eprintln!("skipping: fish not on PATH");
            return;
        }
        let args = [r"end\", "it's", r"a\'b; echo pwned", "$x"];
        let extra: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let cmd = append_extra_args(fish_cmd(r"printf '%s\n'"), &extra);
        let out = Command::new("fish")
            .arg("-c")
            .arg(&cmd.cmd)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let expected: String = args.iter().map(|a| format!("{a}\n")).collect();
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
    }

    #[test]
    fn append_extra_args_preserves_shell_and_cwd() {
        let input = DevyCommand {
            cmd: "make".into(),
            cwd: Some("/tmp".into()),
            shell: "bash".into(),
        };
        let cmd = append_extra_args(input, &["all".into()]);
        assert_eq!(cmd.shell, "bash");
        assert_eq!(cmd.cwd, Some("/tmp".into()));
    }

    #[test]
    fn append_extra_args_cmd_shell_returns_unchanged() {
        let input = DevyCommand {
            cmd: "dir".into(),
            cwd: None,
            shell: "cmd".into(),
        };
        let cmd = append_extra_args(input, &["/w".into()]);
        assert_eq!(cmd.cmd, "dir", "cmd shell must ignore extra args");
    }

    #[test]
    fn append_extra_args_powershell_returns_unchanged() {
        let input = DevyCommand {
            cmd: "Get-ChildItem".into(),
            cwd: None,
            shell: "powershell".into(),
        };
        let cmd = append_extra_args(input, &["-Path".into(), "C:\\".into()]);
        assert_eq!(
            cmd.cmd, "Get-ChildItem",
            "powershell shell must ignore extra args"
        );
    }

    #[test]
    fn spawn_cmd_rejects_disallowed_shell() {
        let cmd = DevyCommand {
            cmd: "true".into(),
            cwd: None,
            shell: "not-a-shell".into(),
        };
        assert!(spawn_cmd(&cmd, "test").is_err());
    }

    // ── resolve_cwd ───────────────────────────────────────────────────────────

    #[test]
    fn resolve_cwd_joins_relative_path_to_project_root() {
        let root = crate::test_support::tmp_dir();
        std::fs::create_dir(root.join("api")).unwrap();
        let expected = without_verbatim_prefix(root.canonicalize().unwrap().join("api"));
        assert_eq!(resolve_cwd("api", &root).unwrap(), expected);
        assert_eq!(resolve_cwd("./api", &root).unwrap(), expected);
        // A canonicalized root (as `locate_config` returns) gives the same result, and on
        // Windows never a verbatim path.
        let canonical = root.canonicalize().unwrap();
        let resolved = resolve_cwd("api", &canonical).unwrap();
        assert_eq!(resolved, expected);
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn resolve_cwd_rejects_paths_outside_the_root() {
        let root = crate::test_support::tmp_dir();
        for bad in ["../../", "..", "/tmp/shared"] {
            let err = resolve_cwd(bad, &root).unwrap_err().to_string();
            assert!(err.contains("invalid cwd"), "{bad}: {err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cwd_rejects_symlink_escaping_the_root() {
        let root = crate::test_support::tmp_dir();
        let outside = crate::test_support::tmp_dir();
        std::os::unix::fs::symlink(&*outside, root.join("link")).unwrap();
        let err = resolve_cwd("link", &root).unwrap_err().to_string();
        assert!(err.contains("outside the project root"), "{err}");
    }

    #[test]
    fn run_hook_resolves_cwd_against_project_root() {
        let root = crate::test_support::tmp_dir();
        std::fs::create_dir(root.join("sub")).unwrap();
        let marker = if cfg!(target_os = "windows") {
            "type nul > here"
        } else {
            "touch here"
        };
        let action = HookAction::Single(RawCommand::Configured {
            cmd: marker.into(),
            cwd: Some("sub".into()),
            shell: None,
        });
        run_hook("cwd_hook", &action, &root).unwrap();
        assert!(root.join("sub").join("here").exists());
    }

    #[test]
    fn without_verbatim_prefix_strips_drive_paths_only() {
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\C:\proj\api")),
            PathBuf::from(r"C:\proj\api")
        );
        for kept in [r"\\?\UNC\server\share\x", "/tmp/proj/api", r"C:\proj"] {
            assert_eq!(
                without_verbatim_prefix(PathBuf::from(kept)),
                PathBuf::from(kept)
            );
        }
    }
}
