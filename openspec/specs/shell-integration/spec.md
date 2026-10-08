# shell-integration Specification

## Purpose
`devy hook <shell>` prints a snippet for the user's rc file. The snippet activates the shadowenv environment after `devy up` and adds tab completion for built-in subcommands and project commands.

## Requirements

### Requirement: Supported shells
The system SHALL print a shell integration snippet to stdout for `devy hook zsh`, `devy hook bash` and `devy hook fish`, exiting 0. No `devy.yml` is required. Any other shell MUST fail with `Unsupported shell '<shell>'. Supported shells: zsh, bash, fish` and a non-zero exit status.

#### Scenario: zsh snippet
- **WHEN** the user runs `devy hook zsh`
- **THEN** the zsh snippet is printed to stdout and devy exits 0

#### Scenario: Unsupported shell
- **WHEN** the user runs `devy hook powershell`
- **THEN** devy exits non-zero and stderr names `powershell`

### Requirement: Snippet uses the binary name
The system SHALL substitute the crate's binary name (`devy`) for every `{bin}` placeholder in the snippet. The output MUST NOT contain a literal `{bin}`.

#### Scenario: No placeholders remain
- **WHEN** any supported hook snippet is printed
- **THEN** the output contains no `{bin}` text

### Requirement: Wrapper activates shadowenv after up
Each snippet SHALL define a `devy` shell function that wraps the real binary. After a successful `devy up …`, when shadowenv's hook function `__shadowenv_hook` is defined, it MUST apply the new environment in the current shell by forcing a run of that hook, as `shadowenv init` does: in zsh and bash it checks the wrap (bash: runs the pre-hook), sets `__shadowenv_force_run=1` and calls `__shadowenv_hook`; in fish it re-wraps, runs `set -g __shadowenv_force_run 1` and calls `__shadowenv_hook`. The hook is the wrapped one, so the guard runs first. It returns 0, or the status of a failed `devy up`. `devy hook …` MUST print the snippet for the shell it was generated for. All other invocations are passed through unchanged.

#### Scenario: Successful up activates environment
- **WHEN** the zsh snippet is loaded, shadowenv is set up, and `devy up` succeeds
- **THEN** the wrapper sets `__shadowenv_force_run=1` and runs `__shadowenv_hook`, guard first, in the current shell

#### Scenario: Failed up does not activate
- **WHEN** `devy up` exits non-zero
- **THEN** the shadowenv hook is not run

### Requirement: Tab completion
Each snippet SHALL register completion for:
- the built-in subcommands `up`, `down`, `services`, `start`, `stop`, `restart`, `status`, `check`, `doctor`, `logs`, `ask`, `init`, `hook`, `pr`, `export`, `exec`, `agent-setup` and `prune`
- project command names taken from `devy _commands`, with errors suppressed

It MUST complete:
- `--update`, `--dry-run`, and `--bootstrap` after `up`
- `--force` after `init`
- `--volumes` after `down`
- `--yes`, `--no-ai` and `--show-context` after `doctor`
- `--format` after `export`, and `shell flake` after `export --format`
- `zsh bash fish` after `hook`
- `--follow`, `--lines`, `--explain`, and `--show-context` after `logs`, and service names taken from `devy _services`, with errors suppressed
- `--show-context` after `ask`
- `--json` after `status`, `services` and `check`
- `--force`, `--agents-md`, `--print`, `--agent` and `--all` after `agent-setup`, and `claude codex gemini cursor copilot windsurf opencode amp` after `agent-setup --agent`
- command names from the shell's own command completion after `exec`
- `--yes` and `--volumes` after `prune`

#### Scenario: Project commands appear in completion
- **WHEN** `devy.yml` defines a command `dev` and the user tab-completes `devy <TAB>`
- **THEN** the candidates include the built-ins, including `doctor`, `pr`, `export`, `logs`, `ask`, `exec`, `agent-setup` and `prune`, and `dev`

#### Scenario: Hook argument completion
- **WHEN** the user tab-completes `devy hook <TAB>`
- **THEN** the candidates are `zsh`, `bash` and `fish`

#### Scenario: Up flags
- **WHEN** the user tab-completes `devy up --<TAB>`
- **THEN** the candidates are `--update`, `--dry-run`, and `--bootstrap`

#### Scenario: Doctor flags
- **WHEN** the user tab-completes `devy doctor --<TAB>`
- **THEN** the candidates are `--yes`, `--no-ai` and `--show-context`

#### Scenario: Export format values
- **WHEN** the user tab-completes `devy export --format <TAB>`
- **THEN** the candidates are `shell` and `flake`

#### Scenario: Down flags
- **WHEN** the user tab-completes `devy down --<TAB>`
- **THEN** the candidates include `--volumes`

#### Scenario: Logs flags
- **WHEN** the user tab-completes `devy logs --<TAB>`
- **THEN** the candidates are `--follow`, `--lines`, `--explain`, and `--show-context`

