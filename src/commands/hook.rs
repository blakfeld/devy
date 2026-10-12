use anyhow::{Result, bail};

use super::exec::{fish_quote, sh_quote};

const BINARY: &str = env!("CARGO_PKG_NAME");

/// The snippet for `shell` from `template`, setting shadowenv up by the absolute path
/// `shadowenv` (`None` when it was not found): `{shadowenv_init}` becomes the line that
/// does (see [`shadowenv_init`]), and bash's `{shadowenv_fn}` its `shadowenv` function
/// (see [`bash_shadowenv_fn`]).
fn make_snippet(template: &str, shell: &str, shadowenv: Option<&std::path::Path>) -> String {
    template
        .replace("{shadowenv_fn}", &bash_shadowenv_fn(shadowenv))
        .replace("{bin}", BINARY)
        .replace("{shadowenv_init}", &shadowenv_init(shell, shadowenv))
        .replace("{loader_env}", &loader_env())
        .replace("{fish_loader_env}", &fish_loader_env())
}

/// The dynamic loader's variables that make a program load or read code it would not
/// otherwise (preloaded and audit libraries, library and framework search paths, glibc's
/// iconv modules and locale files). The project's environment may set them for its own
/// programs; the guard runs its utilities with them empty, which loads nothing.
const LOADER_VARS: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "GCONV_PATH",
    "LOCPATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "DYLD_FALLBACK_FRAMEWORK_PATH",
    "DYLD_VERSIONED_LIBRARY_PATH",
    "DYLD_VERSIONED_FRAMEWORK_PATH",
    "DYLD_ROOT_PATH",
];

