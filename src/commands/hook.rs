use anyhow::{Result, bail};

const BINARY: &str = env!("CARGO_PKG_NAME");

fn make_snippet(template: &str) -> String {
    template.replace("{bin}", BINARY)
}

pub fn run(shell: &str) -> Result<()> {
    let snippet = match shell {
        "zsh" => make_snippet(ZSH_SNIPPET_TEMPLATE),
        "bash" => make_snippet(BASH_SNIPPET_TEMPLATE),
        "fish" => make_snippet(FISH_SNIPPET_TEMPLATE),
        other => bail!(
            "Unsupported shell '{}'. Supported shells: zsh, bash, fish",
            other
        ),
    };
    print!("{}", snippet);
    Ok(())
}

// Each snippet defines:
//   1. A `{bin}` shell function that intercepts `{bin} up` to activate shadowenv
//      in the current shell session after installation.
//   2. A completion function/block that provides tab-completion for all built-in
//      subcommands and dynamically completes user-defined commands from {bin}.yml
//      by calling `command {bin} _commands`.
//
// MAINTENANCE: All three snippets (ZSH, BASH, FISH) must be kept in sync.
// When adding a new subcommand, update all three constants AND the test lists in
// `all_builtin_subcommands_appear_in_*_snippet` below.

const ZSH_SNIPPET_TEMPLATE: &str = r#"
{bin}() {
  if [ "$1" = "up" ]; then
    command {bin} "$@" && eval "$(shadowenv hook zsh)"
  elif [ "$1" = "hook" ]; then
    command {bin} hook zsh
  else
    command {bin} "$@"
  fi
}

_{bin}() {
  local -a subcmds
  subcmds=(
    'up:Set up the development environment'
    'down:Stop all services'
    'services:List services and their status'
    'start:Start a named service'
    'stop:Stop a named service'
    'restart:Restart a named service'
    'status:Show install and environment status'
    'check:Validate the environment without making changes'
    'init:Create an empty {bin}.yml'
    'hook:Print shell integration snippet'
    'pr:Open a GitHub pull request for the current branch'
    'export:Export the environment as a Nix shell.nix or flake.nix'
  )
  local user_cmd
  while IFS= read -r user_cmd; do
    [[ -n "$user_cmd" ]] && subcmds+=("$user_cmd")
  done < <(command {bin} _commands 2>/dev/null)

  if (( CURRENT == 2 )); then
    _describe 'command' subcmds
    return
  fi

  case "${words[2]}" in
    up)
      _arguments \
        '--update[Re-resolve all versions and rewrite {bin}.lock]' \
        '--dry-run[Check status without making changes]' \
        '--bootstrap[Install the package manager if missing]'
      ;;
    start|stop|restart)
      _arguments '1:service name'
      ;;
    init)
      _arguments '--force[Overwrite an existing {bin}.yml]'
      ;;
    hook)
      _values 'shell' zsh bash fish
      ;;
    export)
      _arguments '--format[Output format]:format:(shell flake)'
      ;;
  esac
}

compdef _{bin} {bin}
"#;

const BASH_SNIPPET_TEMPLATE: &str = r#"
{bin}() {
  if [ "$1" = "up" ]; then
    command {bin} "$@" && eval "$(shadowenv hook bash)"
  elif [ "$1" = "hook" ]; then
    command {bin} hook bash
  else
    command {bin} "$@"
  fi
}