#### Scenario: Logs service names
- **WHEN** `devy.yml` declares `redis` and `node`, and the user tab-completes `devy logs <TAB>`
- **THEN** the candidates include `redis` and not `node`

#### Scenario: JSON flag
- **WHEN** the user tab-completes `devy status --<TAB>`
- **THEN** the candidates include `--json`

#### Scenario: Agent setup flags
- **WHEN** the user tab-completes `devy agent-setup --<TAB>`
- **THEN** the candidates are `--force`, `--agents-md`, `--print`, `--agent` and `--all`

#### Scenario: Agent setup agent names
- **WHEN** the user tab-completes `devy agent-setup --agent <TAB>`
- **THEN** the candidates are `claude`, `codex`, `gemini`, `cursor`, `copilot`, `windsurf`, `opencode` and `amp`

#### Scenario: Allow flags
- **WHEN** the user tab-completes `devy <TAB>` or `devy allow --<TAB>`
- **THEN** `allow` is not a candidate and `--revoke` is not offered

#### Scenario: Prune flags
- **WHEN** the user tab-completes `devy prune --<TAB>`
- **THEN** the candidates are `--yes` and `--volumes`

### Requirement: Completion treats project data literally
Shell snippets SHALL treat names from `devy _commands` and `devy _services` as literal strings. They SHALL never be subject to parameter expansion, command substitution, arithmetic expansion or globbing. The bash snippet SHALL NOT pass them through `compgen -W` or any other construct that re-evaluates words. It SHALL build `COMPREPLY` by a literal prefix match over the lines read from those commands with a `while IFS= read -r` loop fed by a here-string, which works in bash 3.2 (the macOS `/bin/bash`) and in posix mode, rather than `mapfile`/`readarray`. Because bash inserts candidates unquoted, the bash snippet SHALL skip any name containing a character outside `[[:alnum:]._:+@-]` (whitespace, control characters, `$`, backticks, quotes and glob characters among them). File and program candidates for `devy exec` SHALL likewise be read from `compgen` line by line, never assigned from an unquoted `$(compgen ...)`, and `compopt -o filenames` SHALL be applied when the `compopt` builtin is available so bash quotes them on insertion; where it is not (bash 3.2), candidates containing characters outside `[[:alnum:]._/:+@,=-]` SHALL be skipped. The zsh snippet SHALL escape `:` in project command names before passing them to `_describe`, which otherwise reads text after the first `:` as a description.

#### Scenario: Substitution in a command name is not run
- **WHEN** `devy _commands` prints `$(touch /tmp/p)` (for example from an older devy or a crafted config) and the user tab-completes `devy <TAB>` in bash
- **THEN** `/tmp/p` is not created

#### Scenario: Name with shell syntax is skipped in bash
- **WHEN** `devy _commands` prints `evil$x` and `dev`, and the user tab-completes `devy <TAB>` in bash
- **THEN** the candidates include `dev` and not `evil$x`

#### Scenario: Hostile file names in exec completion
- **WHEN** the directory `files/` contains `sp ace`, `g*` and `gx`, and the user tab-completes `devy exec ls files/<TAB>` in bash
- **THEN** each name is offered as one literal candidate, never split at the space or expanded as a glob, and quoted on insertion (bash 4+)
- **AND** under bash 3.2, which cannot quote them, only `files/gx` is offered

#### Scenario: Colon in a zsh command name
- **WHEN** `devy.yml` defines a command `db:migrate` and the user tab-completes `devy <TAB>` in zsh
- **THEN** the candidate is `db:migrate`, not `db` with the description `migrate`

#### Scenario: Normal completion still works
- **WHEN** `devy.yml` defines a command `dev` and the user tab-completes `devy d<TAB>` in bash
- **THEN** the candidates include `dev`, `down` and `doctor`