/// [`LOADER_VARS`] as bash and zsh prefix assignments (`X= Y= ...`), five to a line,
/// continued with ` \` and indented like the lines they start.
fn loader_env() -> String {
    LOADER_VARS
        .chunks(5)
        .map(|vars| {
            vars.iter()
                .map(|v| format!("{v}="))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(" \\\n        ")
}

/// [`LOADER_VARS`] as fish function-local exports with no value (`set -lx X`).
fn fish_loader_env() -> String {
    LOADER_VARS
        .iter()
        .map(|v| format!("  set -lx {v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn run(shell: &str) -> Result<()> {
    let template = match shell {
        "zsh" => ZSH_SNIPPET_TEMPLATE,
        "bash" => BASH_SNIPPET_TEMPLATE,
        "fish" => FISH_SNIPPET_TEMPLATE,
        other => bail!(
            "Unsupported shell '{}'. Supported shells: zsh, bash, fish",
            other
        ),
    };
    let shadowenv = crate::fs_safe::which_outside_project("shadowenv");
    print!("{}", make_snippet(template, shell, shadowenv.as_deref()));
    Ok(())
}

/// bash's `shadowenv` function, defined only when the snippet sets shadowenv up (found
/// at `shadowenv`), so that without shadowenv `command -v shadowenv` and `type -t
/// shadowenv` still fail. Once shadowenv's hook is wrapped, `shadowenv init bash` prints
/// an eval-safe comment instead of shadowenv's init: shadowenv and hookbook are set up
/// already, and the init would only fail to redefine the read-only hook (an error at
/// each new shell, and the end of it under `set -e` in bash 5). Everything else, and an
/// init run by absolute path (as the snippet's own is), is shadowenv's own. Defined
/// with `function name {`, so an alias of that name defined earlier is not expanded in
/// its place.
fn bash_shadowenv_fn(shadowenv: Option<&std::path::Path>) -> String {
    if shadowenv.and_then(|p| p.to_str()).is_none() {
        return format!(
            "# shadowenv was not on PATH when `{BINARY} hook` ran: no `shadowenv` function."
        );
    }
    format!(
        r##"function shadowenv {{
  if [ "$#" -eq 2 ] && [ "$1" = init ] && [ "$2" = bash ] && [ -n "${{_{BINARY}_shadowenv_wrapped-}}" ]; then
    echo "# shadowenv is already set up by {BINARY}'s hook"
    return 0
  fi
  command shadowenv "$@"
}}"##
    )
}

/// The line that sets up shadowenv in the snippet: its own `shadowenv init <shell>`, run
/// by the absolute path found when `devy hook` runs (never a project-local one), shell
/// quoted. The snippet runs it after defining the guard and before wrapping shadowenv's
/// hook, so the hook is wrapped before its first run. A comment when shadowenv is not
/// installed (or its path is not UTF-8).
fn shadowenv_init(shell: &str, shadowenv: Option<&std::path::Path>) -> String {
    match shadowenv.and_then(|p| p.to_str()) {
        Some(path) if shell == "fish" => format!("{} init fish | source", fish_quote(path)),
        Some(path) => format!("eval \"$({} init {shell})\"", sh_quote(path)),
        None => format!(
            "# shadowenv was not on PATH when `{BINARY} hook` ran, so it is not set up here."
        ),
    }
}

// Each snippet defines:
//   1. The shadowenv guard. `shadowenv trust` signs the `.shadowenv.d` directory, not
//      the files in it, so lisp a `git pull` adds there after `{bin} up` (or a
//      `500_devy.lisp` it replaces) would be evaluated by shadowenv's own hook at the
//      next prompt, before any {bin} command runs. The guard runs before shadowenv's
//      hook, every time, and shadowenv's hook runs only when the guard succeeds. It
//      finds the `.shadowenv.d` shadowenv would load (the closest one above the
//      physical working directory), then:
//      - fails if that directory is not owned by the user: shadowenv's trust there is
//        never {bin}'s, and another user could swap what the checks below look at (a
//        `.error-*` entry, a signature) between the check and shadowenv's hook.
//      - fails if that directory is a symlink (a repository can commit one): shadowenv
//        follows it, so it would load whatever directory it points to (another
//        project's trusted one, say) and delete and write `.error-*` files there.
//      - fails if that directory holds a `.error-*` entry that is a symlink or not a
//        regular file, trusted or not: shadowenv (3.4.0) opens
//        `.error-<n>-<shell pid>` to create or truncate it, following symlinks, also in
//        a directory it does not trust, so a cloned repository could make it truncate a
//        user's file. That is upstream's issue; this check works around it.
//      - when the directory carries a `.trust-*` signature, removes the signatures if
//        the directory holds a non-regular entry, a `*.lisp` (any case, dotfiles
//        included) other than `500_devy.lisp`, or a `500_devy.lisp` that is not byte
//        for byte the copy {bin} keeps in the per-user state directory
//        (`$XDG_STATE_HOME/devy/shadowenv/`, or `$HOME/.local/state/devy/shadowenv/`,
//        each only when absolute), named by the random nonce on the file's first line
//        (`; devy-env <32 hex digits>`; see `env_manager::shadowenv::COPY_SUBDIR`).
//        Other regular files (shadowenv's own `.gitignore`, `.trust-*` and `.error-*`)
//        are fine, as in `shadowenv::refuse_foreign_entries`. Repository content can't write
//        that copy or learn a nonce it names, so it can't make a replaced file match. A
//        copy directory that is physically inside the project (the repository could
//        have written it) fails the check, with no exception for a project at `$HOME`:
//        a dotfiles repository there loses shadowenv's trust at every prompt (the Rust
//        side, which can ask git whether the default state directory is tracked, still accepts
//        it). bash and zsh keep a copy that matched (keyed by its path: {bin} writes
//        each copy once, atomically), so a prompt reads only the project's file.
//      - fails if a signature is left after that: `rm` runs by absolute path
//        (`zf_rm` first in zsh, `command -p rm` last in bash and zsh, never found
//        through PATH). zsh empties a signature none of them removed, which shadowenv
//        3.4.0 rejects (it aborts before loading anything), but only when it is the
//        user's, in the user's directory, with a link count of 1 (zsh/stat's `zstat`):
//        emptying a hard link would truncate the file it shares. bash and fish can't
//        count links without a subprocess, so they leave it; the failing guard still
//        keeps shadowenv's hook from running in that shell.
//      A failing guard prints one line saying why, once per directory and reason.
//
//      The physical working directory is what shadowenv uses (getcwd), not `$PWD`,
//      which a symlink retargeted after the `cd` (or a project that exports `PWD`)
//      would point elsewhere. bash and zsh walk up through `.`, `..`, `../..` (the
//      kernel resolves each step physically) and stop where `x -ef x/..`, failing when
//      they can't get there in 256 steps; whether the copy directory is inside the
//      project is decided the same way, from the copy directory upward. This needs no
//      subprocess. fish's builtin `pwd -P` resolves the `$PWD` string (which fish keeps
//      read-only), so fish runs the system `pwd -P` (getcwd) instead and walks up that
//      path. It keeps the answer while `test . -ef <answer>` holds (two stat calls, no
//      process; a renamed ancestor or a `cd` elsewhere breaks it), never keeps an answer
//      it rejected, and runs `pwd -P` every time on a fish before 3.6, which has no
//      `-ef`. It reads the output whole (`string collect -N`, fish 3.1+) and fails
//      unless it is one absolute path with no newline in it: a command substitution
//      would split a directory named `x<newline>y` into `x` and `y`.
//
//      The guard is shell-only and reads only small files, never evaluating them. Its
//      only subprocesses are `rm` when it removes trust and, in fish, `cmp` and `pwd`,
//      all taken from `/bin`, `/usr/bin` or `/run/current-system/sw/bin` (fish fails
//      closed without them; bash and zsh then try `command -p rm`, which searches the
//      system's default PATH), so a PATH the project's environment set can't substitute
//      them. They run with the dynamic loader's variables ([`LOADER_VARS`]: `LD_PRELOAD`,
//      `LD_AUDIT`, `DYLD_INSERT_LIBRARIES`, glibc's `GCONV_PATH` and the like) empty
//      (prefix assignments in bash and zsh, function-local `set -lx` in fish: no extra
//      process), since a project may set those for its own programs. `EXECIGNORE` and
//      `FUNCNEST`, which could hide them or stop the guard's functions,
//      `fish_function_path`, `fish_complete_path`, `FPATH` and `fpath`, which would load
//      functions at every prompt, and the prompt strings (`PS1`, `PROMPT`, ...), which
//      run commands at every prompt, are reserved environment keys
//      (`validate::reserved_env_key`). Without shadowenv's hook the guard does not run
//      at all, so a shell without shadowenv sees no walk and no notice.
//   2. Its hookup. The snippet runs shadowenv's init itself (see [`shadowenv_init`])
//      after defining the guard, wraps `__shadowenv_hook` (which shadowenv's prompt and
//      preexec hooks call by name) so the guard runs first, and runs the guard once.
//      Since a user may also run `shadowenv init` in their rc file, before or after
//      this snippet, the wrap is checked again ahead of each of shadowenv's hooks:
//      - zsh: `_{bin}_shadowenv_wrap` heads `precmd_functions` and `preexec_functions`
//        (hookbook appends shadowenv's after it and never moves them) and compares the
//        hook's text, which zsh exposes without a subshell.
//      - bash: `_{bin}_shadowenv_pre` heads hookbook's `__hookbook_functions` (its
//        DEBUG-trap preexec list, created here when missing so a later `shadowenv init`
//        keeps ours first), and puts itself first again when something went ahead. Its
//        PROMPT_COMMAND entry only has to come before hookbook's own entry for
//        shadowenv's hook, so it moves (to the front) only when it doesn't, and keeps `$?`
//        for the prompt hooks after it. Its PROMPT_COMMAND entry names ` __shadowenv_hook `, which
//        hookbook's `hookbook_add_hook` takes to mean shadowenv's hook is already there:
//        a later `shadowenv init` (even inside a function, or with stderr discarded,
//        where hookbook's DEBUG trap does not run ours first) adds no entry ahead of it,
//        and `_{bin}_shadowenv_pre precmd` runs shadowenv's hook itself unless hookbook
//        has an entry of its own (from an init before the snippet), which then runs
//        after ours. The wrap makes the wrapper and shadowenv's renamed hook read-only
//        (`readonly -f`), so once wrapped the hook never changes: a later
//        `shadowenv init`, wherever it runs (inside a function, with stderr discarded,
//        after VS Code or starship moved devy's entry, after PROMPT_COMMAND was
//        replaced, all places where hookbook's DEBUG trap doesn't run ours around it
//        and hookbook may add its own entry ahead of ours), gets bash's "readonly
//        function" error for the hook and runs the rest of its init (whose
//        `hookbook_add_hook __shadowenv_hook` adds nothing), and whatever calls
//        the hook by name runs the guard. Sourced again, the snippet skips its own
//        init when the hook is wrapped already. A hook that was never wrapped (a
//        user's `shadowenv init` after a snippet made without shadowenv) is wrapped by
//        the snippet's read-only copy of hookbook's `hookbook_add_hook`, which
//        shadowenv's init calls right after defining the hook (hookbook's own
//        definition of it in that init fails, like the hook's), so it is wrapped before
//        the init adds hookbook's PROMPT_COMMAND entry, wherever that init runs; the
//        pre-hook also wraps one it finds unwrapped. That copy is defined only when the
//        hook is still unwrapped after the snippet's own init (in practice: a snippet
//        made without shadowenv), since once wrapped it is not needed and a read-only
//        copy would refuse other tools' hookbook. A snippet sourced again after
//        shadowenv was installed runs its init with that copy in place: quietly (stderr
//        on /dev/null, in an `||` list for `set -e`), and the copy wraps the hook.
//        Reading a function's definition in bash 3.2 means `declare -f` in a subshell,
//        so the snippet forks once per source while the hook is unwrapped (and the
//        pre-hook once when such an init runs), never on an ordinary prompt. When the
//        snippet sets shadowenv up, a `shadowenv` function makes `shadowenv init bash`
//        print only a comment once the hook is wrapped, so the usual
//        `eval "$(shadowenv init bash)"` after the snippet does not even reach the
//        hook's read-only error (or, under `set -e` in bash 5, end the shell); an init
//        run by absolute path still gets it. Without shadowenv no `shadowenv` function
//        is defined, so `command -v shadowenv` still tells whether it is installed.
//        So the guard runs once per prompt and per command (twice per prompt when
//        hookbook has its own PROMPT_COMMAND entry, from an init before the snippet:
//        the wrapper guards that run again), `_{bin}_shadowenv_ok` tells the
//        wrapper the guard just passed: set around the hook run `_{bin}_shadowenv_pre`
//        starts itself, or before a command when shadowenv's hook is the very next
//        entry in `__hookbook_functions`. The wrapper clears it, and runs the guard
//        whenever it is not set, so a direct call is always guarded. The entry is found
//        by a fixed token in every element of PROMPT_COMMAND (an array from bash 5.1)
//        and in starship's copy of it (`STARSHIP_PROMPT_COMMAND`, or
//        `_PRESERVED_PROMPT_COMMAND` before starship 1.19), and has no trailing `;`
//        (bash-preexec strips it). In starship's copy it stays only while PROMPT_COMMAND
//        has no entry of hookbook's (which a later `shadowenv init` puts ahead of
//        starship's hook); otherwise it moves to the front of PROMPT_COMMAND (where it
//        cannot keep `PIPESTATUS` or `$_` for the prompt hooks after it, only `$?`).
//        Later copies of the entry in PROMPT_COMMAND become `_{bin}_shadowenv_return`
//        (with stderr on /dev/null, which hookbook's DEBUG trap skips). A copy another
//        function runs (a terminal integration such as VS Code keeps PROMPT_COMMAND in
//        a variable of its own and evals it from its prompt hook: a `precmd` with
//        `FUNCNAME[1]` set) leaves PROMPT_COMMAND alone unless hookbook's entry is in
//        it: hookbook's DEBUG trap skips the integration's hook only while
//        PROMPT_COMMAND is exactly that hook, and would otherwise run shadowenv's
//        preexec hook at every prompt. Nor does anything but the snippet itself (when
//        sourced) add a missing entry to PROMPT_COMMAND. The copy is a no-op right
//        after bash ran the entry from PROMPT_COMMAND in the same prompt (a command in
//        between, seen by hookbook's preexec as a command that is not one of
//        PROMPT_COMMAND's, compared whole, ends that), so each prompt, an empty
//        line's included, runs the guard once, except the first one after a second
//        copy appears.
//      - fish: the wrapper takes the place of shadowenv's handler (fish does not order
//        event handlers) and calls a copy of it. `__{bin}_shadowenv_pre` (on
//        `fish_prompt` and `PWD`) runs the guard and wraps a hook that is not wrapped
//        (a wrapped one runs the guard itself, so the guard and `pwd -P` run once per
//        event). Wrapping removes shadowenv's handler, which fish then skips in that
//        event, so the pre-hook runs shadowenv's hook itself when the guard passed,
//        unless shadowenv's handler already ran in that event (it clears
//        `__shadowenv_force_run`, which `shadowenv init` sets).
//   3. A `{bin}` shell function that intercepts `{bin} up` to apply the new environment
//      in the current shell right away: it forces a run of shadowenv's (wrapped) hook
//      with `__shadowenv_force_run`, as `shadowenv init` does.
//   4. A completion function/block that provides tab-completion for all built-in
//      subcommands and dynamically completes user-defined commands from {bin}.yml
//      by calling `command {bin} _commands`.
//
// MAINTENANCE: All three snippets (ZSH, BASH, FISH) must be kept in sync.
// When adding a new subcommand, update all three constants AND the test lists in
// `all_builtin_subcommands_appear_in_*_snippet` below.

const ZSH_SNIPPET_TEMPLATE: &str = r#"
# Removes shadowenv's trust when .shadowenv.d holds files {bin} did not write (see
# `{bin} hook`'s source for the design). Runs before shadowenv's hook, which it wraps,
# and fails when shadowenv must not run here.
_{bin}_shadowenv_guard() {
  emulate -L zsh
  local p=.
  integer n=0 rc=0
  # The closest .shadowenv.d above the working directory, found through `..` so every
  # step is resolved physically, as shadowenv's getcwd() is: $PWD plays no part.
  while [[ ! -d $p/.shadowenv.d ]]; do
    if [[ $p -ef $p/.. ]]; then _{bin}_shadowenv_noted=; return 0; fi
    if [[ ! -d $p/.. ]] || (( n >= 256 )); then
      _{bin}_shadowenv_note "walk|$PWD" "could not check the directories above this one for a .shadowenv.d, so shadowenv is not run here"
      return 1
    fi
    p=$p/..
    (( ++n ))
  done
  # shadowenv's trust in a directory another user owns is never {bin}'s, and what the
  # checks below test there could be swapped before shadowenv's hook runs.
  if [[ ! -O $p/.shadowenv.d ]]; then
    _{bin}_shadowenv_note "owner|$PWD|$p" ".shadowenv.d is not owned by you, so shadowenv is not run here"
    return 1
  fi
  # A symlink a repository committed: shadowenv would load (and write its `.error-*`
  # file into) wherever it points, another project's trusted directory included.
  if [[ -L $p/.shadowenv.d ]]; then
    _{bin}_shadowenv_note "symlink|$PWD|$p" ".shadowenv.d is a symbolic link, so shadowenv is not run here; remove it"
    return 1
  fi
  if ! _{bin}_shadowenv_errors_ok $p/.shadowenv.d; then
    rc=1
    _{bin}_shadowenv_note "error|$PWD|$p" ".shadowenv.d holds a .error-* entry that is not a regular file, which shadowenv would write through, so shadowenv is not run here; remove it"
  fi
  _{bin}_shadowenv_check $p/.shadowenv.d $p || _{bin}_shadowenv_untrust $p/.shadowenv.d || rc=1
  (( rc )) || _{bin}_shadowenv_noted=
  return rc
}

# Prints the message $2 to stderr unless the last one printed had the key $1.
_{bin}_shadowenv_note() {
  [[ ${_{bin}_shadowenv_noted-} == "$1" ]] && return 0
  typeset -g _{bin}_shadowenv_noted=$1
  print -ru2 -- "{bin}: $2"
}

# Fails when the .shadowenv.d at $1 has a `.error-*` entry that is not a regular file:
# shadowenv's hook opens `.error-<n>-<shell pid>` to create or truncate it, following
# a symlink, whether or not it trusts the directory.
_{bin}_shadowenv_errors_ok() {
  emulate -L zsh
  local f
  for f in $1/.error-*(DN); do
    [[ -L $f || ! -f $f ]] && return 1
  done
  return 0
}

# Succeeds when the .shadowenv.d at $1 (in the directory $2) is untrusted or holds only
# what {bin} and shadowenv wrote.
_{bin}_shadowenv_check() {
  emulate -L zsh
  local sd=$1 root=$2 f n line nonce state copies copy mine theirs
  integer i=0
  local -a trust
  trust=( $sd/.trust-*(DN^/) )
  (( $#trust )) || return 0
  for f in $sd/*(DN); do
    [[ -L $f || ! -f $f ]] && return 1
    n=${f:t}
    [[ $n == 500_devy.lisp ]] && continue
    [[ $n == *.[lL][iI][sS][pP] ]] && return 1
  done
  f=$sd/500_devy.lisp
  [[ -f $f ]] || return 0
  IFS= read -r line 2>/dev/null < $f || return 1
  nonce=${line#'; devy-env '}
  [[ $line == "; devy-env $nonce" && ${#nonce} -eq 32 && $nonce != *[^0123456789abcdef]* ]] || return 1
  # devy's state directory, as devy finds it.
  if [[ ${XDG_STATE_HOME-} == /* ]]; then state=$XDG_STATE_HOME
  elif [[ ${HOME-} == /* ]]; then state=$HOME/.local/state
  else return 1
  fi
  copies=$state/devy/shadowenv
  [[ -d $copies ]] || return 1
  # Copies physically inside the project could be the repository's.
  f=$copies
  while :; do
    [[ $f -ef $root ]] && return 1
    [[ $f -ef $f/.. ]] && break
    (( ++i < 256 )) && [[ -d $f/.. ]] || return 1
    f=$f/..
  done
  f=$sd/500_devy.lisp
  copy=$copies/$nonce.lisp
  [[ -f $copy && ! -L $copy && -O $copy ]] || return 1
  # `read -d ''` stops at a NUL byte and then succeeds: a file with one is not devy's.
  # A copy that matched is kept, so later prompts read only the project's file: {bin}
  # writes one copy per nonce, atomically, and never changes it.
  if [[ ${_{bin}_shadowenv_copy_key-} == "$copy" ]]; then
    mine=$_{bin}_shadowenv_copy_text
  else
    IFS= read -r -d '' mine 2>/dev/null < $copy && return 1
  fi
  IFS= read -r -d '' theirs 2>/dev/null < $f && return 1
  [[ $mine == "$theirs" ]] || return 1
  typeset -g _{bin}_shadowenv_copy_key=$copy _{bin}_shadowenv_copy_text=$mine
}

# Removes shadowenv's trust from the .shadowenv.d at $1, failing when a signature is
# left. rm is zsh's own `zf_rm` or run by absolute path (not found through PATH, which
# the project's environment may set), then `command -p rm`. A signature none of them
# removes is emptied, which shadowenv never accepts, but only when it and the directory
# are the user's and it has no other link (zsh/stat's `zstat`, no subprocess), so the
# truncation can't reach another file.
_{bin}_shadowenv_untrust() {
  emulate -L zsh
  local f rm removed= left=
  local -a nlink
  for f in $1/.trust-*(DN^/); do
    if (( ${+builtins[zf_rm]} )); then
      zf_rm -f -- $f 2>/dev/null
    fi
    for rm in /bin/rm /usr/bin/rm /run/current-system/sw/bin/rm -p; do
      [[ -f $f || -L $f ]] || break
      if [[ $rm == -p ]]; then
        {loader_env} \
          command -p rm -f -- $f 2>/dev/null
      elif [[ -x $rm ]]; then
        {loader_env} \
          command $rm -f -- $f 2>/dev/null
      fi
    done
    if [[ -f $f && ! -L $f && -O $f && -O $1 ]] && (( ${+builtins[zstat]} )) \
      && zstat -A nlink +nlink -- $f && (( nlink[1] == 1 )); then
      : >| $f
    fi 2>/dev/null
    if [[ -f $f || -L $f ]]; then left=1; else removed=1; fi
  done
  if [[ -n $left ]]; then
    _{bin}_shadowenv_note "trust|$PWD|$1" "could not remove shadowenv's trust from .shadowenv.d, which holds files {bin} did not write, so shadowenv is not run here; delete its .trust-* files"
    return 1
  fi
  [[ -n $removed ]] && print -ru2 -- "{bin}: .shadowenv.d holds files {bin} did not write, so shadowenv's trust was removed; run {bin} up"
  return 0
}

# Wraps shadowenv's hook so the guard runs first, and shadowenv only when it succeeds.
# It heads precmd_functions and preexec_functions, so a hook that `shadowenv init`
# (re)defined is wrapped before it runs.
_{bin}_shadowenv_wrap() {
  emulate -L zsh
  (( ${+functions[__shadowenv_hook]} )) || return 0
  [[ ${functions[__shadowenv_hook]} == *_{bin}_shadowenv_guard* ]] && return 0
  functions[_{bin}_shadowenv_orig_hook]=${functions[__shadowenv_hook]}
  __shadowenv_hook() { _{bin}_shadowenv_guard && _{bin}_shadowenv_orig_hook "$@"; }
}
zmodload zsh/parameter 2>/dev/null
zmodload -F zsh/files b:zf_rm 2>/dev/null
zmodload -F zsh/stat b:zstat 2>/dev/null
typeset -ga precmd_functions preexec_functions
# The guard's state starts empty and is never exported, whatever the environment held.
typeset -g +x _{bin}_shadowenv_copy_key= _{bin}_shadowenv_copy_text= _{bin}_shadowenv_noted=
{shadowenv_init}
_{bin}_shadowenv_wrap
precmd_functions=(_{bin}_shadowenv_wrap ${precmd_functions:#_{bin}_shadowenv_wrap})
preexec_functions=(_{bin}_shadowenv_wrap ${preexec_functions:#_{bin}_shadowenv_wrap})
# Without shadowenv's hook there is nothing to guard (yet): no walk, no notice.
(( ${+functions[__shadowenv_hook]} )) && _{bin}_shadowenv_guard

# `function name {` rather than `name() {`, so an alias of the same name defined
# earlier is not expanded in its place.
function {bin} {
  if [ "${1-}" = "up" ]; then
    command {bin} "$@" || return
    # Apply the new environment now: a forced run of shadowenv's (wrapped) hook.
    if (( ${+functions[__shadowenv_hook]} )); then
      _{bin}_shadowenv_wrap
      __shadowenv_force_run=1
      __shadowenv_hook
    fi
    return 0
  elif [ "${1-}" = "hook" ]; then
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
    'prune:Remove services left behind by removed checkouts'
  )
  # _describe splits each entry at its first unescaped `:` into name and description,
  # so colons in project command names (`db:migrate`) are escaped.
  local user_cmd
  while IFS= read -r user_cmd; do
    [[ -n "$user_cmd" ]] && subcmds+=("${user_cmd//:/\\:}")
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
        '--print[Print the skill without writing]' \
        '(--all)*--agent[Write the skill only for this agent]:agent:(claude codex gemini cursor copilot windsurf opencode amp)' \
        '(--agent)--all[Write the skill for every supported agent]'
      ;;
    prune)
      _arguments \
        '--yes[Remove them without asking]' \
        '--volumes[Also remove the data volumes of removed containers]'
      ;;
  esac
}

compdef _{bin} {bin}
"#;

const BASH_SNIPPET_TEMPLATE: &str = r#"
# Removes shadowenv's trust when .shadowenv.d holds files {bin} did not write (see
# `{bin} hook`'s source for the design). Runs before shadowenv's hook, which it wraps,
# and fails when shadowenv must not run here. Works on bash 3.2.
_{bin}_shadowenv_guard() {
  local p=. n=0 rc=0 failglob= noglob= nocasematch= dotglob= globignore= ignored=
  # The closest .shadowenv.d above the working directory, found through `..` so every
  # step is resolved physically, as shadowenv's getcwd() is: $PWD plays no part.
  while [ ! -d "$p/.shadowenv.d" ]; do
    if [ "$p" -ef "$p/.." ]; then _{bin}_shadowenv_noted=; return 0; fi
    if [ ! -d "$p/.." ] || [ "$n" -ge 256 ]; then
      _{bin}_shadowenv_note "walk|$PWD" "could not check the directories above this one for a .shadowenv.d, so shadowenv is not run here"
      return 1
    fi
    p=$p/..
    n=$((n + 1))
  done
  # shadowenv's trust in a directory another user owns is never {bin}'s, and what the
  # checks below test there could be swapped before shadowenv's hook runs.
  if [ ! -O "$p/.shadowenv.d" ]; then
    _{bin}_shadowenv_note "owner|$PWD|$p" ".shadowenv.d is not owned by you, so shadowenv is not run here"
    return 1
  fi
  # A symlink a repository committed: shadowenv would load (and write its `.error-*`
  # file into) wherever it points, another project's trusted directory included.
  if [ -L "$p/.shadowenv.d" ]; then
    _{bin}_shadowenv_note "symlink|$PWD|$p" ".shadowenv.d is a symbolic link, so shadowenv is not run here; remove it"
    return 1
  fi
  # Globs must expand to every name (and to nothing when nothing matches), and
  # patterns must match case-sensitively, whatever the user's options.
  if shopt -q failglob; then failglob=1; shopt -u failglob; fi
  if shopt -q nocasematch; then nocasematch=1; shopt -u nocasematch; fi
  if shopt -q dotglob; then dotglob=1; fi
  if [ -n "${GLOBIGNORE+set}" ]; then ignored=1; globignore=$GLOBIGNORE; unset GLOBIGNORE; fi
  case $- in *f*) noglob=1; set +f ;; esac
  if ! _{bin}_shadowenv_errors_ok "$p/.shadowenv.d"; then
    rc=1
    _{bin}_shadowenv_note "error|$PWD|$p" ".shadowenv.d holds a .error-* entry that is not a regular file, which shadowenv would write through, so shadowenv is not run here; remove it"
  fi
  _{bin}_shadowenv_check "$p/.shadowenv.d" "$p" || _{bin}_shadowenv_untrust "$p/.shadowenv.d" || rc=1
  # Setting or unsetting GLOBIGNORE also switches dotglob, so it is restored after.
  if [ -n "$ignored" ]; then GLOBIGNORE=$globignore; fi
  if [ -n "$dotglob" ]; then shopt -s dotglob; else shopt -u dotglob; fi
  if [ -n "$nocasematch" ]; then shopt -s nocasematch; fi
  if [ -n "$failglob" ]; then shopt -s failglob; fi
  if [ -n "$noglob" ]; then set -f; fi
  [ "$rc" -eq 0 ] && _{bin}_shadowenv_noted=
  return $rc
}

# Prints the message $2 to stderr unless the last one printed had the key $1.
_{bin}_shadowenv_note() {
  [ "${_{bin}_shadowenv_noted-}" = "$1" ] && return 0
  _{bin}_shadowenv_noted=$1
  printf '%s\n' "{bin}: $2" >&2
}

# Fails when the .shadowenv.d at $1 has a `.error-*` entry that is not a regular file:
# shadowenv's hook opens `.error-<n>-<shell pid>` to create or truncate it, following
# a symlink, whether or not it trusts the directory.
_{bin}_shadowenv_errors_ok() {
  local f
  for f in "$1"/.error-*; do
    if [ -L "$f" ] || { [ -e "$f" ] && [ ! -f "$f" ]; }; then return 1; fi
  done
  return 0
}

# Succeeds when the .shadowenv.d at $1 (in the directory $2) is untrusted or holds only
# what {bin} and shadowenv wrote.
_{bin}_shadowenv_check() {
  local sd=$1 root=$2 f n i=0 trusted= line nonce state copies copy mine theirs
  for f in "$sd"/.trust-*; do
    if [ -f "$f" ] || [ -L "$f" ]; then trusted=1; break; fi
  done
  [ -n "$trusted" ] || return 0
  for f in "$sd"/* "$sd"/.[!.]* "$sd"/..?*; do
    if [ ! -e "$f" ] && [ ! -L "$f" ]; then continue; fi
    if [ -L "$f" ] || [ ! -f "$f" ]; then return 1; fi
    n=${f##*/}
    [ "$n" = 500_devy.lisp ] && continue
    case $n in *.[lL][iI][sS][pP]) return 1 ;; esac
  done
  f=$sd/500_devy.lisp
  [ -f "$f" ] || return 0
  IFS= read -r line 2>/dev/null < "$f" || return 1
  nonce=${line#'; devy-env '}
  [ "$line" = "; devy-env $nonce" ] && [ ${#nonce} -eq 32 ] || return 1
  case $nonce in *[!0123456789abcdef]*) return 1 ;; esac
  # devy's state directory, as devy finds it.
  case ${XDG_STATE_HOME-} in
    /*) state=$XDG_STATE_HOME ;;
    *) case ${HOME-} in /*) state=$HOME/.local/state ;; *) return 1 ;; esac ;;
  esac
  copies=$state/devy/shadowenv
  [ -d "$copies" ] || return 1
  # Copies physically inside the project could be the repository's.
  f=$copies
  while :; do
    [ "$f" -ef "$root" ] && return 1
    [ "$f" -ef "$f/.." ] && break
    [ "$i" -lt 256 ] && [ -d "$f/.." ] || return 1
    f=$f/..
    i=$((i + 1))
  done
  f=$sd/500_devy.lisp
  copy=$copies/$nonce.lisp
  [ -f "$copy" ] && [ ! -L "$copy" ] && [ -O "$copy" ] || return 1
  # `read -d ''` stops at a NUL byte and then succeeds: a file with one is not devy's.
  # A copy that matched is kept, so later prompts read only the project's file: {bin}
  # writes one copy per nonce, atomically, and never changes it.
  if [ "${_{bin}_shadowenv_copy_key-}" = "$copy" ]; then
    mine=$_{bin}_shadowenv_copy_text
  else
    IFS= read -r -d '' mine 2>/dev/null < "$copy" && return 1
  fi
  IFS= read -r -d '' theirs 2>/dev/null < "$f" && return 1
  [ "$mine" = "$theirs" ] || return 1
  _{bin}_shadowenv_copy_key=$copy
  _{bin}_shadowenv_copy_text=$mine
}

# Removes shadowenv's trust from the .shadowenv.d at $1, failing when a signature is
# left. rm is run by absolute path (not found through PATH, which the project's
# environment may set), then `command -p rm`. A signature none of them removes is left
# as it is (and the guard fails): bash can't count its links without a subprocess, so
# emptying it could truncate another file it is hard-linked to.
_{bin}_shadowenv_untrust() {
  local f rm removed= left=
  for f in "$1"/.trust-*; do
    { [ -f "$f" ] || [ -L "$f" ]; } || continue
    for rm in /bin/rm /usr/bin/rm /run/current-system/sw/bin/rm -p; do
      { [ -f "$f" ] || [ -L "$f" ]; } || break
      if [ "$rm" = -p ]; then
        {loader_env} \
          command -p rm -f -- "$f" 2>/dev/null
      elif [ -x "$rm" ]; then
        {loader_env} \
          command "$rm" -f -- "$f" 2>/dev/null
      fi
    done
    if [ -f "$f" ] || [ -L "$f" ]; then left=1; else removed=1; fi
  done
  if [ -n "$left" ]; then
    _{bin}_shadowenv_note "trust|$PWD|$1" "could not remove shadowenv's trust from .shadowenv.d, which holds files {bin} did not write, so shadowenv is not run here; delete its .trust-* files"
    return 1
  fi
  if [ -n "$removed" ]; then
    printf '%s\n' "{bin}: .shadowenv.d holds files {bin} did not write, so shadowenv's trust was removed; run {bin} up" >&2
  fi
  return 0
}

# Wraps shadowenv's hook so the guard runs first, and shadowenv only when the guard
# succeeds, then makes the wrapper and the copy of shadowenv's hook it calls read-only
# (`readonly -f`), so nothing replaces either for the rest of the shell's life: a later
# `shadowenv init` (anywhere, also inside a function or with stderr discarded, where
# hookbook's DEBUG trap doesn't run the pre-hook around it) can't redefine the hook
# (bash reports `__shadowenv_hook: readonly function` and runs the rest of the init),
# so whatever calls the hook by name, hookbook's PROMPT_COMMAND entry and preexec list
# included, runs the guard. Reading the hook's definition takes a subshell, so this
# forks once per source of the snippet, plus once when a hook appears that was never
# wrapped (a `shadowenv init` of the user's, when shadowenv was not on PATH as
# `{bin} hook` ran: from the snippet's `hookbook_add_hook`, which that init calls, or
# from `_{bin}_shadowenv_pre` at the next prompt or command). With `existing` it only takes over a hook that
# is wrapped already (the snippet sourced again) and wraps nothing.
# `_{bin}_shadowenv_ok`, set by `_{bin}_shadowenv_pre` right before it, says the guard
# has just passed.
_{bin}_shadowenv_wrap() {
  local def
  declare -F __shadowenv_hook >/dev/null 2>&1 || return 0
  def=$(declare -f __shadowenv_hook) || return 0
  case $def in
    *_{bin}_shadowenv_guard*) ;;
    *)
      [ "${1-}" = existing ] && return 0
      # shadowenv's own function, renamed (its definition, never project data).
      eval "_{bin}_shadowenv_orig_hook${def#__shadowenv_hook}"
      __shadowenv_hook() {
        if [ -n "${_{bin}_shadowenv_ok-}" ]; then
          _{bin}_shadowenv_ok=
        else
          _{bin}_shadowenv_guard || return 0
        fi
        _{bin}_shadowenv_orig_hook "$@"
      }
      ;;
  esac
  readonly -f __shadowenv_hook
  if declare -F _{bin}_shadowenv_orig_hook >/dev/null 2>&1; then
    readonly -f _{bin}_shadowenv_orig_hook
  fi
  _{bin}_shadowenv_wrapped=1
}

# Succeeds when the command $1 is one of PROMPT_COMMAND's (in any of its elements, split
# at newlines and `;`, without the blanks around it). An approximation, not bash's
# parser: a quoted `;` splits too, and a compound entry (`{ a; b; }`, `if ...`) is
# split into pieces that never equal a command bash runs from it. A miss counts as a
# command, which only clears `_{bin}_shadowenv_direct`, so the copy of {bin}'s entry
# runs the pre-hook (an extra guarded run of shadowenv's hook), never one less.
_{bin}_shadowenv_in_prompt_command() {
  local el rest piece nl='
'
  for el in ${PROMPT_COMMAND[@]+"${PROMPT_COMMAND[@]}"}; do
    rest=${el//;/$nl}
    while [ -n "$rest" ]; do
      piece=${rest%%"$nl"*}
      case $rest in
        *"$nl"*) rest=${rest#*"$nl"} ;;
        *) rest= ;;
      esac
      piece=${piece#"${piece%%[![:space:]]*}"}
      piece=${piece%"${piece##*[![:space:]]}"}
      [ -n "$piece" ] && [ "$piece" = "$1" ] && return 0
    done
  done
  return 1
}

# Runs ahead of shadowenv's hooks (see `_{bin}_shadowenv_first`), with `precmd` from
# PROMPT_COMMAND and `preexec` from hookbook's DEBUG trap: puts {bin}'s entries back
# ahead of shadowenv's if something went before them and, when shadowenv's hook exists,
# wraps it if it was never wrapped (once wrapped it can't change), then runs the guard.
# On a prompt it also runs shadowenv's hook itself, unless hookbook's own
# PROMPT_COMMAND entry does (it adds none when {bin}'s entry is already there). A copy
# of {bin}'s entry that another function runs (a terminal integration or prompt
# framework that moved PROMPT_COMMAND into a variable of its own and evals it from its
# prompt hook, such as VS Code's `__vsc_original_prompt_command`) is a `precmd` called
# from another function (`FUNCNAME[1]`). It leaves PROMPT_COMMAND as it is unless
# hookbook's own entry is there (see `_{bin}_shadowenv_first`), and it does nothing
# right after a run of the entry from PROMPT_COMMAND directly, which marks
# `_{bin}_shadowenv_direct`: the copy clears the mark, and so does a command (a
# `preexec` whose command is not one of PROMPT_COMMAND's, which hookbook's DEBUG trap
# also runs for an entry of PROMPT_COMMAND that does not discard stderr). So the copy
# runs in a prompt where bash did not run the entry itself (starship's copy; a line
# editor that evaluates PROMPT_COMMAND itself, such as ble.sh, from the prompt after it
# attached), and an empty line still gets its prompt. (Without hookbook's preexec, as
# under ble.sh or bash-preexec, the prompt right after a line editor attached can go
# without a run when bash ran the entry itself at the one before.)
_{bin}_shadowenv_pre() {
  _{bin}_shadowenv_ok=
  case ${1-} in
    precmd)
      if [ -z "${FUNCNAME[1]-}" ]; then
        _{bin}_shadowenv_direct=1
      elif [ -n "${_{bin}_shadowenv_direct-}" ]; then
        _{bin}_shadowenv_direct=
        case "${PROMPT_COMMAND[*]-}" in *"$_{bin}_shadowenv_token"*) return 0 ;; esac
      fi
      ;;
    preexec)
      if [ -n "${BASH_COMMAND-}" ]; then
        _{bin}_shadowenv_in_prompt_command "$BASH_COMMAND" || _{bin}_shadowenv_direct=
      else
        case "${PROMPT_COMMAND[*]-}" in
          *"$_{bin}_shadowenv_token"*) ;;
          *) _{bin}_shadowenv_direct= ;;
        esac
      fi
      ;;
  esac
  _{bin}_shadowenv_first
  declare -F __shadowenv_hook >/dev/null 2>&1 || return 0
  [ -n "${_{bin}_shadowenv_wrapped-}" ] || _{bin}_shadowenv_wrap
  _{bin}_shadowenv_guard || return 0
  case ${1-} in
    precmd)
      case "${PROMPT_COMMAND[*]-}
${STARSHIP_PROMPT_COMMAND-}
${_PRESERVED_PROMPT_COMMAND-}" in
        # hookbook's own entry (an init before the snippet) runs shadowenv's hook
        # later in this prompt, and the wrapper runs the guard again then: other
        # prompt hooks may run in between.
        *"__shadowenv_hook precmd 2>&3"*) ;;
        *)
          [ -n "${_{bin}_shadowenv_wrapped-}" ] && _{bin}_shadowenv_ok=1
          __shadowenv_hook precmd
          _{bin}_shadowenv_ok=
          ;;
      esac
      ;;
    preexec)
      # hookbook runs shadowenv's hook right after this one: it skips the guard.
      if [ -n "${_{bin}_shadowenv_wrapped-}" ] && [ "${__hookbook_functions[1]-}" = __shadowenv_hook ]; then
        _{bin}_shadowenv_ok=1
      fi
      ;;
  esac
  return 0
}

# Puts `_{bin}_shadowenv_pre` first in hookbook's preexec list (`__hookbook_functions`,
# which shadowenv's init keeps when it exists and appends to), and its PROMPT_COMMAND
# entry ahead of hookbook's own entry for shadowenv's hook (from an init before the
# snippet). Its entry stays where it is otherwise, so other prompt hooks keep their
# order; when it moves, it goes first. A missing entry is added (first) only with
# `add`, when the snippet is sourced, and not while starship's or VS Code's copy of
# PROMPT_COMMAND (`__vsc_original_prompt_command`) has it: from the pre-hook, an entry
# PROMPT_COMMAND lacks is being run from such a copy, and adding one ahead of a
# terminal integration's hook would make hookbook's DEBUG trap take that hook for a
# command at every prompt (it skips only a PROMPT_COMMAND that is exactly the command
# it runs). Like hookbook's own entries, it
# runs with stderr on /dev/null (its messages go to fd 3, the real stderr), which is how
# hookbook's DEBUG trap tells a prompt hook from a command and skips its preexec hooks.
# It keeps `$?` for the entries after it: saved first, and returned last by
# `_{bin}_shadowenv_return` (a function, so no subshell). The entry names
# ` __shadowenv_hook `, which tells hookbook (in a later `shadowenv init`) that the hook
# is already in PROMPT_COMMAND, so it adds no entry of its own ahead of {bin}'s.
# The entry is found by `_{bin}_shadowenv_token`, in every element of PROMPT_COMMAND (an
# array from bash 5.1, as bash-preexec makes it) and in starship's copy of it
# (`STARSHIP_PROMPT_COMMAND`, `_PRESERVED_PROMPT_COMMAND` before starship 1.19), which
# starship's prompt hook runs. There it is enough while PROMPT_COMMAND has no entry of
# hookbook's for shadowenv (a `shadowenv init` after starship's adds one ahead of
# starship's hook); with one, {bin}'s entry moves to the front of PROMPT_COMMAND. It has
# no trailing `;`, which bash-preexec strips. The copies of one that moves (also in
# starship's copy) are replaced by `_{bin}_shadowenv_keep`, a run of
# `_{bin}_shadowenv_return` with stderr on /dev/null (so hookbook's DEBUG trap doesn't
# take it for a command either), so the commands around them still parse and keep `$?`
# (the moved entry saved it earlier in the same prompt), but not `PIPESTATUS` or `$_`,
# which the entry's own commands replace. So are the copies after the first in
# PROMPT_COMMAND itself (`_{bin}_shadowenv_dedup`), so each prompt runs it once from
# the one after a second copy appears (that one can run it twice). Only copies of this
# exact text are replaced: another text with the token in it (such as an entry from an
# earlier build of this snippet in a long-lived shell) stays, runs the pre-hook too, and
# has `_{bin}_shadowenv_dedup` run, changing nothing, at every prompt; restarting the
# shell drops it.
_{bin}_shadowenv_keep='{ _{bin}_shadowenv_return; } 2>/dev/null'
_{bin}_shadowenv_entry='{ _{bin}_shadowenv_status=$?; _{bin}_shadowenv_pre precmd __shadowenv_hook 2>&3; _{bin}_shadowenv_return; } 4>&2 2>/dev/null 3>&4'
_{bin}_shadowenv_token='_{bin}_shadowenv_pre precmd __shadowenv_hook'
_{bin}_shadowenv_return() { return "${_{bin}_shadowenv_status:-0}"; }
_{bin}_shadowenv_first() {
  local fn i entry=$_{bin}_shadowenv_entry token=$_{bin}_shadowenv_token pc="${PROMPT_COMMAND[*]-}"
  local keep=$_{bin}_shadowenv_keep
  local -a rest
  if [ "${__hookbook_functions[0]-}" != _{bin}_shadowenv_pre ]; then
    rest=()
    for fn in ${__hookbook_functions[@]+"${__hookbook_functions[@]}"}; do
      [ "$fn" = _{bin}_shadowenv_pre ] || rest[${#rest[@]}]=$fn
    done
    __hookbook_functions=(_{bin}_shadowenv_pre ${rest[@]+"${rest[@]}"})
  fi
  case $pc in
    *"$token"*)
      case ${pc%%"$token"*} in
        *"__shadowenv_hook precmd 2>&3"*) ;;
        *)
          case ${pc#*"$token"} in *"$token"*) _{bin}_shadowenv_dedup ;; esac
          return 0
          ;;
      esac
      ;;
    *"__shadowenv_hook precmd 2>&3"*) ;;
    *)
      [ "${1-}" = add ] || return 0
      case "${STARSHIP_PROMPT_COMMAND-}
${_PRESERVED_PROMPT_COMMAND-}
${__vsc_original_prompt_command[*]-}" in
        *"$token"*) return 0 ;;
      esac
      ;;
  esac
  # Only a real array has more than its first element (bash 3.2 can't take the length
  # of a scalar under `set -u`, nor substitute in `${scalar[0]}`).
  if [ "${PROMPT_COMMAND[*]-}" != "${PROMPT_COMMAND-}" ]; then
    for i in "${!PROMPT_COMMAND[@]}"; do
      PROMPT_COMMAND[i]=${PROMPT_COMMAND[i]//"$entry"/$keep}
    done
  fi
  if [ -n "${STARSHIP_PROMPT_COMMAND-}" ]; then
    STARSHIP_PROMPT_COMMAND=${STARSHIP_PROMPT_COMMAND//"$entry"/$keep}
  fi
  if [ -n "${_PRESERVED_PROMPT_COMMAND-}" ]; then
    _PRESERVED_PROMPT_COMMAND=${_PRESERVED_PROMPT_COMMAND//"$entry"/$keep}
  fi
  pc=${PROMPT_COMMAND-}
  pc=${pc//"$entry"/$keep}
  PROMPT_COMMAND="$entry${pc:+
$pc}"
}
# Keeps the first of {bin}'s entries in PROMPT_COMMAND (in any of its elements) and
# replaces the others with `_{bin}_shadowenv_keep`.
_{bin}_shadowenv_dedup() {
  local i v head seen= entry=$_{bin}_shadowenv_entry keep=$_{bin}_shadowenv_keep
  if [ "${PROMPT_COMMAND[*]-}" != "${PROMPT_COMMAND-}" ]; then
    for i in "${!PROMPT_COMMAND[@]}"; do
      v=${PROMPT_COMMAND[i]}
      case $v in *"$entry"*) ;; *) continue ;; esac
      if [ -n "$seen" ]; then
        PROMPT_COMMAND[i]=${v//"$entry"/$keep}
      else
        seen=1
        head=${v%%"$entry"*}
        v=${v#*"$entry"}
        PROMPT_COMMAND[i]=$head$entry${v//"$entry"/$keep}
      fi
    done
  else
    v=${PROMPT_COMMAND-}
    case $v in *"$entry"*) ;; *) return 0 ;; esac
    head=${v%%"$entry"*}
    v=${v#*"$entry"}
    PROMPT_COMMAND=$head$entry${v//"$entry"/$keep}
  fi
}
# The hook's state starts empty and is never exported, whatever the environment held.
_{bin}_shadowenv_wrapped=
_{bin}_shadowenv_ok=
_{bin}_shadowenv_noted=
_{bin}_shadowenv_copy_key=
_{bin}_shadowenv_copy_text=
_{bin}_shadowenv_status=0
_{bin}_shadowenv_direct=
_{bin}_shadowenv_shim=
export -n _{bin}_shadowenv_wrapped _{bin}_shadowenv_ok _{bin}_shadowenv_noted \
  _{bin}_shadowenv_copy_key _{bin}_shadowenv_copy_text _{bin}_shadowenv_status \
  _{bin}_shadowenv_entry _{bin}_shadowenv_token _{bin}_shadowenv_keep \
  _{bin}_shadowenv_direct _{bin}_shadowenv_shim
declare -p __hookbook_functions >/dev/null 2>&1 || __hookbook_functions=()
# With stderr on /dev/null (the guard's notice goes to fd 3), so hookbook's DEBUG trap
# doesn't run shadowenv's preexec hook for each of these lines.
{ _{bin}_shadowenv_first add; _{bin}_shadowenv_wrap existing; } 4>&2 2>/dev/null 3>&4
# shadowenv's init, unless its hook is wrapped (and read-only) already: the snippet
# sourced again, or the pre-hook wrapped the hook of an init that ran before it.
# `_{bin}_shadowenv_shim` is set when the read-only `hookbook_add_hook` below is
# defined already (by an earlier snippet made without shadowenv, before shadowenv was
# installed). The init's own definition of `hookbook_add_hook` then fails, and its call
# comes to that copy, which wraps the hook just the same; so in that case the init runs
# with stderr on /dev/null (hiding bash's `hookbook_add_hook: readonly function`) and
# in an `||` list (where that failure can't end a `set -e` shell).
if [ -z "$_{bin}_shadowenv_wrapped" ]; then
  { declare -F hookbook_add_hook >/dev/null && case $(declare -f hookbook_add_hook 2>/dev/null) in
    *_{bin}_shadowenv_wrap*) _{bin}_shadowenv_shim=1 ;;
  esac; } 4>&2 2>/dev/null 3>&4
  if [ -z "$_{bin}_shadowenv_shim" ]; then
    {shadowenv_init}
    :
  else
    {
      {shadowenv_init}
      :
    } 2>/dev/null || :
  fi
fi
# Without shadowenv's hook there is nothing to guard (yet): no walk, no notice.
{ [ -n "$_{bin}_shadowenv_wrapped" ] || _{bin}_shadowenv_wrap; _{bin}_shadowenv_first add; ! declare -F __shadowenv_hook >/dev/null 2>&1 || _{bin}_shadowenv_guard 2>&3; } 4>&2 2>/dev/null 3>&4
# hookbook's `hookbook_add_hook` for bash, as shadowenv's init bundles it, with one
# change: it wraps shadowenv's hook (`_{bin}_shadowenv_wrap`) before hooking it up,
# unless it is wrapped (and read-only) already.
# shadowenv's init defines `__shadowenv_hook`, then hookbook, then calls
# `hookbook_add_hook __shadowenv_hook`; this copy is read-only, so hookbook's own
# definition in a later init fails (bash reports `hookbook_add_hook: readonly
# function` and runs the rest of the init) and that call comes here. So a
# `shadowenv init` of the user's that the snippet never saw (shadowenv was not on PATH
# as `{bin} hook` ran), even inside a function or with stderr discarded, where
# hookbook's DEBUG trap doesn't run `_{bin}_shadowenv_pre` around it, has its hook
# wrapped (and read-only) before hookbook's PROMPT_COMMAND entry or preexec list can
# call it. Hooks of other hookbook users are added as hookbook adds them.
# It is defined only when shadowenv's hook is still not wrapped after the snippet's own
# init (in practice: the snippet was made without shadowenv), and not again when it is
# defined already. Once the hook is wrapped it is not needed: the hook can't change,
# and while {bin}'s PROMPT_COMMAND entry is in PROMPT_COMMAND it names
# ` __shadowenv_hook `, so hookbook's own `hookbook_add_hook` adds no entry for it
# (when starship or a terminal integration moved that entry, the entry hookbook adds
# calls the read-only wrapper). Not defining it then leaves other tools
# that bundle hookbook free to define theirs (a read-only copy makes their definition
# fail, reported as `hookbook_add_hook: readonly function`, which ends a `set -e`
# shell in bash 5). With stderr on /dev/null like the lines above, so hookbook's DEBUG
# trap runs no preexec hook for it.
# Hookbook (https://github.com/Shopify/hookbook) is Copyright 2019 Shopify Inc., MIT
# license; the full notice is in shadowenv's init.
{ [ -n "$_{bin}_shadowenv_wrapped" ] || [ -n "$_{bin}_shadowenv_shim" ] || {
    function hookbook_add_hook {
      \local fn="$1"

      [[ "${fn}" == __shadowenv_hook && -z "${_{bin}_shadowenv_wrapped-}" ]] && _{bin}_shadowenv_wrap

      [[ ! "${PROMPT_COMMAND}" == *" $fn "* ]] && {
        # This is essentially:
        #   PROMPT_COMMAND="${fn}; ${PROMPT_COMMAND}"
        # ...except with weird magic to toggle off `-x` if it's set, much like
        # in the DEBUG trap above.
        PROMPT_COMMAND="{
        [[ \$- =~ x ]] && {
          \set +x; ${fn} precmd 2>&3; \set -x;
        } || {
          ${fn} precmd 2>&3;
        }
      } 4>&2 2>/dev/null 3>&4;
      ${PROMPT_COMMAND}"
      }

      __hookbook_array_contains "${fn}" "${__hookbook_functions[@]}" \
        || __hookbook_functions+=("${fn}")
    }
    readonly -f hookbook_add_hook
  }; } 4>&2 2>/dev/null 3>&4
{shadowenv_fn}

# `function name {` rather than `name() {`, so an alias of the same name defined
# earlier is not expanded in its place.
function {bin} {
  if [ "${1-}" = "up" ]; then
    command {bin} "$@" || return
    # Apply the new environment now: a forced run of shadowenv's (wrapped) hook.
    if declare -F __shadowenv_hook >/dev/null 2>&1; then
      [ -n "${_{bin}_shadowenv_wrapped-}" ] || _{bin}_shadowenv_wrap
      _{bin}_shadowenv_ok=
      __shadowenv_force_run=1
      __shadowenv_hook
    fi
    return 0
  elif [ "${1-}" = "hook" ]; then
    command {bin} hook bash
  else
    command {bin} "$@"
  fi
}

# Adds each word argument that starts with the prefix in $1 to COMPREPLY.
# Matching is a literal prefix test, so words are never expanded or evaluated.
_{bin}_complete_words() {
  local cur="$1" word
  shift
  for word in "$@"; do
    [[ $word == "$cur"* ]] && COMPREPLY+=("$word")
  done
}

# Like _{bin}_complete_words, but reads one candidate per line from stdin.
# Project data from `{bin} _commands` / `{bin} _services` only takes this path.
# A `while read` loop is used because macOS bash 3.2 has no array-reading builtin;
# callers feed it with a here-string, which (unlike `< <(...)`) also parses in
# posix mode on bash < 5.1.
# Bash inserts candidates unquoted, so only names made of letters, digits and
# `._:+@-` are offered; anything else (whitespace, control characters, `$`,
# backticks, quotes, globs) is skipped.
_{bin}_complete_lines() {
  local cur="$1" line
  while IFS= read -r line || [[ -n $line ]]; do
    [[ -z $line || $line == *[![:alnum:]._:+@-]* ]] && continue
    [[ $line == "$cur"* ]] && COMPREPLY+=("$line")
  done
}

_{bin}_completions() {
  local cur="${COMP_WORDS[COMP_CWORD]}"
  local subcmds
  subcmds=(up down services start stop restart status check doctor logs ask init hook pr export exec agent-setup prune)
  COMPREPLY=()

  if [ "$COMP_CWORD" -eq 1 ]; then
    _{bin}_complete_words "$cur" "${subcmds[@]}"
    _{bin}_complete_lines "$cur" <<< "$(command {bin} _commands 2>/dev/null)"
    return
  fi

  case "${COMP_WORDS[1]}" in
    up)
      _{bin}_complete_words "$cur" --update --dry-run --bootstrap
      ;;
    down)
      _{bin}_complete_words "$cur" --volumes
      ;;
    doctor)
      _{bin}_complete_words "$cur" --yes --no-ai --show-context
      ;;
    logs)
      case "${COMP_WORDS[COMP_CWORD-1]}" in
        -n|--lines) return ;;
      esac
      _{bin}_complete_words "$cur" --follow --lines --explain --show-context
      _{bin}_complete_lines "$cur" <<< "$(command {bin} _services 2>/dev/null)"
      ;;
    ask)
      _{bin}_complete_words "$cur" --show-context
      ;;
    init)
      _{bin}_complete_words "$cur" --force
      ;;
    hook)
      _{bin}_complete_words "$cur" zsh bash fish
      ;;
    export)
      if [ "${COMP_WORDS[COMP_CWORD-1]}" = "--format" ]; then
        _{bin}_complete_words "$cur" shell flake
      else
        _{bin}_complete_words "$cur" --format
      fi
      ;;
    status|services|check)
      _{bin}_complete_words "$cur" --json
      ;;
    exec)
      # The program name, then its arguments as files. compgen's output is read line
      # by line so file names are never word-split or globbed, and `-o filenames`
      # makes bash quote them on insertion. bash 3.2 has no compopt and would insert
      # them unquoted, so there names with shell syntax (anything outside
      # `[[:alnum:]._/:+@,=-]`) are skipped, as for project names.
      local kind=-f word quoted=
      if [ "$COMP_CWORD" -eq 2 ] || { [ "$COMP_CWORD" -eq 3 ] && [ "${COMP_WORDS[2]}" = "--" ]; }; then
        kind=-c
      fi
      [[ $(type -t compopt) == builtin ]] && compopt -o filenames 2>/dev/null && quoted=1
      while IFS= read -r word; do
        [[ -z $word ]] && continue
        [[ -z $quoted && $word == *[![:alnum:]._/:+@,=-]* ]] && continue
        COMPREPLY+=("$word")
      done <<< "$(compgen "$kind" -- "$cur")"
      ;;
    agent-setup)
      if [ "${COMP_WORDS[COMP_CWORD-1]}" = "--agent" ]; then
        _{bin}_complete_words "$cur" claude codex gemini cursor copilot windsurf opencode amp
      else
        _{bin}_complete_words "$cur" --force --agents-md --print --agent --all
      fi
      ;;
    prune)
      _{bin}_complete_words "$cur" --yes --volumes
      ;;
  esac
}

complete -F _{bin}_completions {bin}
"#;

const FISH_SNIPPET_TEMPLATE: &str = r#"
# Runs a system utility by its absolute path, so a PATH the project's environment set
# can't substitute it. Fails (127) when there is none: never falls back to PATH.
function __{bin}_sys
  set -l tool $argv[1]
  set -e argv[1]
  # Without the dynamic loader's variables (the project's environment may set them
  # for its own programs): empty, they load nothing.
{fish_loader_env}
  for dir in /bin /usr/bin /run/current-system/sw/bin
    if test -x $dir/$tool
      $dir/$tool $argv
      return
    end
  end
  return 127
end

# Prints the message $argv[2] to stderr unless the last one printed had the key $argv[1].
function __{bin}_shadowenv_note
  test "$__{bin}_shadowenv_noted" = "$argv[1]"; and return 0
  set -g __{bin}_shadowenv_noted $argv[1]
  echo "{bin}: $argv[2]" >&2
end

# Removes shadowenv's trust when .shadowenv.d holds files {bin} did not write (see
# `{bin} hook`'s source for the design). Runs before shadowenv's hook, which it wraps,
# and fails when shadowenv must not run here.
function __{bin}_shadowenv_guard
  # The physical working directory, as shadowenv's getcwd() sees it: fish's own
  # `pwd -P` resolves the $PWD string, so the system one runs, and its answer is kept
  # while `test . -ef` (two stat calls, no process) says it still names the working
  # directory; a fish before 3.6 has no `-ef` and runs `pwd -P` every time. Read whole
  # (a command substitution would split it at a newline in a directory name): one
  # absolute path and its one trailing newline, or nothing kept, and the guard fails.
  if not test . -ef "$__{bin}_guard_phys" 2>/dev/null
    set -g __{bin}_guard_phys
    set -l out (__{bin}_sys pwd -P 2>/dev/null | string collect -N)
    if test (count $out) -eq 1; and string match -qr -- '^/[^\n]*\n\z' "$out"
      set -g __{bin}_guard_phys (string replace -r -- '\n\z' '' "$out")
    end
  end
  set -l d $__{bin}_guard_phys
  if test (count $d) -ne 1; or not string match -q -- '/*' "$d"
    __{bin}_shadowenv_note "walk|$PWD" "could not read the working directory, so shadowenv is not run here"
    return 1
  end
  # The closest .shadowenv.d above it, as shadowenv finds it.
  set -l sd
  while true
    if test -d "$d/.shadowenv.d"
      set sd "$d/.shadowenv.d"
      break
    end
    if test -z "$d"; or not string match -q -- '*/*' "$d"
      set -g __{bin}_shadowenv_noted
      return 0
    end
    set d (string replace -r -- '/[^/]*$' '' "$d")
  end
  # shadowenv's trust in a directory another user owns is never {bin}'s, and what the
  # checks below test there could be swapped before shadowenv's hook runs.
  if not test -O "$sd"
    __{bin}_shadowenv_note "owner|$sd" ".shadowenv.d is not owned by you, so shadowenv is not run here"
    return 1
  end
  # A symlink a repository committed: shadowenv would load (and write its `.error-*`
  # file into) wherever it points, another project's trusted directory included.
  if test -L "$sd"
    __{bin}_shadowenv_note "symlink|$sd" ".shadowenv.d is a symbolic link, so shadowenv is not run here; remove it"
    return 1
  end
  set -l rc 0
  if not __{bin}_shadowenv_errors_ok "$sd"
    set rc 1
    __{bin}_shadowenv_note "error|$sd" ".shadowenv.d holds a .error-* entry that is not a regular file, which shadowenv would write through, so shadowenv is not run here; remove it"
  end
  __{bin}_shadowenv_check "$sd" "$d"
  or __{bin}_shadowenv_untrust "$sd"
  or set rc 1
  test $rc -eq 0; and set -g __{bin}_shadowenv_noted
  return $rc
end

# Fails when the .shadowenv.d at $argv[1] has a `.error-*` entry that is not a regular
# file: shadowenv's hook opens `.error-<n>-<shell pid>` to create or truncate it,
# following a symlink, whether or not it trusts the directory.
function __{bin}_shadowenv_errors_ok
  for f in $argv[1]/.error-*
    if test -L $f; or not test -f $f
      return 1
    end
  end
  return 0
end

# Succeeds when the .shadowenv.d at $argv[1] (in the directory $argv[2]) is untrusted or
# holds only what {bin} and shadowenv wrote.
function __{bin}_shadowenv_check
  set -l sd $argv[1]
  set -l root $argv[2]
  set -l trusted
  for f in $sd/.trust-*
    if test -f $f; or test -L $f
      set trusted 1
      break
    end
  end
  set -q trusted[1]; or return 0
  for f in $sd/* $sd/.*
    set -l n (string replace -r -- '.*/' '' $f)
    contains -- $n . ..; and continue
    if test -L $f; or not test -f $f
      return 1
    end
    if test "$n" != 500_devy.lisp; and string match -qi -- '*.lisp' $n
      return 1
    end
  end
  set -l f $sd/500_devy.lisp
  test -f $f; or return 0
  set -l line
  read line < $f; or return 1
  string match -qr -- '^; devy-env [0-9a-f]{32}$' "$line"; or return 1
  set -l nonce (string sub -s 12 -- "$line")
  # devy's state directory, as devy finds it.
  set -l state
  if string match -q -- '/*' "$XDG_STATE_HOME"
    set state "$XDG_STATE_HOME"
  else if string match -q -- '/*' "$HOME"
    set state "$HOME/.local/state"
  else
    return 1
  end
  # Copies physically inside the project could be the repository's (a literal prefix
  # test: the project's path is never read as a pattern).
  set -l copies (builtin realpath "$state/devy/shadowenv" 2>/dev/null); or return 1
  string match -q -- '/*' "$copies"; or return 1
  if test "$copies" = "$root"
    return 1
  end
  set -l prefix "$root/"
  if test (string sub -l (string length -- "$prefix") -- "$copies") = "$prefix"
    return 1
  end
  set -l copy "$copies/$nonce.lisp"
  test -f "$copy"; and not test -L "$copy"; and test -O "$copy"; or return 1
  __{bin}_sys cmp -s -- "$copy" "$f"
end

# Removes shadowenv's trust from the .shadowenv.d at $argv[1], failing when a signature
# is left. One rm does not remove is left as it is: fish can't count its links without
# a subprocess, so emptying it could truncate another file it is hard-linked to.
function __{bin}_shadowenv_untrust
  set -l removed
  set -l left
  for f in $argv[1]/.trust-*
    if test -f $f; or test -L $f
      __{bin}_sys rm -f -- $f 2>/dev/null
      if test -f $f; or test -L $f
        set left 1
      else
        set removed 1
      end
    end
  end
  if set -q left[1]
    __{bin}_shadowenv_note "trust|$argv[1]" "could not remove shadowenv's trust from .shadowenv.d, which holds files {bin} did not write, so shadowenv is not run here; delete its .trust-* files"
    return 1
  end
  if set -q removed[1]
    echo "{bin}: .shadowenv.d holds files {bin} did not write, so shadowenv's trust was removed; run {bin} up" >&2
  end
  return 0
end

# Wraps shadowenv's hook so the guard runs first, and shadowenv only when it succeeds:
# fish does not order event handlers, so the wrapper takes the place of shadowenv's
# handler and calls a copy of it (a copy keeps no event handlers).
function __{bin}_shadowenv_wrap
  functions -q __shadowenv_hook; or return 0
  string match -q -- '*__{bin}_shadowenv_guard*' (functions __shadowenv_hook); and return 0
  functions -e __{bin}_shadowenv_orig_hook 2>/dev/null
  functions -c __shadowenv_hook __{bin}_shadowenv_orig_hook; or return 0
  functions -e __shadowenv_hook
  function __shadowenv_hook --on-event fish_prompt --on-variable PWD
    __{bin}_shadowenv_guard $argv; and __{bin}_shadowenv_orig_hook $argv
  end
end
# Wraps a shadowenv set up anew (by a later `shadowenv init`), running the guard first
# since shadowenv's own handler may run in this same event. A hook already wrapped runs
# the guard itself, so it is not run twice. Wrapping removes shadowenv's handler, so
# fish skips it in this event and the new wrapper's handler only fires from the next
# one: when the guard passed, this runs shadowenv's hook itself, unless shadowenv's
# handler already ran in this event (it clears the `__shadowenv_force_run` that
# `shadowenv init` sets).
function __{bin}_shadowenv_pre --on-event fish_prompt --on-variable PWD
  functions -q __shadowenv_hook; or return 0
  string match -q -- '*__{bin}_shadowenv_guard*' (functions __shadowenv_hook); and return 0
  __{bin}_shadowenv_guard $argv
  set -l ok $status
  __{bin}_shadowenv_wrap
  test $ok -eq 0; and set -q __shadowenv_force_run; or return 0
  functions -q __shadowenv_hook; or return 0
  string match -q -- '*__{bin}_shadowenv_guard*' (functions __shadowenv_hook); or return 0
  __{bin}_shadowenv_orig_hook $argv
end
# The guard's state starts empty and is never exported, whatever the environment held.
set -eg __{bin}_guard_phys
set -eg __{bin}_shadowenv_noted
set -gu __{bin}_guard_phys
set -gu __{bin}_shadowenv_noted
{shadowenv_init}
__{bin}_shadowenv_wrap
# Without shadowenv's hook there is nothing to guard (yet): no walk, no notice.
functions -q __shadowenv_hook; and __{bin}_shadowenv_guard

function {bin}
  if test "$argv[1]" = "up"
    command {bin} $argv; or return
    # Apply the new environment now: a forced run of shadowenv's (wrapped) hook.
    if functions -q __shadowenv_hook
      __{bin}_shadowenv_wrap
      set -g __shadowenv_force_run 1
      __shadowenv_hook
    end
    return 0
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
  not __fish_seen_subcommand_from up down services start stop restart status check doctor logs ask init hook pr export exec agent-setup prune
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
complete -c {bin} -n __{bin}_no_subcommand -a prune    -d "Remove services left behind by removed checkouts"
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
complete -c {bin} -n "__fish_seen_subcommand_from agent-setup" -l agent -x -a "claude codex gemini cursor copilot windsurf opencode amp" -d "Write the skill only for this agent"
complete -c {bin} -n "__fish_seen_subcommand_from agent-setup" -l all -d "Write the skill for every supported agent"
complete -c {bin} -n "__fish_seen_subcommand_from prune" -l yes -d "Remove them without asking"
complete -c {bin} -n "__fish_seen_subcommand_from prune" -l volumes -d "Also remove the data volumes of removed containers"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn zsh_snippet() -> String {
        make_snippet(ZSH_SNIPPET_TEMPLATE, "zsh", None)
    }
    fn bash_snippet() -> String {
        make_snippet(BASH_SNIPPET_TEMPLATE, "bash", None)
    }
    fn fish_snippet() -> String {
        make_snippet(FISH_SNIPPET_TEMPLATE, "fish", None)
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

    /// shadowenv's checked-in bash init (`tests/fixtures/shadowenv-init/bash.sh`).
    const BASH_INIT: &str = include_str!("../../tests/fixtures/shadowenv-init/bash.sh");

    /// The lines of the bash `hookbook_add_hook` definition starting at `start` in
    /// `text` (up to its closing brace at the same indentation), each trimmed, blank
    /// lines dropped.
    fn add_hook_body(text: &str, start: usize) -> Vec<&str> {
        let first = &text[start..];
        let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
        let close = format!("\n{}}}\n", " ".repeat(start - line_start));
        let end = first.find(&close).expect("closing brace") + close.len();
        first[..end]
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// The snippet's read-only `hookbook_add_hook` relies on shadowenv's bash init
    /// defining `__shadowenv_hook` before it calls `hookbook_add_hook`, and on that
    /// being the only hook it adds: a rename or a new hook in a future init must be
    /// noticed (update the fixture from the new init, and the snippet).
    #[test]
    fn shadowenv_bash_init_adds_only_its_hook_after_defining_it() {
        let calls: Vec<&str> = BASH_INIT
            .lines()
            .filter_map(|l| l.strip_prefix("hookbook_add_hook "))
            .collect();
        assert_eq!(calls, ["__shadowenv_hook"]);
        let defined = BASH_INIT.find("__shadowenv_hook() {").unwrap();
        let call = BASH_INIT
            .find("\nhookbook_add_hook __shadowenv_hook")
            .unwrap();
        assert!(defined < call);
        // Nor anywhere else in the init (an indented call, say).
        assert_eq!(BASH_INIT.matches("hookbook_add_hook ").count(), 1);
    }

    /// The snippet's `hookbook_add_hook` is hookbook's bash one (from the checked-in
    /// init) plus the line that wraps shadowenv's hook first.
    #[test]
    fn bash_snippet_add_hook_is_hookbooks_plus_the_wrap() {
        let bash_branch = BASH_INIT.find("== \"bash\" ]] && {").unwrap();
        let theirs_at = bash_branch
            + BASH_INIT[bash_branch..]
                .find("hookbook_add_hook() {")
                .unwrap();
        let theirs = add_hook_body(BASH_INIT, theirs_at);
        let snippet = bash_snippet();
        let ours_at = snippet.find("function hookbook_add_hook {").unwrap();
        let mut ours = add_hook_body(&snippet, ours_at);
        let wrap = "[[ \"${fn}\" == __shadowenv_hook && -z \"${_devy_shadowenv_wrapped-}\" ]] && _devy_shadowenv_wrap";
        assert_eq!(ours.iter().filter(|l| **l == wrap).count(), 1, "{ours:#?}");
        ours.retain(|l| *l != wrap);
        // Declared alias-safe: `function name {` for hookbook's `name() {`.
        assert_eq!(ours[0], "function hookbook_add_hook {");
        assert_eq!(theirs[0], "hookbook_add_hook() {");
        assert_eq!(ours[1..], theirs[1..]);
        assert!(snippet.contains("readonly -f hookbook_add_hook"));
        assert!(snippet.contains("Copyright 2019 Shopify Inc."));
        // Defined only while shadowenv's hook is unwrapped, and not over an earlier copy.
        assert!(
            snippet.contains(
                "{ [ -n \"$_devy_shadowenv_wrapped\" ] || [ -n \"$_devy_shadowenv_shim\" ] || {\n    function hookbook_add_hook {"
            ),
            "{snippet}"
        );
    }

    /// bash's `shadowenv` function exists only when the snippet sets shadowenv up, so
    /// `command -v shadowenv` still fails without it; once the hook is wrapped it turns
    /// `shadowenv init bash` into an eval-safe comment.
    #[test]
    fn bash_shadowenv_function_only_with_shadowenv() {
        assert!(!bash_snippet().contains("function shadowenv"));
        assert!(!bash_snippet().contains("shadowenv()"));
        let path = std::path::Path::new("/opt/bin/shadowenv");
        let s = make_snippet(BASH_SNIPPET_TEMPLATE, "bash", Some(path));
        assert_eq!(s.matches("\nfunction shadowenv {\n").count(), 1, "{s}");
        assert!(
            s.contains("    echo \"# shadowenv is already set up by devy's hook\"\n    return 0\n"),
            "{s}"
        );
        assert!(!s.contains("{shadowenv_fn}"));
        assert!(!make_snippet(ZSH_SNIPPET_TEMPLATE, "zsh", Some(path)).contains("shadowenv {"));
    }

    /// The functions a user might also have an alias for are defined with
    /// `function name {`, which an alias of that name doesn't break.
    #[test]
    fn snippets_define_wrappers_alias_safe() {
        for s in [zsh_snippet(), bash_snippet()] {
            assert!(s.contains("\nfunction devy {\n"), "{s}");
            assert!(!s.contains("\ndevy() {"), "{s}");
        }
        assert!(!bash_snippet().contains("hookbook_add_hook() {"));
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

    /// `devy up` applies the new environment through a forced run of shadowenv's hook:
    /// shadowenv 3 has no `shadowenv hook <shell>`.
    #[test]
    fn snippets_force_a_shadowenv_hook_run_after_up() {
        for (shell, s, force) in [
            (
                "zsh",
                zsh_snippet(),
                "__shadowenv_force_run=1\n      __shadowenv_hook\n",
            ),
            (
                "bash",
                bash_snippet(),
                "__shadowenv_force_run=1\n      __shadowenv_hook\n",
            ),
            (
                "fish",
                fish_snippet(),
                "set -g __shadowenv_force_run 1\n      __shadowenv_hook\n",
            ),
        ] {
            assert!(s.contains(force), "{shell}: {s}");
            assert!(!s.contains("shadowenv hook"), "{shell}: {s}");
        }
    }

    /// shadowenv's init runs inside the snippet, after the guard is defined and before
    /// the hook is wrapped, by the absolute path `devy hook` found, quoted.
    #[test]
    fn shadowenv_init_runs_between_the_guard_and_the_wrap() {
        let path = std::path::Path::new("/opt/it's here/shadowenv");
        let bash = shadowenv_init("bash", Some(path));
        assert_eq!(bash, "eval \"$('/opt/it'\\''s here/shadowenv' init bash)\"");
        assert_eq!(
            shadowenv_init("zsh", Some(path)),
            "eval \"$('/opt/it'\\''s here/shadowenv' init zsh)\""
        );
        assert_eq!(
            shadowenv_init("fish", Some(path)),
            "'/opt/it\\'s here/shadowenv' init fish | source"
        );
        assert!(shadowenv_init("bash", None).starts_with("# shadowenv was not on PATH"));
        for (template, init, wrap) in [
            (ZSH_SNIPPET_TEMPLATE, "zsh", "_devy_shadowenv_wrap"),
            (
                BASH_SNIPPET_TEMPLATE,
                "bash",
                "{ [ -n \"$_devy_shadowenv_wrapped\" ] || _devy_shadowenv_wrap;",
            ),
            (FISH_SNIPPET_TEMPLATE, "fish", "__devy_shadowenv_wrap"),
        ] {
            let line = shadowenv_init(init, Some(path));
            let s = make_snippet(template, init, Some(path));
            let guard = s
                .find("_shadowenv_guard()")
                .or_else(|| s.find("function __devy_shadowenv_guard"));
            // bash runs the init in one of two branches: as is, or quietly when the
            // read-only `hookbook_add_hook` of an earlier snippet is in place.
            let runs: Vec<usize> = s.match_indices(&line).map(|(i, _)| i).collect();
            assert_eq!(runs.len(), if init == "bash" { 2 } else { 1 }, "{init}");
            for at in runs {
                // The first line after the init that is not a comment, the other
                // branch's init, nor part of bash's `if` (which skips the init when the
                // hook is wrapped already, and picks the branch) wraps the hook.
                let next = s[at..].lines().skip(1).find(|l| {
                    let l = l.trim_start();
                    !l.starts_with('#')
                        && l != line
                        && ![":", "fi", "else", "{", "} 2>/dev/null || :"].contains(&l)
                });
                assert!(guard.is_some_and(|g| g < at), "{init}: guard before init");
                assert!(
                    next.is_some_and(|l| l.starts_with(wrap)),
                    "{init}: wrapped right after init: {next:?}"
                );
            }
        }
    }

    /// The guard fails closed: shadowenv's hook runs only after it succeeds, utilities
    /// never come from PATH, and bash's PROMPT_COMMAND entry names shadowenv's hook so
    /// hookbook adds no entry of its own ahead of it.
    #[test]
    fn snippets_fail_closed() {
        let zsh = zsh_snippet();
        assert!(
            zsh.contains("_devy_shadowenv_guard && _devy_shadowenv_orig_hook \"$@\""),
            "{zsh}"
        );
        let bash = bash_snippet();
        assert!(
            bash.contains("_devy_shadowenv_guard || return 0\n        fi\n        _devy_shadowenv_orig_hook \"$@\""),
            "{bash}"
        );
        let entry = bash
            .lines()
            .find_map(|l| l.strip_prefix("_devy_shadowenv_entry='"))
            .expect("entry");
        assert!(entry.contains(" __shadowenv_hook "), "{entry}");
        // Every rm (two runs in each of bash and zsh) and every fish utility runs with
        // all of the dynamic loader's variables empty.
        let fish = fish_snippet();
        for var in LOADER_VARS {
            for s in [&zsh, &bash] {
                assert_eq!(s.matches(&format!(" {var}= ")).count(), 2, "{var}: {s}");
            }
            assert!(fish.contains(&format!("\n  set -lx {var}\n")), "{var}");
        }
        assert!(LOADER_VARS.contains(&"LD_AUDIT") && LOADER_VARS.contains(&"GCONV_PATH"));
        for s in [&zsh, &bash] {
            for rm in ["/bin/rm", "/usr/bin/rm", "/run/current-system/sw/bin/rm"] {
                assert!(s.contains(rm), "{rm}");
            }
        }
        // Only zsh, which can count links without a subprocess, empties a signature rm
        // left, and only a single-link one of the user's in the user's directory.
        assert!(
            zsh.contains("-O $f && -O $1 ]] && (( ${+builtins[zstat]} ))"),
            "{zsh}"
        );
        assert!(
            zsh.contains("(( nlink[1] == 1 )); then\n      : >| $f"),
            "{zsh}"
        );
        assert!(!bash.contains(": >|"), "{bash}");
        let fish = fish_snippet();
        assert!(
            fish.contains("__devy_shadowenv_guard $argv; and __devy_shadowenv_orig_hook $argv"),
            "{fish}"
        );
        let sys = &fish[fish.find("function __devy_sys").unwrap()..];
        let sys = &sys[..sys.find("\nend\n").unwrap()];
        assert!(!sys.contains("command"), "no PATH fallback: {sys}");
        assert!(sys.contains("return 127"), "{sys}");
        assert!(!fish.contains("printf '' >"), "{fish}");
        assert!(
            !fish.contains("builtin pwd"),
            "no fallback to fish's pwd: {fish}"
        );
        for s in [&zsh, &bash, &fish] {
            assert!(s.contains("/.error-*"), "the .error-* check");
            assert!(
                s.contains(".shadowenv.d is not owned by you"),
                "owner check"
            );
        }
    }

    #[test]
    fn zsh_snippet_contains_completion_function() {
        let s = zsh_snippet();
        assert!(s.contains(&format!("_{}()", BINARY)));
        assert!(s.contains(&format!("compdef _{bin} {bin}", bin = BINARY)));
        assert!(s.contains("_commands"));
        // _describe reads an unescaped `:` as the start of a description.
        assert!(s.contains("subcmds+=(\"${user_cmd//:/\\\\:}\")"), "{s}");
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
            .find_map(|l| l.trim().strip_prefix("subcmds=("))
            .and_then(|l| l.strip_suffix(')'))
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
        assert!(
            bash_snippet().contains("    down)\n      _devy_complete_words \"$cur\" --volumes\n")
        );
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
            "_devy_complete_words \"$cur\" --follow --lines --explain --show-context\n      _devy_complete_lines \"$cur\" <<< \"$(command devy _services 2>/dev/null)\"\n"
        ));
        assert!(bash.contains("    ask)\n      _devy_complete_words \"$cur\" --show-context\n"));
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
            "    doctor)\n      _devy_complete_words \"$cur\" --yes --no-ai --show-context\n"
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
            "'(--all)*--agent[Write the skill only for this agent]:agent:(claude codex gemini cursor copilot windsurf opencode amp)'",
            "'(--agent)--all[",
        ] {
            assert!(zsh.contains(needle), "zsh missing {needle:?}");
        }
        let bash = bash_snippet();
        for needle in [
            "    status|services|check)\n      _devy_complete_words \"$cur\" --json\n",
            "done <<< \"$(compgen \"$kind\" -- \"$cur\")\"",
            "[[ $(type -t compopt) == builtin ]] && compopt -o filenames 2>/dev/null",
            "    agent-setup)\n      if [ \"${COMP_WORDS[COMP_CWORD-1]}\" = \"--agent\" ]; then\n        _devy_complete_words \"$cur\" claude codex gemini cursor copilot windsurf opencode amp\n      else\n        _devy_complete_words \"$cur\" --force --agents-md --print --agent --all\n      fi\n",
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
            "-n \"__fish_seen_subcommand_from agent-setup\" -l agent -x -a \"claude codex gemini cursor copilot windsurf opencode amp\" ",
            "-n \"__fish_seen_subcommand_from agent-setup\" -l all ",
            "hook pr export exec agent-setup prune\nend",
        ] {
            assert!(fish.contains(needle), "fish missing {needle:?}");
        }
    }

    #[test]
    fn snippets_complete_prune_and_its_flags() {
        let zsh = zsh_snippet();
        assert!(zsh.contains("    'prune:"), "{zsh}");
        assert!(
            zsh.contains(
                "    prune)\n      _arguments \\\n        '--yes[Remove them without asking]' \\\n        \
                 '--volumes[Also remove the data volumes of removed containers]'\n"
            ),
            "{zsh}"
        );
        let bash = bash_snippet();
        assert!(bash.contains("exec agent-setup prune)\n"), "{bash}");
        assert!(
            bash.contains("    prune)\n      _devy_complete_words \"$cur\" --yes --volumes\n"),
            "{bash}"
        );
        let fish = fish_snippet();
        assert!(fish.contains("-a prune "), "{fish}");
        for flag in ["yes", "volumes"] {
            assert!(
                fish.contains(&format!(
                    "-n \"__fish_seen_subcommand_from prune\" -l {flag} "
                )),
                "{fish}"
            );
        }
    }

    #[test]
    fn snippets_do_not_complete_a_removed_allow_subcommand() {
        for snippet in [zsh_snippet(), bash_snippet(), fish_snippet()] {
            assert!(!snippet.contains("allow)"), "{snippet}");
            assert!(!snippet.contains("'allow:"), "{snippet}");
            assert!(!snippet.contains("-a allow "), "{snippet}");
            assert!(!snippet.contains("--revoke"), "{snippet}");
            assert!(!snippet.contains("agent-setup allow"), "{snippet}");
        }
    }

    #[test]
    fn bash_snippet_never_reevaluates_project_data() {
        let bash = bash_snippet();
        // compgen -W re-expands its word list; project data must never reach it.
        assert!(!bash.contains("compgen -W"), "{bash}");
        assert!(!bash.contains("mapfile"), "bash 3.2 has no mapfile: {bash}");
        for source in ["_commands", "_services"] {
            assert!(
                bash.contains(&format!(
                    "_devy_complete_lines \"$cur\" <<< \"$(command devy {source} 2>/dev/null)\""
                )),
                "{source} must go through the literal line matcher"
            );
        }
        assert!(bash.contains("[[ $line == \"$cur\"* ]] && COMPREPLY+=(\"$line\")"));
        // An unquoted `$(compgen ...)` in an array assignment word-splits and globs.
        assert!(!bash.contains("=($(compgen"), "{bash}");
    }
}