_{bin}_completions() {
  local cur="${COMP_WORDS[COMP_CWORD]}"
  local subcmds="up down services start stop restart status check init hook pr export"
  local user_cmds
  user_cmds=$(command {bin} _commands 2>/dev/null)
  [ -n "$user_cmds" ] && subcmds="$subcmds $user_cmds"

  if [ "$COMP_CWORD" -eq 1 ]; then
    COMPREPLY=($(compgen -W "$subcmds" -- "$cur"))
    return
  fi

  case "${COMP_WORDS[1]}" in
    up)
      COMPREPLY=($(compgen -W "--update --dry-run --bootstrap" -- "$cur"))
      ;;
    init)
      COMPREPLY=($(compgen -W "--force" -- "$cur"))
      ;;
    hook)
      COMPREPLY=($(compgen -W "zsh bash fish" -- "$cur"))
      ;;
    export)
      if [ "${COMP_WORDS[COMP_CWORD-1]}" = "--format" ]; then
        COMPREPLY=($(compgen -W "shell flake" -- "$cur"))
      else
        COMPREPLY=($(compgen -W "--format" -- "$cur"))
      fi
      ;;
  esac
}

complete -F _{bin}_completions {bin}
"#;

const FISH_SNIPPET_TEMPLATE: &str = r#"
function {bin}
  if test "$argv[1]" = "up"
    command {bin} $argv; and shadowenv hook fish | source
  else if test "$argv[1]" = "hook"
    command {bin} hook fish
  else
    command {bin} $argv
  end
end

function __{bin}_user_commands
  command {bin} _commands 2>/dev/null
end

function __{bin}_no_subcommand
  not __fish_seen_subcommand_from up down services start stop restart status check init hook pr export
end

