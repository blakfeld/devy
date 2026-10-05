//! Validation rules for values read from untrusted project files (devy.yml, devy.lock).
//!
//! Every value that can reach a process argument, a file path or an environment
//! variable name is checked here when the file is loaded, so a hostile config is
//! rejected the same way by every command (`check`, `up`, `_commands`, …).

use regex::Regex;
use std::path::{Component, Path};
use std::sync::LazyLock;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static validation regex")
}

static DEP_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9][A-Za-z0-9._+@:-]*$"));
static VERSION: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9][A-Za-z0-9._+~:-]*$"));
static LIST_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| re(r"^[A-Za-z0-9@][A-Za-z0-9._+@/:=^~<>*-]*$"));
static COMMAND_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9][A-Za-z0-9_.:-]*$"));
static TOOLCHAIN: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9][A-Za-z0-9._-]*$"));
static ENV_KEY: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z_][A-Za-z0-9_]*$"));
static IMAGE_DIGEST: LazyLock<Regex> =
    LazyLock::new(|| re(r"^[A-Za-z0-9][A-Za-z0-9._/:-]*@sha256:[0-9a-f]{64}$"));

/// A dependency name: never option-like, never a path, never a local package file
/// (`.deb` for apt, `.rb`/`.json` formula or cask files and `.bottle.` / `.tar.gz` /
/// `.tgz` bottles for Homebrew).
pub fn dep_name(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    DEP_NAME.is_match(s)
        && ![".deb", ".rb", ".json", ".tar.gz", ".tgz"]
            .iter()
            .any(|ext| lower.ends_with(ext))
        && !lower.contains(".bottle.")
}

/// A rustup toolchain name (`stable`, `nightly-2024-01-01`, `1.78.0`,
/// `stable-x86_64-unknown-linux-gnu`): never option-like, never a path.
pub fn toolchain(s: &str) -> bool {
    TOOLCHAIN.is_match(s)
}

/// A version string: no shell metacharacters, no `/`, no `..`.
pub fn version(s: &str) -> bool {
    VERSION.is_match(s) && !s.contains("..")
}

/// An entry of a list passed to a tool as arguments (npm packages, rustup targets and
/// components, gcloud components): no options, URLs or local paths, and no npm spec that
/// installs from somewhere other than the registry (`file:../x`, `github:o/r`, `o/r`,
/// `user@host:o/r`, `name@file:...`). A `/` is only allowed after a leading `@scope`, and
/// the only `:` allowed is an `npm:` alias (`name@npm:other@1`).
pub fn list_entry(s: &str) -> bool {
    LIST_ENTRY.is_match(s) && registry_spec(s)
}

/// `[@scope/]name[@range]` or `[@scope/]name@npm:[@scope/]name[@range]`, where a range
/// has no `/`, `:` or `@`.
fn registry_spec(s: &str) -> bool {
    let (name, spec) = split_package(s);
    if !package_name(name) {
        return false;
    }
    match spec {
        None => true,
        Some(spec) => match spec.strip_prefix("npm:") {
            Some(target) => {
                let (name, range) = split_package(target);
                package_name(name) && range.is_none_or(plain_range)
            }
            None => plain_range(spec),
        },
    }
}

/// Splits `[@scope/]name[@spec]` at the `@` that ends the name.
fn split_package(s: &str) -> (&str, Option<&str>) {
    let from = usize::from(s.starts_with('@'));
    match s[from..].find('@') {
        Some(i) => (&s[..from + i], Some(&s[from + i + 1..])),
        None => (s, None),
    }
}

/// npm reads a name or range ending in a tarball suffix as a local file spec.
fn tarball(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    [".tgz", ".tar", ".tar.gz"]
        .iter()
        .any(|ext| s.ends_with(ext))
}

fn package_name(name: &str) -> bool {
    let bare = match name.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, rest)) if !scope.is_empty() => rest,
            _ => return false,
        },
        None => name,
    };
    !bare.is_empty() && !bare.starts_with('.') && !bare.contains(['/', ':', '@']) && !tarball(bare)
}

