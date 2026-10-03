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
    'doctor:Diagnose the environment and the last failed up'
    'logs:Show recent service log output'
    'ask:Ask Claude about this environment'
    'init:Create an empty {bin}.yml'
    'hook:Print shell integration snippet'
    'pr:Open a GitHub pull request for the current branch'
    'export:Export the environment as a Nix shell.nix or flake.nix'
    'exec:Run a program with the project environment'
    'agent-setup:Write the coding agent skill'
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
    down)
      _arguments '--volumes[Also remove docker-managed containers and volumes]'
      ;;
    doctor)
      _arguments \
        '--yes[Apply a suggested {bin}.yml fix without asking]' \
        '--no-ai[Skip the AI diagnosis]' \
        '--show-context[Print the AI request without sending it]'
      ;;
    start|stop|restart)
      _arguments '1:service name'
      ;;
    logs)
      local -a services
      services=(${(f)"$(command {bin} _services 2>/dev/null)"})
      _arguments \
        '(-f --follow)'{-f,--follow}'[Stream new log output]' \
        '(-n --lines)'{-n,--lines}'[Lines to show per service]:lines:' \
        '--explain[Explain the logs with Claude]' \
        '--show-context[Print the AI request without sending it]' \
        '1:service:compadd -a services'
      ;;
    ask)
      _arguments \
        '--show-context[Print the AI request without sending it]' \
        '1:question:'
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
    status|services|check)
      _arguments '--json[Print one JSON document]'
      ;;
    exec)
      # The program and its arguments, after an optional `--`.
      local skip=2
      (( CURRENT > 3 )) && [[ ${words[3]} == -- ]] && skip=3
      shift $skip words
      (( CURRENT -= skip ))
      _normal
      ;;
    agent-setup)
      _arguments \
        '--force[Overwrite a skill {bin} did not write]' \
        '--agents-md[Create AGENTS.md if missing]' \
        '--print[Print the skill without writing]'
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
  local subcmds="up down services start stop restart status check doctor logs ask init hook pr export exec agent-setup"
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
    down)
      COMPREPLY=($(compgen -W "--volumes" -- "$cur"))
      ;;
    doctor)
      COMPREPLY=($(compgen -W "--yes --no-ai --show-context" -- "$cur"))
      ;;
    logs)
      case "${COMP_WORDS[COMP_CWORD-1]}" in
        -n|--lines) return ;;
      esac
      COMPREPLY=($(compgen -W "--follow --lines --explain --show-context $(command {bin} _services 2>/dev/null)" -- "$cur"))
      ;;
    ask)
      COMPREPLY=($(compgen -W "--show-context" -- "$cur"))
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
    status|services|check)
      COMPREPLY=($(compgen -W "--json" -- "$cur"))
      ;;
    exec)
      # The program name, then its arguments as files.
      if [ "$COMP_CWORD" -eq 2 ] || { [ "$COMP_CWORD" -eq 3 ] && [ "${COMP_WORDS[2]}" = "--" ]; }; then
        COMPREPLY=($(compgen -c -- "$cur"))
      else
        COMPREPLY=($(compgen -f -- "$cur"))
      fi
      ;;
    agent-setup)
      COMPREPLY=($(compgen -W "--force --agents-md --print" -- "$cur"))
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

function __{bin}_complete_exec
  # The program and its arguments, after an optional `--`.
  set -l tokens (commandline -opc)
  if test (count $tokens) -ge 3; and test "$tokens[3]" = "--"
    __fish_complete_subcommand --fcs-skip=3
  else
    __fish_complete_subcommand --fcs-skip=2
  end
end