### Requirement: Shadowenv guard before every shadowenv hook
`shadowenv trust` signs the `.shadowenv.d` directory, not its files, so after `devy up` any lisp a `git pull` adds there, or a `500_devy.lisp` it replaces, would be evaluated by shadowenv's own prompt hook before any devy command runs. Each snippet (bash, zsh, fish) SHALL therefore define a guard that runs before shadowenv's hook on every prompt and every command shadowenv hooks:
- The snippet SHALL set up shadowenv itself: when `devy hook` finds `shadowenv` outside the project (filesystem-safety), the snippet SHALL run that binary's `shadowenv init <shell>` by its absolute path, shell-quoted (`eval "$('<path>' init bash|zsh)"`, `'<path>' init fish | source`), after defining the guard; otherwise it SHALL print a comment saying shadowenv was not on PATH, and set nothing up. Right after the init it SHALL wrap shadowenv's hook function `__shadowenv_hook` (which shadowenv's prompt and preexec hooks call by name) so the guard runs first and the original hook runs only when the guard succeeds, and then run the guard once, so the first prompt and the first preexec are guarded. It SHALL never wrap its own wrapper, so loading the snippet twice is harmless. In bash, wrapping SHALL make the wrapper and the renamed original hook read-only (`readonly -f`), so nothing (a later `shadowenv init` in particular, wherever it runs) can replace or remove either for the rest of the shell's life, and whatever calls `__shadowenv_hook` by name, hookbook's `PROMPT_COMMAND` entry and preexec list included, runs the guard. Loaded again when the hook is already wrapped, the bash snippet SHALL skip its own `shadowenv init` (whose definition of the hook would fail), so it prints no error and adds no hookbook entry.
- A user may also run `shadowenv init` in their rc file, before or after the snippet, which redefines the hook unwrapped (in bash, only an init before the snippet can: after the wrap, an init run by absolute path makes bash report `__shadowenv_hook: readonly function` and run the rest of the init, which then adds no second `PROMPT_COMMAND` entry or `__hookbook_functions` element, while `shadowenv init bash` run through the snippet's `shadowenv` function prints only a comment; an init after a snippet made without shadowenv has its hook wrapped by the snippet's `hookbook_add_hook` as the init hooks it up; the README SHALL say these errors are expected). The snippet SHALL check the wrap again ahead of each of shadowenv's hooks, without starting a process on an ordinary prompt:
  - zsh: the wrap check SHALL head `precmd_functions` and `preexec_functions` (declared with `typeset -ga` first, so the snippet loads under `setopt nounset`), and compare the hook's text.
  - bash: after its own init, when shadowenv's hook is still not wrapped (in practice: the snippet was made without shadowenv), the snippet SHALL define (not again when it is the snippet's already) a read-only (`readonly -f`) copy of hookbook's bash `hookbook_add_hook`, with the same effect (it prepends hookbook's `PROMPT_COMMAND` entry for the function unless `PROMPT_COMMAND` contains ` <function> `, and appends the function to `__hookbook_functions` unless it is there), that first wraps shadowenv's hook when called for `__shadowenv_hook`, and SHALL keep hookbook's MIT attribution. shadowenv's init defines `__shadowenv_hook`, then hookbook (whose own definition of `hookbook_add_hook` then fails, reported as `hookbook_add_hook: readonly function`, and the rest of the init runs), then calls `hookbook_add_hook __shadowenv_hook`; a test SHALL pin the checked-in init to that order and to `__shadowenv_hook` as the only hook it adds. So a later `shadowenv init` the snippet never saw, wherever it runs (inside a function, with stderr discarded, after starship or a terminal integration moved devy's entry), has its hook wrapped before hookbook's `PROMPT_COMMAND` entry or preexec list can call it, and other users of hookbook still get their hooks added. Once the hook is wrapped the snippet SHALL NOT define that copy: the hook can't change, and while devy's `PROMPT_COMMAND` entry is in `PROMPT_COMMAND` it names ` __shadowenv_hook `, so hookbook's own `hookbook_add_hook` adds no entry for it (when starship or a terminal integration moved devy's entry, the entry hookbook adds calls the read-only wrapper); another tool that bundles hookbook and is set up after a snippet made with shadowenv SHALL define its `hookbook_add_hook` and add its hook without an error. After a snippet made without shadowenv, such a tool's definition fails (`hookbook_add_hook: readonly function`, which ends a `set -e` shell in bash 5) and its hook is added by the copy; the README SHALL say so. A snippet sourced again when that copy is defined already (shadowenv installed since) SHALL run its own init with stderr on `/dev/null` and in an `||` list, so hookbook's failing redefinition neither prints an error nor ends a `set -e` shell, and the copy wraps the hook.
  - bash: when shadowenv was found as the snippet was made (and only then, so that without shadowenv `command -v shadowenv` and `type -t shadowenv` still fail), the snippet SHALL define a `shadowenv` shell function that returns 0 and prints only the comment line `# shadowenv is already set up by devy's hook` when called as exactly `shadowenv init bash` while shadowenv's hook is wrapped, and otherwise runs `command shadowenv` with its arguments, so the usual `eval "$(shadowenv init bash)"` after the snippet neither reports the read-only error nor, under `set -e` in bash 5, ends the shell. The snippet's own init runs shadowenv by absolute path and is not affected.
  - bash and zsh: the snippet SHALL define its `devy` function, and bash's `shadowenv` function and `hookbook_add_hook` copy, as `function name {` rather than `name() {`, so an alias of the same name defined before the snippet is not expanded in its place.
  - bash: a pre-hook SHALL head hookbook's preexec list `__hookbook_functions` (created when missing, so a later `shadowenv init` keeps it first), and SHALL put itself first again whenever something went ahead of it. Its `PROMPT_COMMAND` entry SHALL come before hookbook's own `PROMPT_COMMAND` entry for shadowenv's hook (from an init before the snippet), and SHALL be moved (to the front) only when it comes after that entry, or is missing when the snippet is sourced, so other prompt hooks keep their order. The entry SHALL keep `$?` for the entries after it: it saves `$?` first and ends with a shell function that returns the saved status (no subshell); `PIPESTATUS` and `$_` are not kept for entries after it, also when it moves to the front ahead of hookbook's entry (the starship case below). The entry SHALL have no trailing `;` (bash-preexec strips trailing `;` and whitespace, which would make an exact match miss it), and its presence SHALL be detected by the fixed token `_devy_shadowenv_pre precmd __shadowenv_hook` in every element of `PROMPT_COMMAND` (`${PROMPT_COMMAND[*]}`: bash 5.1 makes it an array, as bash-preexec uses it) and in starship's copy of `PROMPT_COMMAND`, which starship's prompt hook evaluates: `STARSHIP_PROMPT_COMMAND` (starship 1.19 and later) or `_PRESERVED_PROMPT_COMMAND` (earlier). Found only in starship's copy, it SHALL NOT be added to `PROMPT_COMMAND` again while `PROMPT_COMMAND` has no hookbook entry for shadowenv's hook (`__shadowenv_hook precmd 2>&3`); when it has one (a `shadowenv init` after starship's, even inside a function or with stderr discarded, puts it ahead of starship's hook), devy's entry SHALL be moved to the front of `PROMPT_COMMAND`, whether the pre-hook runs from a copy or not. When it must move, its old copies (in `PROMPT_COMMAND` and in starship's copy) SHALL be replaced by `{ _devy_shadowenv_return; } 2>/dev/null` (stderr on `/dev/null`, so hookbook's DEBUG trap does not take it for a command), so the commands around them still parse and see the `$?` the moved entry saved earlier in the same prompt; so SHALL every copy after the first within `PROMPT_COMMAND` itself (the prompt where the second copy appears MAY run the guard twice). Only copies of the entry's exact text are replaced; another text with the token in it (an entry from an earlier build of the snippet, in a long-lived shell) MAY stay until the shell restarts. A copy of the entry that another function evaluates (a terminal integration or prompt framework that moved `PROMPT_COMMAND` into a variable of its own and evals it from its prompt hook, such as VS Code's `__vsc_original_prompt_command`, wherever that copy is kept; the pre-hook tells it apart by `FUNCNAME[1]`, set only when a function called it) SHALL leave `PROMPT_COMMAND` untouched while it has no hookbook entry for shadowenv's hook, and neither the pre-hook's prompt nor its preexec run SHALL add a missing entry then (only sourcing the snippet does, and not while starship's or VS Code's copy has it): hookbook's DEBUG trap skips the integration's hook only while `PROMPT_COMMAND` is exactly that command (VS Code sets `PROMPT_COMMAND=__vsc_prompt_cmd_original`, stderr not redirected), so an entry put ahead of it would make hookbook run shadowenv's preexec hook at every prompt. The copy SHALL do nothing (neither guard nor hook) when bash ran the entry from `PROMPT_COMMAND` directly earlier in the same prompt: that run sets a mark, which the copy clears, and which a preexec run of the pre-hook for a command (`BASH_COMMAND`) that is not one of `PROMPT_COMMAND`'s also clears (compared whole with each command of each element, split at newlines and `;`, blanks around it removed). So every prompt (an empty line included) runs the guard and shadowenv's hook once, also under a line editor such as ble.sh that attaches at a prompt and then evaluates `PROMPT_COMMAND` from a function; without hookbook's preexec hooks (ble.sh or bash-preexec replacing its DEBUG trap) the one prompt right after such an attach MAY go without a run, when bash ran the entry itself at the prompt before. Its `PROMPT_COMMAND` entry SHALL contain the text ` __shadowenv_hook ` (as an argument), which hookbook's `hookbook_add_hook` takes to mean the hook is already there, so a later `shadowenv init` adds no `PROMPT_COMMAND` entry ahead of devy's, even when that init runs inside a function or with stderr discarded (where hookbook's DEBUG trap does not run devy's pre-hook first). The pre-hook SHALL wrap the hook when it was never wrapped (a `shadowenv init` of the user's after a snippet made without shadowenv, which the snippet's `hookbook_add_hook` below normally wraps first); once wrapped the hook cannot change, so reading its definition, which takes a subshell, happens once per source of the snippet (and once more when such an init runs), never on an ordinary prompt. Then it SHALL run the guard. On a prompt it SHALL itself run `__shadowenv_hook precmd` when the guard succeeds and hookbook has no `PROMPT_COMMAND` entry of its own (an init before the snippet leaves one, which then runs after devy's, and the wrapper runs the guard a second time for it, since other prompt hooks may run in between). So that the guard otherwise runs once per prompt and per command, the pre-hook SHALL tell the wrapper that the guard just passed only for the hook run it starts itself, or, before a command, when shadowenv's hook is the next entry of `__hookbook_functions`; the wrapper SHALL consume that and otherwise run the guard, so a direct call of the hook is always guarded.
  - fish, whose event handlers have no promised order: the guard's function SHALL take the place of `__shadowenv_hook` (with the same `fish_prompt` and `PWD` events) and call a copy of the original only when the guard succeeds, and a handler on the same events SHALL, when the hook is not wrapped, run the guard and re-wrap. Re-wrapping removes shadowenv's own handler, which fish then skips in that event, and the new wrapper's handler fires only from the next event, so when the guard passed that handler SHALL run the copy of shadowenv's hook itself, unless shadowenv's handler already ran in that event (it clears the `__shadowenv_force_run` that `shadowenv init` sets), so the hook runs once per event.
- The guard SHALL find the `.shadowenv.d` shadowenv loads: the closest directory named `.shadowenv.d` at or above the physical working directory (what getcwd returns, as shadowenv uses), never derived from `$PWD`. bash and zsh SHALL walk `.`, `..`, `../..` (each step resolved by the kernel) up to the directory that is `-ef` its `..`, at most 256 levels; a walk that cannot finish (too deep, or a `..` that cannot be read) SHALL fail the guard. fish SHALL use the system `pwd -P`, and MAY keep its answer only while `test . -ef <answer>` succeeds (two stat calls, no process: a renamed ancestor, which fish's `$PWD` does not follow, or a `cd` elsewhere invalidates it; a fish before 3.6, which has no `test -ef`, runs `pwd -P` at every guard), SHALL never keep an answer it rejected, SHALL read its output whole (`string collect -N`, not split at newlines) and strip the one trailing newline with `string replace -r` (not `string sub -e`, which fish before 3.2 lacks), and SHALL fail the guard unless that output is exactly one absolute path followed by one newline, with no other newline in it; it SHALL NOT fall back to fish's builtin `pwd`. (A command substitution would split a directory named `x<newline>y` into two words, which joined would steer the guard to a decoy `x y/.shadowenv.d`.)
- The guard SHALL fail, whatever shadowenv's trust state, when the closest `.shadowenv.d` is not owned by the current user (`-O`): shadowenv's trust in a directory another user owns is never devy's, and that user could swap what the guard checks (a `.error-*` entry, a signature) between the check and shadowenv's hook. This check SHALL come first, before any file in the directory is looked at, removed or written. It also means shadowenv does not run for root in a checkout a regular user owns (a dev container running as root over a mounted checkout), nor in a checkout shared by a group where another member owns `.shadowenv.d`; the README SHALL say so.
- The guard SHALL fail, whatever shadowenv's trust state, when the closest `.shadowenv.d` is a symlink (`-L`), which a repository can commit: shadowenv follows it, so it would load the directory it points to (another project's trusted environment, for example) and delete stale `.error-*` files and write its own there. This check SHALL come right after the owner check (which, following the link, already fails for a target another user owns), before any file in the directory is looked at, removed or written.
- The guard SHALL fail, whatever shadowenv's trust state, when the closest `.shadowenv.d` holds a `.error-*` entry that is a symlink or not a regular file: shadowenv's hook creates or truncates `.error-<n>-<shell pid>` there, following symlinks, also in a directory it does not trust (an upstream shadowenv behavior devy works around). This check SHALL come before any other but the owner and symlink checks and start no process.
- When the directory holds a `.trust-*` file, the guard SHALL remove every `.trust-*` file (not directories) in it when the directory holds a symlink or any other non-regular entry, a `*.lisp` file other than `500_devy.lisp` (matched case-insensitively, dotfiles such as `.trust-x.lisp` included), or a `500_devy.lisp` that is not devy's. A `500_devy.lisp` is not devy's when its first line is not `; devy-env <32 lowercase hex digits>`, or the file is not byte for byte the copy `<state>/devy/shadowenv/<nonce>.lisp` (shell-environment) that line names. `<state>` is `$XDG_STATE_HOME` when absolute, else `$HOME/.local/state` when `$HOME` is absolute; with neither, nothing is devy's. The copy must be a regular file, not a symlink, owned by the user. A file containing a NUL byte is never devy's. A copy directory physically at or below the project directory is never trusted to hold copies (judged by the same `..` walk in bash and zsh, and by a literal prefix test on the resolved path in fish), with no exception for a project at `$HOME`. bash and zsh MAY keep the content of a copy that matched, keyed by its path, since devy writes each copy once, atomically. Other regular files (shadowenv's `.gitignore`, `.trust-*` and the `.error-*` files its hook leaves) SHALL NOT remove trust. When it removes trust, it SHALL print one line to stderr: `devy: .shadowenv.d holds files devy did not write, so shadowenv's trust was removed; run devy up`. `devy up` then rewrites the file and runs `shadowenv trust` again, or refuses, as shell-environment says.
- Removing trust SHALL use `zf_rm` in zsh when available, then `rm` by absolute path (`/bin/rm`, `/usr/bin/rm`, `/run/current-system/sw/bin/rm`), then `command -p rm` (bash and zsh); fish SHALL use only the absolute paths. In zsh, a signature none of them removed SHALL be emptied (shadowenv 3.4.0 rejects an empty signature: it aborts before loading anything) only when it is a regular file, not a symlink, owned by the user, in a `.shadowenv.d` owned by the user, with a link count of 1 (read with zsh/stat's `zstat`, no subprocess; without that module it is not emptied), since emptying a hard link would empty the file it shares. bash and fish, which cannot count links without a subprocess, SHALL NOT empty it. When any `.trust-*` file is still there afterwards, the guard SHALL fail.
- When the guard fails, shadowenv's hook SHALL NOT run, and the guard SHALL print one short line to stderr saying why, once per directory and reason, until a guard succeeds.
- The guard SHALL run no devy process, never evaluate file content, quote every expansion, read only the first line and, when that names an existing copy, the two files, and work in bash 3.2, zsh and fish 3.1 or later. It SHALL run only when shadowenv's hook `__shadowenv_hook` is defined (the snippet's load-time run and bash's pre-hook check first), so a shell without shadowenv sees no walk and no notice; a snippet made without shadowenv followed by the user's own `shadowenv init` is guarded from the first prompt. In bash it SHALL work whatever the user's `set -f`, `failglob`, `nocasematch`, `dotglob` and `GLOBIGNORE` settings, restoring them afterwards. It SHALL start a process only to remove trust and, in fish, to compare the files (`cmp`) and to read the physical directory (`pwd -P`), taking the fish utilities only from `/bin`, `/usr/bin` or `/run/current-system/sw/bin` (failing closed when none has them, never searching PATH), so a PATH the project's environment set cannot substitute them. Every process it starts SHALL get the dynamic loader's variables `LD_PRELOAD`, `LD_LIBRARY_PATH`, `LD_AUDIT`, `GCONV_PATH`, `LOCPATH`, `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH`, `DYLD_FRAMEWORK_PATH`, `DYLD_FALLBACK_LIBRARY_PATH`, `DYLD_FALLBACK_FRAMEWORK_PATH`, `DYLD_VERSIONED_LIBRARY_PATH`, `DYLD_VERSIONED_FRAMEWORK_PATH` and `DYLD_ROOT_PATH` empty, without starting another process (prefix assignments on `command` in bash and zsh, function-local `set -lx` in fish), since the project's environment may set them. The environment keys that could hide those programs or stop the guard's functions from running (`EXECIGNORE`, `FUNCNEST`), change how it parses or globs (`IFS`, `GLOBIGNORE`, `BASH_COMPAT`, `POSIXLY_CORRECT`), and every other shell variable that would make the running shell run code or act on files (the prompt strings, the mail check, hook arrays, autoload paths, history files), are refused at load (project-config).
- The guard's state (bash and zsh: the copy cache and the last notice; bash also the prompt bookkeeping below; fish: the cached physical directory and last notice) SHALL be cleared when the snippet loads and SHALL NOT be exported (bash `export -n`, zsh `typeset +x`, fish `set -gu` after erasing the inherited global), so values inherited from the environment cannot steer the guard, and the guard's values are not passed to child processes.

The guard closes the window in which shadowenv evaluates pulled lisp before devy runs, for shells that load the snippet. One gap remains when the user also keeps a separate `shadowenv init`: in bash, one before the snippet lets hookbook's preexec run shadowenv's hook for the rc lines up to and including the snippet's own. In fish, with one after the snippet, the guard runs before shadowenv's hook only because fish (4.9.3) runs devy's pre-hook, defined first, before shadowenv's handler in the next event; fish does not promise that order, and a fish that ran shadowenv's handler first would run it once unguarded in that event. In bash, a snippet made without shadowenv on PATH relies on the user's later `shadowenv init` hooking shadowenv's hook up through `hookbook_add_hook` right after defining it (as shadowenv's init does, pinned by a test against the checked-in init) to wrap it before its first run; that init reports `hookbook_add_hook: readonly function` (and, under `set -e` in bash 5, ends the shell). The wrapped hook keeps the shadowenv path of the init that defined it, so the README SHALL tell users to open a new shell after installing or upgrading shadowenv. In bash, a `PROMPT_COMMAND` assigned after the snippet drops devy's entry, which is not restored (the guard then runs before commands only, and shadowenv applies changes only then); the README SHALL say to put the hook's line after such assignments. In bash, a hook that hookbook runs before a command is not guarded again when another hookbook preexec function placed between devy's and shadowenv's changes directory (devy only skips the second check when shadowenv's hook directly follows its own). The README SHALL tell users to replace their `shadowenv init` with devy's hook.

#### Scenario: Pulled lisp loses shadowenv's trust before shadowenv loads it
- **WHEN** `devy up` ran, a pull adds `.shadowenv.d/000_evil.lisp`, and the next prompt runs shadowenv's hook
- **THEN** the guard first removes `.shadowenv.d/.trust-*` and prints the one-line notice, so shadowenv refuses to load the directory

#### Scenario: Replaced environment file
- **WHEN** a pull replaces `.shadowenv.d/500_devy.lisp`, keeping its first line, with other content
- **THEN** the guard removes shadowenv's trust at the next prompt

#### Scenario: Devy's own setup keeps trust
- **WHEN** `.shadowenv.d` holds only `500_devy.lisp` as devy wrote it, `.gitignore`, `.trust-<fingerprint>` and `.error-0-<pid>`
- **THEN** the guard leaves the trust files in place and prints nothing

#### Scenario: First prompt is guarded
- **WHEN** shadowenv is installed, the bash snippet is loaded, and `.shadowenv.d` holds planted lisp
- **THEN** no run of shadowenv's hook, from hookbook's preexec or from the first `PROMPT_COMMAND`, happens while the trust file exists

#### Scenario: shadowenv set up again after the snippet
- **WHEN** the rc file runs `eval "$(shadowenv init zsh)"` after `eval "$(devy hook zsh)"`, or the fish equivalent, at the top level or inside a function
- **THEN** the next prompt runs the guard first and wraps the redefined hook again

#### Scenario: bash shadowenv init after the snippet
- **WHEN** a bash rc file or prompt runs `eval "$(shadowenv init bash)"` after `eval "$(devy hook bash)"`, also inside a function or with stderr discarded, under VS Code's shell integration, starship, or a replaced `PROMPT_COMMAND`
- **THEN** run by absolute path, bash reports `__shadowenv_hook: readonly function`; run as `shadowenv init bash` through PATH, it prints only a comment and bash reports nothing; either way the wrapper stays in place, `PROMPT_COMMAND` and `__hookbook_functions` gain no second entry for shadowenv, and every later run of shadowenv's hook comes after the guard

#### Scenario: bash snippet sourced again
- **WHEN** an interactive bash under VS Code's shell integration sources the snippet a second time
- **THEN** it prints no error, does not run `shadowenv init` again, and each prompt and command runs shadowenv's hook as often as before

#### Scenario: Directory reached through a retargeted symlink
- **WHEN** a zsh user `cd`s into the project through a symlink that is then pointed elsewhere, or `PWD` is set to another directory
- **THEN** the guard still checks the project's `.shadowenv.d`

#### Scenario: Symlinked shadowenv error file
- **WHEN** `.shadowenv.d` holds `.error-0-4242` as a symlink to a file outside the project, trusted by shadowenv or not
- **THEN** shadowenv's hook does not run, the file is untouched, and the guard prints one line saying the `.error-*` entry is not a regular file

#### Scenario: Trust that cannot be removed
- **WHEN** `.shadowenv.d` holds planted lisp and a `.trust-*` file that `rm` cannot remove
- **THEN** the guard prints one line saying it could not remove shadowenv's trust, and shadowenv's hook does not run; zsh also empties the signature, and bash and fish leave it as it is

#### Scenario: Hard-linked signature is not emptied
- **WHEN** in zsh, `.shadowenv.d` holds planted lisp and a `.trust-*` file that `rm` cannot remove and that has a second hard link elsewhere
- **THEN** the other link's content is unchanged, and shadowenv's hook does not run

#### Scenario: .shadowenv.d owned by another user
- **WHEN** the closest `.shadowenv.d` (here a symlink to a directory owned by root) is not owned by the current user
- **THEN** shadowenv's hook does not run, and the guard prints one line saying `.shadowenv.d` is not owned by the user

#### Scenario: Newline in the directory name (fish)
- **WHEN** a fish user is in a directory named `x<newline>y` below a project shadowenv trusts, with planted lisp, next to a decoy `x y/.shadowenv.d`
- **THEN** the guard fails with a line saying it could not read the working directory, and shadowenv's hook does not run

#### Scenario: Symlinked .shadowenv.d
- **WHEN** a repository commits `.shadowenv.d` as a symlink to another project's trusted `.shadowenv.d`, and the user is below it in bash, zsh or fish
- **THEN** shadowenv's hook does not run, nothing in the target directory changes (its signature, lisp and `.error-*` files stay), and the guard prints one line saying `.shadowenv.d` is a symbolic link

#### Scenario: Renamed ancestor (fish)
- **WHEN** fish ran the guard in a directory, and then an ancestor of it is renamed and a decoy `.shadowenv.d` is created at the old path
- **THEN** the next guard judges the directory the shell is really in, not the decoy

#### Scenario: No shadowenv
- **WHEN** the snippet was made without shadowenv on PATH and shadowenv is never set up, in a directory whose `.shadowenv.d` the guard would reject
- **THEN** loading the snippet and running its prompt and preexec hooks prints nothing

#### Scenario: Prompt frameworks and bash-preexec (bash)
- **WHEN** in an interactive bash, starship moves `PROMPT_COMMAND` into `STARSHIP_PROMPT_COMMAND` (or, before starship 1.19, `_PRESERVED_PROMPT_COMMAND`), or keeps it under both names (evaluating one), or bash-preexec strips its trailing `;` (and, from bash 5.1, makes it an array), or devy's entry appears twice in `PROMPT_COMMAND`, or VS Code's shell integration (or another terminal integration) moves `PROMPT_COMMAND` into a variable of its own, sets `PROMPT_COMMAND` to its prompt hook alone (stderr not redirected) and evals the saved value from that hook, or a line editor attaches at the first prompt and from then on evaluates `PROMPT_COMMAND` from a function
- **THEN** from the first command on, each prompt (an empty line's included) runs the guard once and shadowenv's precmd hook once, and each command runs the guard once and shadowenv's preexec hook once, as without them (the prompt where a second copy of devy's entry appears MAY run them twice)

#### Scenario: shadowenv init in a function after starship (bash)
- **WHEN** in an interactive bash (bash 3.2 and 5.x), starship moved devy's entry into `STARSHIP_PROMPT_COMMAND` (or `_PRESERVED_PROMPT_COMMAND`), and a function that runs `eval "$(shadowenv init bash)"` is called from the rc file and again at a prompt, with a signature and a planted lisp file written right before it on the same line, and stderr either a pipe or `/dev/null` (where hookbook's DEBUG trap runs no preexec hooks)
- **THEN** no run of shadowenv's hook happens while the signature and the planted file are both there, every prompt runs shadowenv's hook after the guard, and a prompt hook after devy's entry in starship's copy sees the last command's `$?` although devy's entry moved to the front of `PROMPT_COMMAND` and hookbook's entry ran in between

#### Scenario: Empty line (bash)
- **WHEN** in an interactive bash with devy's single `PROMPT_COMMAND` entry and hookbook's preexec hooks running, the user enters an empty line between two commands
- **THEN** the prompt after the empty line runs the guard and shadowenv's hook, like the prompt before it

#### Scenario: Guard utilities without loader injection
- **WHEN** the project's environment sets `LD_PRELOAD`, `LD_AUDIT`, `GCONV_PATH` or another of the dynamic loader's variables
- **THEN** the utilities the guard runs (bash's and zsh's `rm`, fish's `cmp`, `pwd` and `rm`) see them empty, and the shell keeps its own values

#### Scenario: Other prompt hooks keep `$?` (bash)
- **WHEN** `PROMPT_COMMAND` has an entry that prints `$?`, set before the snippet or added before or after devy's entry afterwards, and the last command failed with status 1
- **THEN** that entry prints `status=1` at every prompt, and an entry added ahead of devy's stays ahead

#### Scenario: shadowenv set up again after the snippet (fish)
- **WHEN** the fish config runs `shadowenv init fish | source` after devy's hook
- **THEN** at the next prompt (or `cd`) shadowenv's hook runs exactly once, after the guard, and from then on through devy's wrapper

#### Scenario: Inherited guard state
- **WHEN** the environment exports devy's guard cache variables with forged values (a copy cache matching a replaced `500_devy.lisp`, or a fish physical directory of `/`)
- **THEN** the snippet clears them, the guard still removes shadowenv's trust, and a child process does not see them

#### Scenario: shadowenv set up after a snippet made without it
- **WHEN** `devy hook bash` ran without shadowenv on PATH, and the rc file runs `eval "$(shadowenv init bash)"` after it (also inside a function or with `2>/dev/null`)
- **THEN** `PROMPT_COMMAND` starts with devy's entry and has no hookbook entry for shadowenv, and the first prompt runs the guard before shadowenv's hook

#### Scenario: shadowenv set up in a function after a snippet made without it, under starship
- **WHEN** `devy hook bash` ran without shadowenv on PATH, starship moved `PROMPT_COMMAND` into its copy, and a function then runs `eval "$(shadowenv init bash)"` (also with stderr discarded), so hookbook adds its own `PROMPT_COMMAND` entry ahead of starship's hook and hookbook's DEBUG trap does not see the init
- **THEN** the snippet's `hookbook_add_hook` wraps shadowenv's hook before hookbook's entry is added, and no run of shadowenv's hook, at any prompt or command, comes without the guard before it

#### Scenario: Another hookbook user after the snippet (bash)
- **WHEN** `devy hook bash` ran with shadowenv on PATH, and the rc file then sets up another tool that bundles hookbook and adds its own hook, under `set -e`
- **THEN** bash reports no error, the shell goes on, and the tool's hook is added to `PROMPT_COMMAND` and `__hookbook_functions` after devy's and shadowenv's

#### Scenario: No shadowenv function without shadowenv (bash)
- **WHEN** `devy hook bash` ran without shadowenv on PATH, and the snippet is loaded
- **THEN** `type -t shadowenv` prints nothing and `command -v shadowenv` fails while shadowenv is not on PATH

#### Scenario: Snippet sourced again after installing shadowenv (bash)
- **WHEN** a snippet made without shadowenv was loaded, and then one made with shadowenv is sourced, under `set -e`
- **THEN** bash reports no error, the shell goes on, and shadowenv's hook is wrapped and hooked up once

#### Scenario: Alias named like a snippet function
- **WHEN** the rc file defines an alias `devy` (and in bash `shadowenv` or `hookbook_add_hook`) before loading the snippet
- **THEN** the snippet loads without a syntax error and defines its functions