complete -c {bin} -f
complete -c {bin} -n __{bin}_no_subcommand -a up       -d "Set up the development environment"
complete -c {bin} -n __{bin}_no_subcommand -a down     -d "Stop all services"
complete -c {bin} -n __{bin}_no_subcommand -a services -d "List services and their status"
complete -c {bin} -n __{bin}_no_subcommand -a start    -d "Start a named service"
complete -c {bin} -n __{bin}_no_subcommand -a stop     -d "Stop a named service"
complete -c {bin} -n __{bin}_no_subcommand -a restart  -d "Restart a named service"
complete -c {bin} -n __{bin}_no_subcommand -a status   -d "Show install and environment status"
complete -c {bin} -n __{bin}_no_subcommand -a check    -d "Validate the environment"
complete -c {bin} -n __{bin}_no_subcommand -a init     -d "Scaffold a {bin}.yml"
complete -c {bin} -n __{bin}_no_subcommand -a hook     -d "Print shell integration snippet"
complete -c {bin} -n __{bin}_no_subcommand -a pr       -d "Open a GitHub pull request"
complete -c {bin} -n __{bin}_no_subcommand -a export   -d "Export a Nix shell.nix or flake.nix"
complete -c {bin} -n __{bin}_no_subcommand -a "(__{bin}_user_commands)" -d "User-defined command"
complete -c {bin} -n "__fish_seen_subcommand_from hook" -a "zsh bash fish"
complete -c {bin} -n "__fish_seen_subcommand_from up" -l update  -d "Re-resolve all versions"
complete -c {bin} -n "__fish_seen_subcommand_from up" -l dry-run -d "Check without making changes"
complete -c {bin} -n "__fish_seen_subcommand_from up" -l bootstrap -d "Install the package manager if missing"
complete -c {bin} -n "__fish_seen_subcommand_from export" -l format -x -a "shell flake" -d "Output format"
complete -c {bin} -n "__fish_seen_subcommand_from init" -l force -d "Overwrite existing {bin}.yml"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn zsh_snippet() -> String {
        make_snippet(ZSH_SNIPPET_TEMPLATE)
    }
    fn bash_snippet() -> String {
        make_snippet(BASH_SNIPPET_TEMPLATE)
    }
    fn fish_snippet() -> String {
        make_snippet(FISH_SNIPPET_TEMPLATE)
    }

    #[test]
    fn run_zsh_succeeds() {
        assert!(run("zsh").is_ok());
    }

    #[test]
    fn run_bash_succeeds() {
        assert!(run("bash").is_ok());
    }

    #[test]
    fn run_fish_succeeds() {
        assert!(run("fish").is_ok());
    }

    #[test]
    fn run_unsupported_shell_returns_error() {
        let err = run("powershell").unwrap_err();
        assert!(err.to_string().contains("Unsupported shell"));
        assert!(err.to_string().contains("powershell"));
    }

    #[test]
    fn zsh_snippet_contains_shadowenv_hook() {
        assert!(zsh_snippet().contains("shadowenv hook zsh"));
    }

    #[test]
    fn bash_snippet_contains_shadowenv_hook() {
        assert!(bash_snippet().contains("shadowenv hook bash"));
    }

    #[test]
    fn fish_snippet_contains_shadowenv_hook() {
        assert!(fish_snippet().contains("shadowenv hook fish"));
    }

    #[test]
    fn zsh_snippet_contains_completion_function() {
        let s = zsh_snippet();
        assert!(s.contains(&format!("_{}()", BINARY)));
        assert!(s.contains(&format!("compdef _{bin} {bin}", bin = BINARY)));
        assert!(s.contains("_commands"));
    }

    #[test]
    fn bash_snippet_contains_completion_function() {
        let s = bash_snippet();
        assert!(s.contains(&format!("_{}_completions", BINARY)));
        assert!(s.contains(&format!(
            "complete -F _{bin}_completions {bin}",
            bin = BINARY
        )));
        assert!(s.contains("_commands"));
    }

    #[test]
    fn fish_snippet_contains_completion_directives() {
        let s = fish_snippet();
        assert!(s.contains(&format!("complete -c {}", BINARY)));
        assert!(s.contains(&format!("__{}_user_commands", BINARY)));
        assert!(s.contains("_commands"));
    }

    #[test]
    fn all_builtin_subcommands_appear_in_zsh_snippet() {
        let s = zsh_snippet();
        for cmd in &[
            "up", "down", "services", "start", "stop", "restart", "status", "check", "init",
            "hook", "pr", "export",
        ] {
            assert!(s.contains(cmd), "zsh snippet missing '{}'", cmd);
        }
    }

    #[test]
    fn all_builtin_subcommands_appear_in_bash_snippet() {
        let s = bash_snippet();
        for cmd in &[
            "up", "down", "services", "start", "stop", "restart", "status", "check", "init",
            "hook", "pr", "export",
        ] {
            assert!(s.contains(cmd), "bash snippet missing '{}'", cmd);
        }
    }

    #[test]
    fn all_builtin_subcommands_appear_in_fish_snippet() {
        let s = fish_snippet();
        for cmd in &[
            "up", "down", "services", "start", "stop", "restart", "status", "check", "init",
            "hook", "pr", "export",
        ] {
            assert!(s.contains(cmd), "fish snippet missing '{}'", cmd);
        }
    }

    #[test]
    fn snippets_contain_binary_name() {
        // Verifies that make_snippet substituted {bin} with the actual binary name.
        // If CARGO_PKG_NAME changes, this test catches snippets that forgot to use the template.
        let bin = env!("CARGO_PKG_NAME");
        assert!(
            zsh_snippet().contains(bin),
            "zsh snippet must contain the binary name"
        );
        assert!(
            bash_snippet().contains(bin),
            "bash snippet must contain the binary name"
        );
        assert!(
            fish_snippet().contains(bin),
            "fish snippet must contain the binary name"
        );
    }

    #[test]
    fn snippet_templates_do_not_contain_literal_placeholder() {
        // After substitution, no {bin} placeholder should remain.
        assert!(!zsh_snippet().contains("{bin}"));
        assert!(!bash_snippet().contains("{bin}"));
        assert!(!fish_snippet().contains("{bin}"));
    }

    #[test]
    fn snippets_complete_new_flags_and_values() {
        for (shell, s) in [
            ("zsh", zsh_snippet()),
            ("bash", bash_snippet()),
            ("fish", fish_snippet()),
        ] {
            for needle in ["pr", "export", "bootstrap", "format", "shell flake"] {
                assert!(s.contains(needle), "{shell} snippet missing '{needle}'");
            }
        }
    }
}