function __{bin}_no_subcommand
  not __fish_seen_subcommand_from up down services start stop restart status check doctor logs ask init hook pr export exec agent-setup
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
complete -c {bin} -n __{bin}_no_subcommand -a doctor   -d "Diagnose the environment and the last failed up"
complete -c {bin} -n __{bin}_no_subcommand -a logs     -d "Show recent service log output"
complete -c {bin} -n __{bin}_no_subcommand -a ask      -d "Ask Claude about this environment"
complete -c {bin} -n __{bin}_no_subcommand -a init     -d "Scaffold a {bin}.yml"
complete -c {bin} -n __{bin}_no_subcommand -a hook     -d "Print shell integration snippet"
complete -c {bin} -n __{bin}_no_subcommand -a pr       -d "Open a GitHub pull request"
complete -c {bin} -n __{bin}_no_subcommand -a export   -d "Export a Nix shell.nix or flake.nix"
complete -c {bin} -n __{bin}_no_subcommand -a exec     -d "Run a program with the project environment"
complete -c {bin} -n __{bin}_no_subcommand -a agent-setup -d "Write the coding agent skill"
complete -c {bin} -n __{bin}_no_subcommand -a "(__{bin}_user_commands)" -d "User-defined command"
complete -c {bin} -n "__fish_seen_subcommand_from hook" -a "zsh bash fish"
complete -c {bin} -n "__fish_seen_subcommand_from up" -l update  -d "Re-resolve all versions"
complete -c {bin} -n "__fish_seen_subcommand_from up" -l dry-run -d "Check without making changes"
complete -c {bin} -n "__fish_seen_subcommand_from up" -l bootstrap -d "Install the package manager if missing"
complete -c {bin} -n "__fish_seen_subcommand_from down" -l volumes -d "Also remove docker containers and volumes"
complete -c {bin} -n "__fish_seen_subcommand_from doctor" -l yes -d "Apply a suggested {bin}.yml fix without asking"
complete -c {bin} -n "__fish_seen_subcommand_from doctor" -l no-ai -d "Skip the AI diagnosis"
complete -c {bin} -n "__fish_seen_subcommand_from doctor" -l show-context -d "Print the AI request without sending it"
complete -c {bin} -n "__fish_seen_subcommand_from logs" -a "(command {bin} _services 2>/dev/null)" -d "Service"
complete -c {bin} -n "__fish_seen_subcommand_from logs" -s f -l follow -d "Stream new log output"
complete -c {bin} -n "__fish_seen_subcommand_from logs" -s n -l lines -x -d "Lines to show per service"
complete -c {bin} -n "__fish_seen_subcommand_from logs" -l explain -d "Explain the logs with Claude"
complete -c {bin} -n "__fish_seen_subcommand_from logs" -l show-context -d "Print the AI request without sending it"
complete -c {bin} -n "__fish_seen_subcommand_from ask" -l show-context -d "Print the AI request without sending it"
complete -c {bin} -n "__fish_seen_subcommand_from export" -l format -x -a "shell flake" -d "Output format"
complete -c {bin} -n "__fish_seen_subcommand_from init" -l force -d "Overwrite existing {bin}.yml"
complete -c {bin} -n "__fish_seen_subcommand_from status services check" -l json -d "Print one JSON document"
complete -c {bin} -n "__fish_seen_subcommand_from exec" -a "(__{bin}_complete_exec)"
complete -c {bin} -n "__fish_seen_subcommand_from agent-setup" -l force -d "Overwrite a skill {bin} did not write"
complete -c {bin} -n "__fish_seen_subcommand_from agent-setup" -l agents-md -d "Create AGENTS.md if missing"
complete -c {bin} -n "__fish_seen_subcommand_from agent-setup" -l print -d "Print the skill without writing"
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

    /// The subcommands `devy --help` lists: every built-in except the hidden helpers.
    fn visible_subcommands() -> Vec<String> {
        let names: Vec<String> = crate::cli::builtin_subcommands()
            .into_iter()
            .filter(|c| !c.starts_with('_') && c != "help")
            .collect();
        assert!(names.contains(&"agent-setup".to_string()), "{names:?}");
        names
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
        for cmd in visible_subcommands() {
            assert!(
                s.contains(&format!("    '{cmd}:")),
                "zsh snippet missing '{cmd}'"
            );
        }
    }

    #[test]
    fn all_builtin_subcommands_appear_in_bash_snippet() {
        let s = bash_snippet();
        let subcmds: Vec<&str> = s
            .lines()
            .find_map(|l| l.trim().strip_prefix("local subcmds=\""))
            .and_then(|l| l.strip_suffix('"'))
            .expect("subcmds list")
            .split(' ')
            .collect();
        for cmd in visible_subcommands() {
            assert!(
                subcmds.contains(&cmd.as_str()),
                "bash snippet missing '{cmd}'"
            );
        }
    }

    #[test]
    fn all_builtin_subcommands_appear_in_fish_snippet() {
        let s = fish_snippet();
        for cmd in visible_subcommands() {
            assert!(
                s.contains(&format!("-a {cmd} ")),
                "fish snippet missing '{cmd}'"
            );
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

    #[test]
    fn snippets_complete_down_volumes() {
        assert!(zsh_snippet().contains("    down)\n      _arguments '--volumes["));
        assert!(bash_snippet().contains("    down)\n      COMPREPLY=($(compgen -W \"--volumes\""));
        assert!(fish_snippet().contains("-n \"__fish_seen_subcommand_from down\" -l volumes"));
    }

    #[test]
    fn snippets_complete_logs_and_ask() {
        let zsh = zsh_snippet();
        for needle in [
            "'logs:",
            "'ask:",
            "    logs)\n",
            "{-f,--follow}'[",
            "{-n,--lines}'[",
            "'--explain[",
            "_services 2>/dev/null",
            "    ask)\n      _arguments \\\n        '--show-context[",
        ] {
            assert!(zsh.contains(needle), "zsh missing {needle:?}");
        }
        let bash = bash_snippet();
        assert!(bash.contains("doctor logs ask init"), "{bash}");
        assert!(bash.contains(
            "compgen -W \"--follow --lines --explain --show-context $(command devy _services 2>/dev/null)\""
        ));
        assert!(bash.contains("    ask)\n      COMPREPLY=($(compgen -W \"--show-context\""));
        let fish = fish_snippet();
        for needle in [
            "-a logs ",
            "-a ask ",
            "-n \"__fish_seen_subcommand_from logs\" -a \"(command devy _services 2>/dev/null)\"",
            "-n \"__fish_seen_subcommand_from logs\" -s f -l follow ",
            "-n \"__fish_seen_subcommand_from logs\" -s n -l lines ",
            "-n \"__fish_seen_subcommand_from logs\" -l explain ",
            "-n \"__fish_seen_subcommand_from logs\" -l show-context ",
            "-n \"__fish_seen_subcommand_from ask\" -l show-context ",
        ] {
            assert!(fish.contains(needle), "fish missing {needle:?}");
        }
    }

    #[test]
    fn snippets_complete_doctor_flags() {
        let zsh = zsh_snippet();
        assert!(zsh.contains("'doctor:"), "{zsh}");
        assert!(zsh.contains("    doctor)\n      _arguments \\\n        '--yes["));
        for flag in ["'--no-ai[", "'--show-context["] {
            assert!(zsh.contains(flag), "zsh missing {flag}");
        }
        assert!(bash_snippet().contains(
            "    doctor)\n      COMPREPLY=($(compgen -W \"--yes --no-ai --show-context\""
        ));
        let fish = fish_snippet();
        assert!(fish.contains("-a doctor"), "{fish}");
        for flag in ["yes", "no-ai", "show-context"] {
            assert!(
                fish.contains(&format!(
                    "-n \"__fish_seen_subcommand_from doctor\" -l {flag} "
                )),
                "fish missing --{flag}"
            );
        }
    }

    #[test]
    fn snippets_complete_json_exec_and_agent_setup() {
        let zsh = zsh_snippet();
        for needle in [
            "    status|services|check)\n      _arguments '--json[",
            "    exec)\n      # The program and its arguments, after an optional `--`.\n      local skip=2\n      (( CURRENT > 3 )) && [[ ${words[3]} == -- ]] && skip=3\n",
            "      _normal\n",
            "    agent-setup)\n      _arguments \\\n        '--force[",
            "'--agents-md[",
            "'--print[",
        ] {
            assert!(zsh.contains(needle), "zsh missing {needle:?}");
        }
        let bash = bash_snippet();
        for needle in [
            "    status|services|check)\n      COMPREPLY=($(compgen -W \"--json\"",
            "COMPREPLY=($(compgen -c -- \"$cur\"))",
            "    agent-setup)\n      COMPREPLY=($(compgen -W \"--force --agents-md --print\"",
        ] {
            assert!(bash.contains(needle), "bash missing {needle:?}");
        }
        let fish = fish_snippet();
        for needle in [
            "-n \"__fish_seen_subcommand_from status services check\" -l json ",
            "-n \"__fish_seen_subcommand_from exec\" -a \"(__devy_complete_exec)\"",
            "__fish_complete_subcommand --fcs-skip=3",
            "-n \"__fish_seen_subcommand_from agent-setup\" -l force ",
            "-n \"__fish_seen_subcommand_from agent-setup\" -l agents-md ",
            "-n \"__fish_seen_subcommand_from agent-setup\" -l print ",
            "hook pr export exec agent-setup\nend",
        ] {
            assert!(fish.contains(needle), "fish missing {needle:?}");
        }
    }
}