fn plain_range(range: &str) -> bool {
    // npm reads a spec starting with `.` as a local directory (`x@.evil`, `x@..`).
    !range.is_empty()
        && !range.starts_with('.')
        && !range.contains(['/', ':', '@'])
        && !tarball(range)
}

/// A project command name, as typed after `devy` and listed by `devy _commands`.
pub fn command_name(s: &str) -> bool {
    COMMAND_NAME.is_match(s)
}

/// A relative path that stays inside the directory it is joined to. Checked lexically:
/// absolute paths, Windows drive or UNC prefixes, and `..` components that climb above
/// the base are rejected.
pub fn rel_path_inside(s: &str) -> bool {
    // `:` would start a drive prefix (`C:foo`) on Windows; reject it everywhere so the
    // rule is the same on every platform.
    if s.chars().any(char::is_control) || s.contains(':') {
        return false;
    }
    let mut depth: usize = 0;
    for component in Path::new(s).components() {
        match component {
            Component::Normal(name) if !portable_segment(name) => return false,
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    // `Path::components` treats `/` as the only separator on Unix; a `\` there is part
    // of a file name, but on Windows it would be a separator, so reject it everywhere
    // to keep the rule platform-independent.
    !s.contains('\\')
}

/// A path segment Windows uses as written: no trailing `.` or space (which Win32 trims,
/// so `.. ` would become `..`) and no reserved device name (`CON`, `NUL`, `COM1`, ...,
/// with or without an extension). Checked on every platform to keep the rule the same.
fn portable_segment(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if name.ends_with(['.', ' ']) {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit());
    !reserved
}

/// An `environment` key: a portable shell variable name.
pub fn env_key(s: &str) -> bool {
    ENV_KEY.is_match(s)
}

/// Why an `environment` key is refused although it is a valid name. shadowenv exports
/// every entry into the running interactive shell, so a key is reserved when assigning
/// it there would make that shell run code or act on files by itself, change how it
/// parses or runs every command, or defeat the shell hook (`devy hook`) and shadowenv.
/// The list comes from the variables bash ("Bourne Shell Variables" and "Bash
/// Variables"), zsh (zshparam's "Parameters Used By The Shell", and its hook arrays)
/// and fish ("Special variables", and the variables fish's own functions watch)
/// document, classified as:
///
/// - the working directory (`PWD`, `OLDPWD`), which would hide the project from the
///   hook's guard, and devy's state directory (`HOME`, `XDG_STATE_HOME`), which would
///   point it at copies the repository wrote;
/// - parsing (`IFS`, `SHELLOPTS`, `BASHOPTS`, `BASH_COMPAT`, `POSIXLY_CORRECT`,
///   `GLOBIGNORE`, `KEYBOARD_HACK`, `histchars` in bash and zsh, zsh's `HISTCHARS`),
///   which changes how every command is read or expanded (`GLOBIGNORE` also turns on
///   `dotglob`, so `rm *` reaches dotfiles; `histchars` sets the history expansion and
///   comment characters);
/// - the prompt hooks (`PROMPT_COMMAND`, `precmd_functions`, `preexec_functions`, and
///   starship's copy of `PROMPT_COMMAND`: `STARSHIP_PROMPT_COMMAND`, or
///   `_PRESERVED_PROMPT_COMMAND` before starship 1.19) and the other functions the
///   shell calls by name (zsh's `chpwd_functions`, `periodic_functions`,
///   `zshaddhistory_functions`, `zshexit_functions`, `zsh_directory_name_functions`;
///   fish's `fish_key_bindings`);
/// - strings the shell expands, running the command substitutions in them: the prompts
///   (`PS0` to `PS4`, `PROMPT`, `PROMPT2` to `PROMPT4`, zsh's `prompt`, `RPROMPT`,
///   `RPROMPT2`, `RPS1`, `RPS2`, `SPROMPT`, `PROMPT_EOL_MARK`), the mail messages
///   (`MAILPATH`, zsh's `mailpath`) and bash's `$"..."` translations (`TEXTDOMAIN`,
///   `TEXTDOMAINDIR`); also the rest of the mail check (`MAIL`, `MAILCHECK`: the shell
///   checks the files they set before a prompt);
/// - which commands and functions run (`EXECIGNORE`, `FUNCNEST`, `BASH_ALIASES`,
///   `BASH_CMDS`, bash's `auto_resume`, which turns a command line into resuming a
///   stopped job, zsh's `NULLCMD` and `READNULLCMD`) and where code is loaded from
///   (`fish_function_path`, `fish_complete_path`, `FPATH`, `fpath`, zsh's
///   `module_path` and `MODULE_PATH`, `BASH_LOADABLES_PATH`);
/// - files the shell reads or runs by itself (`ZDOTDIR`: a zsh login shell sources
///   `$ZDOTDIR/.zlogout` when it exits), and `TMOUT`, which makes the shell exit by
///   itself;
/// - numbers the shell evaluates as arithmetic, where an array subscript in the value
///   (`PATH[$(cmd)]`) runs its command substitution: zsh's integer specials (`SHLVL`,
///   `LINES`, `COLUMNS`, `ZLE_RPROMPT_INDENT`, `LINENO`, `OPTIND`, `TRY_BLOCK_ERROR`,
///   `TRY_BLOCK_INTERRUPT`, `RANDOM`, `SECONDS`, `ERRNO`, `KEYTIMEOUT`, `LISTMAX`, and
///   `UID`, `EUID`, `GID`, `EGID`, whose value is evaluated before zsh tries to set the
///   id) at assignment, the numbers it reads with `getiparam` (`PERIOD`, `DIRSTACKSIZE`,
///   `LOGCHECK`, `BAUD`, `REPORTTIME`, `REPORTMEMORY`, `MENUSCROLL`) when it uses
///   them, and bash's integer variables (`OPTIND`, `HISTCMD`, `RANDOM`, `SRANDOM`, and
///   `MAILCHECK`, which an interactive bash declares integer) at assignment. bash's
///   `SECONDS` is declared integer too, but its assignment (bash 3.2 and 5.3) does not
///   run a subscript's command substitution; it is reserved anyway, as zsh's. fish does
///   no arithmetic on assignment. (`HISTSIZE`, `SAVEHIST`, `MAILCHECK`, `FUNCNEST` and
///   `TMOUT`, also numbers zsh evaluates, are reserved for their other effects too.) An
///   integer variable the user's own rc file or a
///   module declares (`typeset -i`, `declare -i`) is evaluated the same way at
///   assignment; devy can't know those names, so they can't be reserved;
/// - files the shell writes or truncates and descriptors it closes (`HISTFILE`,
///   `HISTFILESIZE`, `HISTSIZE`, `SAVEHIST`, `fish_history`, `BASH_XTRACEFD`,
///   `TMPPREFIX`, `TMPSUFFIX`);
/// - zsh's special tables of the shell's own state (the `zsh/parameter` and
///   `zsh/zleparameter` modules: `functions`, `aliases`, `options`, `history`,
///   `widgets`, `funcstack` and the like), which a scalar assignment would clobber and
///   some of which hold code;
/// - state kept by the hook and shadowenv (`_devy_*`, `__devy_*`, `__shadowenv_*`,
///   `__hookbook_*`) or by fish (`__fish_*`: its theme hook sources a file under
///   `$__fish_config_dir`), and bash's exported functions (`BASH_FUNC_*`).
///
/// Variables that only change the programs the shell starts, or shells started later,
/// stay allowed (like every entry, they appear in the executable entry listing for AI
/// init/doctor review): `PATH` (and zsh's `path`, fish's `fish_user_paths`), `CDPATH`, `BASH_ENV`, `ENV`, `INPUTRC`, the
/// dynamic loader's variables (`LD_*`, `DYLD_*`; the guard empties the ones that load
/// code, `LD_PRELOAD`, `LD_AUDIT`, `DYLD_INSERT_LIBRARIES` and the like, for its
/// utilities), `TMPDIR`, the locale, and settings with no such effect such as
/// `FIGNORE`, `HISTIGNORE`, `POSTEDIT` (printed, not expanded), `STTY` (terminal
/// modes) or `WORDCHARS`. Names are compared case-sensitively and reserved only in
/// the case a shell uses them (`histchars` and `HISTCHARS` both, but not `ps1` or
/// `pwd`). `None` for any key that is not reserved.
pub fn reserved_env_key(key: &str) -> Option<&'static str> {
    const NAMES: &[(&str, &[&str])] = &[
        (
            "the shell keeps it for the working directory",
            &["PWD", "OLDPWD"],
        ),
        (
            "the shell hook reads devy's state directory from it",
            &["HOME", "XDG_STATE_HOME"],
        ),
        (
            "it changes how the shell parses every command",
            &[
                "IFS",
                "SHELLOPTS",
                "BASHOPTS",
                "BASH_COMPAT",
                "POSIXLY_CORRECT",
                "GLOBIGNORE",
                "KEYBOARD_HACK",
                "histchars",
                "HISTCHARS",
            ],
        ),
        (
            "it would replace the shell's prompt hooks",
            &[
                "PROMPT_COMMAND",
                "precmd_functions",
                "preexec_functions",
                "STARSHIP_PROMPT_COMMAND",
                "_PRESERVED_PROMPT_COMMAND",
            ],
        ),
        (
            "the shell calls the functions it names",
            &[
                "chpwd_functions",
                "periodic_functions",
                "zshaddhistory_functions",
                "zshexit_functions",
                "zsh_directory_name_functions",
                "fish_key_bindings",
            ],
        ),
        (
            "the shell expands it, running the commands in it, whenever it prints a prompt",
            &[
                "PS0",
                "PS1",
                "PS2",
                "PS3",
                "PS4",
                "PROMPT",
                "PROMPT2",
                "PROMPT3",
                "PROMPT4",
                "prompt",
                "RPROMPT",
                "RPROMPT2",
                "RPS1",
                "RPS2",
                "SPROMPT",
                "PROMPT_EOL_MARK",
            ],
        ),
        (
            "the shell checks the mail files it names before a prompt and expands their messages, running the commands in them",
            &["MAILPATH", "mailpath"],
        ),
        (
            "the shell checks the mail files it sets before a prompt",
            &["MAIL", "MAILCHECK"],
        ),
        (
            "bash reads translations of $\"...\" strings from it and expands them, running the commands in them",
            &["TEXTDOMAIN", "TEXTDOMAINDIR"],
        ),
        (
            "it changes which commands and functions the shell runs",
            &[
                "EXECIGNORE",
                "FUNCNEST",
                "BASH_ALIASES",
                "BASH_CMDS",
                "auto_resume",
                "NULLCMD",
                "READNULLCMD",
            ],
        ),
        (
            "the shell loads functions or modules from it",
            &[
                "fish_function_path",
                "fish_complete_path",
                "FPATH",
                "fpath",
                "module_path",
                "MODULE_PATH",
                "BASH_LOADABLES_PATH",
            ],
        ),
        (
            "it changes which files the shell writes, truncates or closes",
            &[
                "HISTFILE",
                "HISTFILESIZE",
                "HISTSIZE",
                "SAVEHIST",
                "fish_history",
                "BASH_XTRACEFD",
                "TMPPREFIX",
                "TMPSUFFIX",
            ],
        ),
        (
            "the shell reads or runs files from it by itself",
            &["ZDOTDIR"],
        ),
        ("it makes the shell exit by itself", &["TMOUT"]),
        (
            "the shell evaluates its value as arithmetic, which runs the commands in an array subscript in it",
            &[
                "SHLVL",
                "LINES",
                "COLUMNS",
                "ZLE_RPROMPT_INDENT",
                "LINENO",
                "OPTIND",
                "TRY_BLOCK_ERROR",
                "TRY_BLOCK_INTERRUPT",
                "RANDOM",
                "SRANDOM",
                "SECONDS",
                "ERRNO",
                "HISTCMD",
                "KEYTIMEOUT",
                "PERIOD",
                "DIRSTACKSIZE",
                "LISTMAX",
                "LOGCHECK",
                "BAUD",
                "REPORTTIME",
                "REPORTMEMORY",
                "MENUSCROLL",
                "UID",
                "EUID",
                "GID",
                "EGID",
            ],
        ),
        (
            "zsh keeps its own functions, aliases, options or history in it",
            &[
                "options",
                "commands",
                "functions",
                "dis_functions",
                "functions_source",
                "dis_functions_source",
                "builtins",
                "dis_builtins",
                "reswords",
                "dis_reswords",
                "patchars",
                "dis_patchars",
                "aliases",
                "dis_aliases",
                "galiases",
                "dis_galiases",
                "saliases",
                "dis_saliases",
                "parameters",
                "modules",
                "dirstack",
                "history",
                "historywords",
                "jobdirs",
                "jobtexts",
                "jobstates",
                "nameddirs",
                "userdirs",
                "usergroups",
                "funcfiletrace",
                "funcsourcetrace",
                "funcstack",
                "functrace",
                "keymaps",
                "widgets",
            ],
        ),
    ];
    const PREFIXES: &[(&str, &[&str])] = &[
        (
            "devy's shell hook and shadowenv keep their state in these names",
            &["_devy_", "__devy_", "__shadowenv_", "__hookbook_"],
        ),
        (
            "fish keeps its own state in these names, and sources files named by some of them",
            &["__fish_"],
        ),
        (
            "bash reads shell functions from these names",
            &["BASH_FUNC_"],
        ),
    ];
    NAMES
        .iter()
        .find(|(_, names)| names.contains(&key))
        .or_else(|| {
            PREFIXES
                .iter()
                .find(|(_, prefixes)| prefixes.iter().any(|p| key.starts_with(p)))
        })
        .map(|(reason, _)| *reason)
}

/// A pinned image reference: `<repository>@sha256:<64 lowercase hex>`.
pub fn image_digest(s: &str) -> bool {
    IMAGE_DIGEST.is_match(s)
}

/// The `<location>: invalid <kind> <value>` error, with control characters in the value
/// escaped (the value is printed with Rust string escaping, in double quotes).
pub fn invalid(location: &str, kind: &str, value: &str) -> anyhow::Error {
    anyhow::anyhow!("{location}: invalid {kind} {value:?}")
}

/// `Ok(())` when `ok`, otherwise the [`invalid`] error.
pub fn require(ok: bool, location: &str, kind: &str, value: &str) -> anyhow::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(invalid(location, kind, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_name_accepts_real_names() {
        for name in [
            "node",
            "postgresql",
            "python3",
            "gcc-12",
            "libssl-dev:arm64",
            "g++",
            "node@20",
            "Microsoft.VisualStudioCode",
            "ruby_build",
        ] {
            assert!(dep_name(name), "{name} must be accepted");
        }
    }

    #[test]
    fn dep_name_rejects_audit_payloads() {
        for name in [
            "-oDPkg::Pre-Invoke::=id",
            "./evil.deb",
            "evil.deb",
            "EVIL.DEB",
            "evilorg/tap/formula",
            "..\\evil",
            "a b",
            "",
            "$(id>/tmp/p)",
            "/tmp/shared",
            "foo\nbar",
        ] {
            assert!(!dep_name(name), "{name:?} must be rejected");
        }
    }

    #[test]
    fn version_accepts_real_versions() {
        for v in [
            "20",
            "3.12.4",
            "1.0.0-rc.1",
            "2:1.2.3-1ubuntu1",
            "1.0+build5",
            "1.0~beta",
            "lts",
            "stable",
        ] {
            assert!(version(v), "{v} must be accepted");
        }
    }

    #[test]
    fn version_rejects_audit_payloads() {
        for v in [
            "1.0;id",
            "-oDPkg::Pre-Invoke::=id",
            "../../x",
            "1..2",
            "lts/iron",
            "$(id)",
            "1.0 2",
            "",
            ".1",
        ] {
            assert!(!version(v), "{v:?} must be rejected");
        }
    }

    #[test]
    fn list_entry_accepts_real_entries() {
        for e in [
            "typescript",
            "@angular/cli",
            "eslint@8",
            "prettier@^3.0.0",
            "npm@>=10",
            "wasm32-unknown-unknown",
            "x86_64-unknown-linux-gnu",
            "clippy",
            "rust-src",
            "gke-gcloud-auth-plugin",
            "pkg=1.2",
            "@types/node@20",
            "ts@npm:typescript@5",
            "lodash@npm:@scope/lodash@^4",
            "pkg@latest",
            "pkg@*",
            "pkg@1.x",
        ] {
            assert!(list_entry(e), "{e} must be accepted");
        }
    }

    #[test]
    fn list_entry_rejects_audit_payloads() {
        for e in [
            "./x",
            "../x",
            "/tmp/shared",
            "-g",
            "--registry=https://evil",
            "https://evil.example/pkg.tgz",
            "git+ssh://git@evil/x",
            ".hidden",
            "a b",
            "$(id>/tmp/p)",
            "",
            "file:../pkg",
            "file:/tmp/shared/pkg",
            "github:evil/pkg",
            "evilorg/pkg",
            "user@example.com:evil/x",
            "x@file:../pkg",
            "x@github:evil/pkg",
            "x@evil/pkg",
            "x@npm:file:../pkg",
            "x@npm:evil/pkg",
            "@/pkg",
            "@scope",
            "@scope/a/b",
            "x@",
            "*",
            "pkg@1 || 2",
            "x@.",
            "x@..",
            "x@.evil",
            "@s/x@.evil",
            "@s/.x",
            "x@npm:y@.evil",
            "evil.tgz",
            "EVIL.TGZ",
            "x@evil.tgz",
            "x@a.tar.gz",
            "x@npm:a.tar",
        ] {
            assert!(!list_entry(e), "{e:?} must be rejected");
        }
    }

    #[test]
    fn command_name_rules() {
        for name in [
            "dev",
            "db:migrate",
            "test.unit",
            "lint-fix",
            "build_all",
            "1",
        ] {
            assert!(command_name(name), "{name} must be accepted");
        }
        for name in ["$(id>/tmp/p)", "-h", "a b", "a;b", "", "x/y", "`id`", ".x"] {
            assert!(!command_name(name), "{name:?} must be rejected");
        }
    }

    #[test]
    fn rel_path_inside_rules() {
        for p in [
            "api", "./api", "a/b/c", "a/../b", "./", ".", "", ".venv", "console", "com", "a.b",
        ] {
            assert!(rel_path_inside(p), "{p:?} must be accepted");
        }
        for p in [
            "../../",
            "..",
            "a/../../b",
            "/tmp/shared",
            "/",
            "C:\\Windows",
            "C:foo",
            "..\\x",
            "a\\b",
            "a\nb",
            ".. ",
            "a.",
            "a ",
            "sub./x",
            "CON",
            "con.txt",
            "a/NUL",
            "COM1",
            "lpt9.log",
        ] {
            assert!(!rel_path_inside(p), "{p:?} must be rejected");
        }
    }

    #[test]
    fn env_key_rules() {
        for k in ["PATH", "_X", "DATABASE_URL", "a1"] {
            assert!(env_key(k), "{k} must be accepted");
        }
        for k in ["1X", "A-B", "A B", "A=B", "", "$(id)", "X\n"] {
            assert!(!env_key(k), "{k:?} must be rejected");
        }
    }

    #[test]
    fn reserved_env_keys() {
        // Representative keys of each category, with the reason fragment it gives.
        for (fragment, keys) in [
            ("working directory", &["PWD", "OLDPWD"][..]),
            ("state directory", &["HOME", "XDG_STATE_HOME"]),
            (
                "parses every command",
                &[
                    "IFS",
                    "SHELLOPTS",
                    "BASHOPTS",
                    "BASH_COMPAT",
                    "POSIXLY_CORRECT",
                    "GLOBIGNORE",
                    "KEYBOARD_HACK",
                    "histchars",
                    "HISTCHARS",
                ],
            ),
            (
                "prompt hooks",
                &[
                    "PROMPT_COMMAND",
                    "STARSHIP_PROMPT_COMMAND",
                    "_PRESERVED_PROMPT_COMMAND",
                    "precmd_functions",
                    "preexec_functions",
                ],
            ),
            (
                "calls the functions it names",
                &[
                    "chpwd_functions",
                    "periodic_functions",
                    "zshaddhistory_functions",
                    "zshexit_functions",
                    "zsh_directory_name_functions",
                    "fish_key_bindings",
                ],
            ),
            (
                "whenever it prints a prompt",
                &[
                    "PS0",
                    "PS1",
                    "PS2",
                    "PS4",
                    "PROMPT",
                    "prompt",
                    "RPROMPT",
                    "RPS1",
                    "SPROMPT",
                    "PROMPT_EOL_MARK",
                ],
            ),
            ("mail files it names", &["MAILPATH", "mailpath"]),
            ("mail files it sets", &["MAIL", "MAILCHECK"]),
            ("translations", &["TEXTDOMAIN", "TEXTDOMAINDIR"]),
            (
                "which commands and functions",
                &[
                    "EXECIGNORE",
                    "FUNCNEST",
                    "BASH_ALIASES",
                    "BASH_CMDS",
                    "auto_resume",
                    "NULLCMD",
                    "READNULLCMD",
                ],
            ),
            (
                "loads functions or modules",
                &[
                    "fish_function_path",
                    "fish_complete_path",
                    "FPATH",
                    "fpath",
                    "module_path",
                    "MODULE_PATH",
                    "BASH_LOADABLES_PATH",
                ],
            ),
            (
                "which files the shell writes",
                &[
                    "HISTFILE",
                    "HISTFILESIZE",
                    "HISTSIZE",
                    "SAVEHIST",
                    "fish_history",
                    "BASH_XTRACEFD",
                    "TMPPREFIX",
                    "TMPSUFFIX",
                ],
            ),
            ("reads or runs files", &["ZDOTDIR"]),
            ("exit by itself", &["TMOUT"]),
            (
                "evaluates its value as arithmetic",
                &[
                    "SHLVL",
                    "LINES",
                    "COLUMNS",
                    "ZLE_RPROMPT_INDENT",
                    "LINENO",
                    "OPTIND",
                    "TRY_BLOCK_ERROR",
                    "TRY_BLOCK_INTERRUPT",
                    "RANDOM",
                    "SRANDOM",
                    "SECONDS",
                    "ERRNO",
                    "HISTCMD",
                    "KEYTIMEOUT",
                    "PERIOD",
                    "DIRSTACKSIZE",
                    "LISTMAX",
                    "LOGCHECK",
                    "BAUD",
                    "REPORTTIME",
                    "REPORTMEMORY",
                    "MENUSCROLL",
                    "UID",
                    "EUID",
                    "GID",
                    "EGID",
                ],
            ),
            (
                "zsh keeps its own",
                &[
                    "functions",
                    "dis_functions",
                    "aliases",
                    "galiases",
                    "saliases",
                    "dis_aliases",
                    "builtins",
                    "commands",
                    "options",
                    "parameters",
                    "modules",
                    "widgets",
                    "keymaps",
                    "jobtexts",
                    "jobstates",
                    "jobdirs",
                    "nameddirs",
                    "userdirs",
                    "history",
                    "historywords",
                    "funcstack",
                    "functrace",
                    "funcsourcetrace",
                    "funcfiletrace",
                ],
            ),
            (
                "keep their state",
                &[
                    "__shadowenv_data",
                    "__shadowenv_force_run",
                    "__hookbook_functions",
                    "_devy_shadowenv_wrapped",
                    "__devy_guard_phys",
                ],
            ),
            (
                "fish keeps its own state",
                &["__fish_config_dir", "__fish_webconfig_theme_notification"],
            ),
            ("shell functions", &["BASH_FUNC_x"]),
        ] {
            for k in keys {
                let reason = reserved_env_key(k).unwrap_or_else(|| panic!("{k} must be reserved"));
                assert!(reason.contains(fragment), "{k}: {reason}");
            }
        }
        // Names that only contain a reserved one, a reserved name in another case, and
        // the variables that only change the programs the shell starts or shells started
        // later.
        for k in [
            "DB_PWD",
            "MYSQL_PWD",
            "pwd",
            "HOMEBREW_PREFIX",
            "MY_HOME",
            "PATH",
            "path",
            "fish_user_paths",
            "CDPATH",
            "BASH_ENV",
            "ENV",
            "INPUTRC",
            "LD_PRELOAD",
            "DYLD_LIBRARY_PATH",
            "TMPDIR",
            "LANG",
            "FIGNORE",
            "HISTIGNORE",
            "POSTEDIT",
            "STTY",
            "WORDCHARS",
            "ps1",
            "mail",
            "histfile",
            "HISTCHARS_X",
            "AUTO_RESUME",
            "FUNCTIONS",
            "my_functions",
            "zdotdir",
            "tmout",
            "lines",
            "columns",
            "EPOCHSECONDS",
            "PROMPT_DIRTRIM",
            "MY_PS1",
            "STARSHIP_CONFIG",
            "__devy",
            "_DEVY_X",
            "__fish",
            "fish_greeting",
            "BASH_FUNCTION",
        ] {
            assert_eq!(reserved_env_key(k), None, "{k} must be allowed");
        }
    }

    #[test]
    fn image_digest_rules() {
        let hex = "a".repeat(64);
        assert!(image_digest(&format!("redis@sha256:{hex}")));
        assert!(image_digest(&format!(
            "registry.example:5000/mirror/postgres@sha256:{hex}"
        )));
        assert!(!image_digest("redis@sha256:abc"));
        assert!(!image_digest(&format!("redis@sha256:{}", "A".repeat(64))));
        assert!(!image_digest(&format!("-redis@sha256:{hex}")));
        assert!(!image_digest(&format!("redis:7@sha256:{hex};id")));
        assert!(!image_digest(&hex));
    }

    #[test]
    fn invalid_error_format_escapes_control_characters() {
        let err = invalid("commands", "command name", "a\x1b[2Jb\n");
        let msg = err.to_string();
        assert_eq!(msg, "commands: invalid command name \"a\\u{1b}[2Jb\\n\"");
        assert!(!msg.contains('\x1b') && !msg.contains('\n'));
    }

    #[test]
    fn require_passes_through_valid_values() {
        assert!(require(true, "x", "y", "z").is_ok());
        assert!(require(false, "x", "y", "z").is_err());
    }

    #[test]
    fn dep_name_rejects_local_package_files() {
        for n in ["evil.deb", "evil.rb", "Evil.RB", "cask.json"] {
            assert!(!dep_name(n), "{n}");
        }
        assert!(dep_name("ruby"));
    }

    #[test]
    fn toolchain_rule() {
        for t in [
            "stable",
            "nightly",
            "1.78.0",
            "nightly-2024-01-01",
            "stable-x86_64-unknown-linux-gnu",
        ] {
            assert!(toolchain(t), "{t}");
        }
        for t in [
            "--force-non-host",
            "-v",
            "/tmp/tc",
            "../tc",
            "a b",
            "x:y",
            "a@b",
            "",
        ] {
            assert!(!toolchain(t), "{t:?}");
        }
    }
}
