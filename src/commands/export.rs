use anyhow::{Context, Result};
use std::path::Path;

use crate::config::DevyConfig;
use crate::modules;
use crate::output;

/// Output format for `devy export`.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ExportFormat {
    /// Generate a `shell.nix` for use with `nix-shell`
    Shell,
    /// Generate a `flake.nix` (default)
    Flake,
}

/// Produces a valid Nix double-quoted string literal from a raw value.
fn nix_string_value(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${");
    format!("\"{}\"", escaped)
}

/// Escapes `s` for the body of a Nix indented string (`''…''`): `''` becomes `'''`,
/// `${` becomes `''${`, and a lone `'` right before `${` becomes `''\'` so it cannot
/// merge with the `''${` escape into a `'''` sequence.
fn nix_indented_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("''") {
            out.push_str("'''");
            rest = &rest[2..];
        } else if rest.starts_with("'${") {
            out.push_str("''\\'");
            rest = &rest[1..];
        } else if rest.starts_with("${") {
            out.push_str("''${");
            rest = &rest[2..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// The shellHook line announcing the shell, safe inside a Nix `''…''` string: the shell
/// receives the message as one single-quoted word, so `$`, backticks and `\` in the
/// project name are not interpreted. Control characters (newlines in particular, which
/// Nix's indentation stripping would alter) are replaced with spaces.
fn shell_hook_echo(project_name: &str) -> String {
    let name: String = project_name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let message = format!("Entered {name} dev shell");
    nix_indented_escape(&format!("echo {}", super::exec::sh_quote(&message)))
}

/// Nix keywords, which cannot be bare attribute names.
const NIX_KEYWORDS: &[&str] = &[
    "assert", "else", "if", "in", "inherit", "let", "or", "rec", "then", "with",
];

/// Returns a Nix attribute name, quoting it (with the `"…"` string escapes) if it is
/// not a valid bare Nix identifier.
fn nix_attr_name(k: &str) -> String {
    let is_bare = !k.is_empty()
        && !NIX_KEYWORDS.contains(&k)
        && k.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_alphabetic() || b == b'_' || (i > 0 && (b.is_ascii_digit() || b == b'-'))
        });
    if is_bare {
        k.to_string()
    } else {
        nix_string_value(k)
    }
}

/// Attributes `devy export` emits itself; an `environment` entry with one of these names
/// would be a duplicate attribute, which Nix rejects.
const EMITTED_ATTRS: &[&str] = &["packages", "shellHook"];

/// `name = "value";` lines for each `environment` entry, indented by `indent`, in key
/// order. Keys and values are escaped, so a value can never close its string or add an
/// attribute of its own.
fn env_attr_lines(config: &DevyConfig, indent: &str) -> Vec<String> {
    let mut entries: Vec<(&String, &String)> = config.environment.iter().collect();
    entries.sort();
    entries
        .into_iter()
        .map(|(k, v)| {
            if EMITTED_ATTRS.contains(&k.as_str()) {
                output::warn(&format!(
                    "environment key {k} would replace the {k} devy export writes; left out of the export"
                ));
                format!("{indent}# {k}: left out by devy export (devy writes its own {k})")
            } else {
                format!("{indent}{} = {};", nix_attr_name(k), nix_string_value(v))
            }
        })
        .collect()
}

/// Attributes the exported nixpkgs import must permit despite nixpkgs' license and
/// vulnerability checks.
#[derive(Default)]
struct Permitted {
    unfree: Vec<String>,
    insecure: Vec<String>,
}

/// Returns `pkgs.<attr>` lines for each dependency, indented by `indent`, and the
/// attributes among them whose module declares them unfree or insecure.
/// A `devy.yml` version that maps to a versioned nixpkgs attribute selects it.
/// Deps whose module returns `None` from `nix_attr` are omitted.
fn collect_pkg_lines(config: &DevyConfig, indent: &str) -> (Vec<String>, Permitted) {
    let mut permitted = Permitted::default();
    let lines = config
        .dependencies
        .iter()
        .flat_map(|raw| match raw {
            crate::config::RawDependency::Simple(name) => vec![(name.clone(), None)],
            crate::config::RawDependency::Configured(map) => map
                .iter()
                .map(|(name, cfg)| (name.clone(), cfg.as_ref().and_then(|c| c.version.clone())))
                .collect(),
        })
        .filter_map(|(name, version)| {
            let canonical = modules::canonical_name(&name);
            let dep = crate::config::Dependency {
                version,
                ..crate::config::Dependency::simple(canonical)
            };
            let module = modules::get(canonical);
            let attr = module.nix_attr(&dep)?;
            if attr.is_empty() {
                return None;
            }
            // `lib.getName` matches the pname, which equals the attribute for every
            // module that declares itself unfree or insecure.
            if module.nix_unfree() && !permitted.unfree.contains(&attr) {
                permitted.unfree.push(attr.clone());
            }
            if module.nix_insecure() && !permitted.insecure.contains(&attr) {
                permitted.insecure.push(attr.clone());
            }
            Some(format!("{indent}pkgs.{attr}"))
        })
        .collect();
    (lines, permitted)
}

/// `config.allowUnfreePredicate = …;` and `config.allowInsecurePredicate = …;`, each
/// permitting exactly its packages matched via `get_name`. `None` when both are empty.
fn permit_predicates(get_name: &str, permitted: &Permitted) -> Option<String> {
    let predicates: Vec<String> = [
        ("allowUnfreePredicate", &permitted.unfree),
        ("allowInsecurePredicate", &permitted.insecure),
    ]
    .into_iter()
    .filter(|(_, attrs)| !attrs.is_empty())
    .map(|(option, attrs)| {
        let names: Vec<String> = attrs.iter().map(|a| nix_string_value(a)).collect();
        format!(
            "config.{option} = pkg: builtins.elem ({get_name} pkg) [ {} ];",
            names.join(" ")
        )
    })
    .collect();
    (!predicates.is_empty()).then(|| predicates.join(" "))
}

fn generate_shell_nix(config: &DevyConfig) -> String {
    let (pkg_lines, permitted) = collect_pkg_lines(config, "    ");

    let env_lines = env_attr_lines(config, "    ");

    let project_name = config.name.as_deref().unwrap_or("project");

    let nixpkgs_args = match permit_predicates("(import <nixpkgs/lib>).getName", &permitted) {
        Some(predicates) => format!("{{ {predicates} }}"),
        None => "{}".to_string(),
    };
    let mut out = format!(
        "# Generated by `devy export --format=shell`. Edit to taste.\n\
         {{ pkgs ? import <nixpkgs> {nixpkgs_args} }}:\n\
         \n\
         pkgs.mkShell {{\n\
         \n\
         "
    );

    if !pkg_lines.is_empty() {
        out.push_str("  packages = with pkgs; [\n");
        for line in &pkg_lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("  ];\n\n");
    }

    if !env_lines.is_empty() {
        out.push_str("  # Environment variables\n");
        for line in &env_lines {
            out.push_str("  ");
            out.push_str(line.trim_start());
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "  shellHook = ''\n    {}\n  '';\n",
        shell_hook_echo(project_name)
    ));

    out.push_str("}\n");
    out
}

fn generate_flake_nix(config: &DevyConfig) -> String {
    let (pkg_lines, permitted) = collect_pkg_lines(config, "          ");

    let env_lines = env_attr_lines(config, "          ");

    let project_name = config.name.as_deref().unwrap_or("project");
    let description = nix_string_value(&format!("{project_name} development environment"));

    let pkgs_expr = match permit_predicates("nixpkgs.lib.getName", &permitted) {
        Some(predicates) => format!("import nixpkgs {{ inherit system; {predicates} }}"),
        None => "nixpkgs.legacyPackages.${system}".to_string(),
    };
    let mut out = format!(
        "# Generated by `devy export --format=flake`. Edit to taste.\n\
         {{\n\
           description = {description};\n\
         \n\
           inputs.nixpkgs.url = \"github:NixOS/nixpkgs/nixpkgs-unstable\";\n\
         \n\
           outputs = {{ self, nixpkgs }}:\n\
             let\n\
               forAllSystems = nixpkgs.lib.genAttrs [\n\
                 \"x86_64-linux\" \"aarch64-linux\" \"x86_64-darwin\" \"aarch64-darwin\"\n\
               ];\n\
             in {{\n\
               devShells = forAllSystems (system:\n\
                 let pkgs = {pkgs_expr}; in {{\n\
                   default = pkgs.mkShell {{\n",
    );

    if !pkg_lines.is_empty() {
        out.push_str("            packages = [\n");
        for line in &pkg_lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("            ];\n\n");
    }

    if !env_lines.is_empty() {
        out.push_str("            # Environment variables\n");
        for line in &env_lines {
            out.push_str(line.trim_start());
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "            shellHook = ''\n              {}\n            '';\n",
        shell_hook_echo(project_name)
    ));

    out.push_str("          };\n        });\n    };\n}\n");
    out
}

/// Writes the export for `config` into `out_path`.
pub(crate) fn export_impl(
    config: &DevyConfig,
    format: ExportFormat,
    out_path: &Path,
) -> Result<()> {
    let (content, filename) = match format {
        ExportFormat::Shell => (generate_shell_nix(config), "shell.nix"),
        ExportFormat::Flake => (generate_flake_nix(config), "flake.nix"),
    };
    let dest = out_path.join(filename);

    if dest.exists() {
        output::warn(&format!("{} already exists — overwriting", dest.display()));
    }

    crate::fs_safe::write_atomic(&dest, content.as_bytes(), 0o644)
        .with_context(|| format!("Failed to write {}", dest.display()))?;

    output::success(&format!("Wrote {}", dest.display()));
    Ok(())
}

#[cfg_attr(test, mutants::skip)] // thin I/O wrapper
pub fn run(format: ExportFormat) -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;
    output::header("devy export");
    export_impl(&config, format, &project_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevyConfig;
    use serde_norway as yaml;

    fn config_from_yaml(yaml: &str) -> DevyConfig {
        yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn module_registry_nix_attrs_are_correct() {
        use crate::config::Dependency;
        use crate::modules;
        let dep = |n| Dependency::simple(n);
        assert_eq!(
            modules::get("redis").nix_attr(&dep("redis")).as_deref(),
            Some("redis")
        );
        assert_eq!(
            modules::get("mysql").nix_attr(&dep("mysql")).as_deref(),
            Some("mysql84")
        );
        assert_eq!(
            modules::get("rabbitmq")
                .nix_attr(&dep("rabbitmq"))
                .as_deref(),
            Some("rabbitmq-server")
        );
        assert_eq!(
            modules::get("mongodb").nix_attr(&dep("mongodb")).as_deref(),
            Some("mongodb-ce")
        );
        assert_eq!(
            modules::get("postgresql")
                .nix_attr(&dep("postgresql"))
                .as_deref(),
            Some("postgresql")
        );
        assert_eq!(
            modules::get("node").nix_attr(&dep("node")).as_deref(),
            Some("nodejs")
        );
        assert_eq!(
            modules::get("rust").nix_attr(&dep("rust")).as_deref(),
            Some("rustup")
        );
        assert_eq!(
            modules::get("python").nix_attr(&dep("python")).as_deref(),
            Some("python3")
        );
        assert_eq!(
            modules::get("go").nix_attr(&dep("go")).as_deref(),
            Some("go")
        );
        assert_eq!(
            modules::get("awscli").nix_attr(&dep("awscli")).as_deref(),
            Some("awscli2")
        );
        assert_eq!(
            modules::get("helm").nix_attr(&dep("helm")).as_deref(),
            Some("kubernetes-helm")
        );
    }

    #[test]
    fn collect_pkg_lines_resolves_aliases() {
        // "postgres" is an alias for "postgresql"; the nix attr must be "postgresql".
        let config = config_from_yaml("dependencies:\n  - postgres\n");
        let (lines, _) = collect_pkg_lines(&config, "");
        assert!(
            lines.iter().any(|l| l.contains("postgresql")),
            "alias 'postgres' must resolve to pkgs.postgresql"
        );
        assert!(
            !lines.iter().any(|l| l.contains("postgres\"")),
            "must not emit pkgs.postgres"
        );
    }

    #[test]
    fn generate_shell_nix_contains_package_entries() {
        let config = config_from_yaml("dependencies:\n  - redis\n  - node\n");
        let out = generate_shell_nix(&config);
        assert!(out.contains("pkgs.redis"), "expected redis in shell.nix");
        assert!(out.contains("pkgs.nodejs"), "expected nodejs in shell.nix");
        assert!(out.contains("mkShell"), "expected mkShell in shell.nix");
    }

    #[test]
    fn generate_flake_nix_contains_package_entries() {
        let config = config_from_yaml("dependencies:\n  - python\n  - postgresql\n");
        let out = generate_flake_nix(&config);
        assert!(out.contains("pkgs.python3"));
        assert!(out.contains("pkgs.postgresql"));
        assert!(out.contains("devShells"));
        assert!(out.contains("nixpkgs-unstable"));
    }

    #[test]
    fn generate_shell_nix_includes_env_vars() {
        let config = config_from_yaml(
            "dependencies:\n  - redis\nenvironment:\n  DATABASE_URL: \"postgres://localhost/dev\"\n",
        );
        let out = generate_shell_nix(&config);
        assert!(
            out.contains("DATABASE_URL = \"postgres://localhost/dev\";"),
            "env var must be properly quoted in shell.nix, got:\n{out}"
        );
    }

    #[test]
    fn nix_string_value_escapes_special_chars() {
        assert_eq!(nix_string_value(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(nix_string_value(r"back\slash"), r#""back\\slash""#);
        assert_eq!(nix_string_value("${FOO}"), r#""\${FOO}""#);
        assert_eq!(nix_string_value("plain"), "\"plain\"");
    }

    #[test]
    fn nix_attr_name_quotes_unsafe_keys() {
        assert_eq!(nix_attr_name("NORMAL_KEY"), "NORMAL_KEY");
        assert_eq!(nix_attr_name("MY-VAR"), "MY-VAR");
        assert_eq!(nix_attr_name("has space"), "\"has space\"");
        assert_eq!(nix_attr_name(""), "\"\"");
        assert_eq!(nix_attr_name("123starts_digit"), "\"123starts_digit\"");
    }

    #[test]
    fn nix_attr_name_escapes_string_metacharacters() {
        assert_eq!(nix_attr_name(r"a\b"), r#""a\\b""#);
        assert_eq!(nix_attr_name("${x}"), r#""\${x}""#);
        assert_eq!(nix_attr_name(r#"a"b"#), r#""a\"b""#);
        assert_eq!(nix_attr_name("let"), "\"let\"");
    }

    #[test]
    fn nix_indented_escape_cases() {
        assert_eq!(nix_indented_escape("plain"), "plain");
        assert_eq!(nix_indented_escape("a''b"), "a'''b");
        assert_eq!(nix_indented_escape("${x}"), "''${x}");
        assert_eq!(nix_indented_escape("'${x}"), "''\\'''${x}");
        assert_eq!(nix_indented_escape("'''"), "''''");
        assert_eq!(nix_indented_escape("$x 'y'"), "$x 'y'");
    }

    #[test]
    fn shell_hook_echo_single_quotes_name() {
        assert_eq!(shell_hook_echo("app"), "echo 'Entered app dev shell'");
        assert_eq!(
            shell_hook_echo("$(id) `id` \\"),
            "echo 'Entered $(id) `id` \\ dev shell'"
        );
        assert_eq!(
            shell_hook_echo("o'neil"),
            "echo 'Entered o'\\'''neil dev shell'"
        );
        assert_eq!(shell_hook_echo("${x}"), "echo 'Entered ''${x} dev shell'");
        assert_eq!(
            shell_hook_echo("a\n    b\r"),
            "echo 'Entered a     b  dev shell'"
        );
    }

    const HOSTILE_NAME: &str = r#"x"; ${builtins.abort "p"} $(touch /tmp/p) ''"#;

    #[test]
    fn hostile_name_is_escaped_in_both_formats() {
        let config = DevyConfig {
            name: Some(HOSTILE_NAME.into()),
            ..config_from_yaml("dependencies:\n  - redis\n")
        };
        let flake = generate_flake_nix(&config);
        assert!(
            flake.contains(
                r#"description = "x\"; \${builtins.abort \"p\"} $(touch /tmp/p) '' development environment";"#
            ),
            "{flake}"
        );
        for out in [generate_shell_nix(&config), flake] {
            assert!(
                out.contains(
                    r#"echo 'Entered x"; ''${builtins.abort "p"} $(touch /tmp/p) '\''''\''' dev shell'"#
                ),
                "{out}"
            );
            assert!(!out.contains("\"; ${builtins"), "{out}");
        }
    }

    /// Writes `content` to `file` in a temp dir and evaluates `expr` (a function of the
    /// file's path `p`) with `nix-instantiate --eval --strict --json`.
    fn nix_eval(file: &str, content: &str, expr: &str) -> serde_json::Value {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join(file);
        std::fs::write(&path, content).unwrap();
        let parse = std::process::Command::new("nix-instantiate")
            .arg("--parse")
            .arg(&path)
            .output()
            .unwrap();
        assert!(parse.status.success(), "{parse:?}\n{content}");
        let out = std::process::Command::new("nix-instantiate")
            .args(["--eval", "--strict", "--json", "--argstr", "p"])
            .arg(&path)
            .arg("-E")
            .arg(format!("{{ p }}: {expr}"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}\n{content}");
        serde_json::from_slice(&out.stdout).unwrap()
    }

    #[test]
    fn hostile_name_evaluates_with_nix() {
        if which::which("nix-instantiate").is_err() {
            eprintln!("skipping: nix-instantiate not on PATH");
            return;
        }
        let marker_dir = crate::test_support::tmp_dir();
        let marker = marker_dir.join("p");
        for name in [
            HOSTILE_NAME.to_string(),
            format!(
                r#"x"; ${{builtins.abort "p"}} $(touch {m}) `touch {m}` '${{x}} ''' \"#,
                m = marker.display()
            ),
        ] {
            let config = DevyConfig {
                name: Some(name.clone()),
                ..config_from_yaml("dependencies:\n  - redis\n")
            };
            let shell_hook = nix_eval(
                "shell.nix",
                &generate_shell_nix(&config),
                "(import p { pkgs = { mkShell = x: x; redis = null; }; }).shellHook",
            );
            let flake = nix_eval(
                "flake.nix",
                &generate_flake_nix(&config),
                r#"let
                     f = import p;
                     pkgs = { mkShell = x: x; redis = null; };
                     systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
                     nixpkgs = {
                       lib.genAttrs = names: fn: builtins.listToAttrs
                         (map (n: { name = n; value = fn n; }) names);
                       legacyPackages = builtins.listToAttrs
                         (map (n: { name = n; value = pkgs; }) systems);
                     };
                   in [ f.description
                        (f.outputs { self = null; inherit nixpkgs; }).devShells.x86_64-linux.default.shellHook ]"#,
            );
            assert_eq!(
                flake[0].as_str().unwrap(),
                format!("{name} development environment")
            );
            for hook in [shell_hook.as_str().unwrap(), flake[1].as_str().unwrap()] {
                // mkShell runs the hook in bash, whose `echo` leaves `\` alone.
                let out = std::process::Command::new("bash")
                    .arg("-c")
                    .arg(hook)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{out:?}");
                assert_eq!(
                    String::from_utf8_lossy(&out.stdout),
                    format!("Entered {name} dev shell\n")
                );
                assert!(!marker.exists(), "shellHook ran an embedded command");
            }
        }
    }

    #[test]
    fn export_impl_shell_writes_file() {
        let dir = crate::test_support::tmp_dir();
        let config = config_from_yaml("dependencies:\n  - redis\n");
        export_impl(&config, ExportFormat::Shell, &dir).unwrap();
        let written = std::fs::read_to_string(dir.join("shell.nix")).unwrap();
        assert!(written.contains("pkgs.redis"));
    }

    #[test]
    fn export_impl_flake_writes_file() {
        let dir = crate::test_support::tmp_dir();
        let config = config_from_yaml("dependencies:\n  - node\n");
        export_impl(&config, ExportFormat::Flake, &dir).unwrap();
        let written = std::fs::read_to_string(dir.join("flake.nix")).unwrap();
        assert!(written.contains("pkgs.nodejs"));
        assert!(written.contains("devShells"));
    }

    #[test]
    fn generate_flake_nix_includes_env_vars() {
        let config =
            config_from_yaml("dependencies:\n  - node\nenvironment:\n  API_KEY: \"secret\"\n");
        let out = generate_flake_nix(&config);
        assert!(
            out.contains("API_KEY = \"secret\";"),
            "env var must be properly quoted in flake.nix, got:\n{out}"
        );
    }

    /// `nix` with the contents of every string literal removed, leaving the code a Nix
    /// parser would evaluate: `"…"` strings (with `\` escapes) and `''…''` indented
    /// strings (with the `'''`, `''$` and `''\` escapes). Antiquotations are not
    /// followed; the generator escapes every `${` in a value.
    fn nix_code_outside_strings(nix: &str) -> String {
        let mut code = String::new();
        let mut chars = nix.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => {
                                chars.next();
                            }
                            '"' => break,
                            _ => {}
                        }
                    }
                    code.push_str("\"\"");
                }
                '\'' if chars.peek() == Some(&'\'') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c != '\'' || chars.peek() != Some(&'\'') {
                            continue;
                        }
                        chars.next();
                        match chars.peek() {
                            Some('\'' | '$') => {
                                chars.next();
                            }
                            Some('\\') => {
                                chars.next();
                                chars.next();
                            }
                            _ => break,
                        }
                    }
                    code.push_str("''''");
                }
                _ => code.push(c),
            }
        }
        code
    }

    #[test]
    fn nix_code_outside_strings_drops_string_contents() {
        assert_eq!(
            nix_code_outside_strings(r#"a = "x\" b = y"; c = ''p''' '''$ ''\n q''; d"#),
            r#"a = ""; c = ''''; d"#
        );
    }

    const HOSTILE_ENV: &str = concat!(
        "environment:\n",
        "  preHook: \"touch /tmp/p\"\n",
        "  BASH_ENV: \"/tmp/evil.sh\"\n",
        "  NOTE: \"line one\\n\\\"; shellHook = \\\"touch /tmp/q\\\"; x = \\\"\\r\\u0007\"\n",
        "  FOO: bar\n",
    );

    #[test]
    fn export_writes_environment_entries_escaped() {
        let config = config_from_yaml(HOSTILE_ENV);
        let mut outputs = Vec::new();
        let warns = crate::output::with_warn_messages(|| {
            outputs.push(generate_shell_nix(&config));
            outputs.push(generate_flake_nix(&config));
        });
        assert!(warns.is_empty(), "{warns:?}");
        for out in outputs {
            assert!(out.contains("preHook = \"touch /tmp/p\";"), "{out}");
            assert!(out.contains("BASH_ENV = \"/tmp/evil.sh\";"), "{out}");
            assert!(out.contains("FOO = \"bar\";"), "{out}");
            // The quote is escaped, so the value cannot close the string.
            assert!(out.contains(r#"shellHook = \"touch /tmp/q\";"#), "{out}");
            // Only devy's own shellHook is live Nix code; the injected one stays inside
            // NOTE's string. (Values keep literal newlines, so a line-based count cannot
            // tell the two apart.)
            assert_eq!(
                nix_code_outside_strings(&out)
                    .matches("shellHook =")
                    .count(),
                1,
                "{out}"
            );
        }
    }

    #[test]
    fn export_leaves_out_attributes_devy_writes() {
        let config = config_from_yaml("environment:\n  shellHook: x\n  packages: y\n  A: b\n");
        let warns = crate::output::with_warn_messages(|| {
            let out = generate_shell_nix(&config);
            assert_eq!(out.matches("shellHook =").count(), 1, "{out}");
            assert_eq!(out.matches("packages =").count(), 0, "{out}");
            assert!(out.contains("A = \"b\";"), "{out}");
        });
        assert_eq!(warns.len(), 2, "{warns:?}");
    }

    #[test]
    fn export_without_environment_has_no_warning_or_section() {
        let config = config_from_yaml("dependencies:\n  - redis\n");
        let warns = crate::output::with_warn_messages(|| {
            let out = generate_shell_nix(&config);
            assert!(!out.contains("Environment variables"), "{out}");
        });
        assert!(warns.is_empty(), "{warns:?}");
    }

    #[test]
    fn export_impl_warns_on_overwrite() {
        let dir = crate::test_support::tmp_dir();
        let config = config_from_yaml("dependencies:\n  - redis\n");
        // Write the file once to trigger the overwrite branch.
        export_impl(&config, ExportFormat::Shell, &dir).unwrap();
        let warn_count = crate::output::with_warn_capture(|| {
            export_impl(&config, ExportFormat::Shell, &dir).unwrap();
        });
        assert_eq!(warn_count, 1, "must warn exactly once when overwriting");
    }

    #[test]
    fn export_uses_versioned_nix_attributes() {
        let config = config_from_yaml(
            "dependencies:\n  - node:\n      version: \"22\"\n  - python:\n      version: \"3.12.4\"\n  - redis:\n      version: \"7\"\n",
        );
        let out = generate_flake_nix(&config);
        assert!(out.contains("pkgs.nodejs_22"), "{out}");
        assert!(out.contains("pkgs.python312"), "{out}");
        // redis has no versioned attributes: the unversioned one is used.
        assert!(out.contains("pkgs.redis\n"), "{out}");
        let out = generate_shell_nix(&config);
        assert!(out.contains("pkgs.nodejs_22"), "{out}");
    }

    #[test]
    fn mongodb_export_allows_exactly_mongodb_ce() {
        let config = config_from_yaml("dependencies:\n  - mongo\n  - redis\n");
        let pred = r#"config.allowUnfreePredicate = pkg: builtins.elem (nixpkgs.lib.getName pkg) [ "mongodb-ce" ];"#;
        let flake = generate_flake_nix(&config);
        assert!(
            flake.contains(&format!(
                "let pkgs = import nixpkgs {{ inherit system; {pred} }}; in"
            )),
            "{flake}"
        );
        assert!(!flake.contains("legacyPackages"), "{flake}");
        let shell = generate_shell_nix(&config);
        assert!(
            shell.contains(r#"{ pkgs ? import <nixpkgs> { config.allowUnfreePredicate = pkg: builtins.elem ((import <nixpkgs/lib>).getName pkg) [ "mongodb-ce" ]; } }:"#),
            "{shell}"
        );
    }

    #[test]
    fn elasticsearch_export_allows_it_as_unfree_and_insecure() {
        let config = config_from_yaml("dependencies:\n  - elasticsearch\n  - mongodb\n");
        let flake = generate_flake_nix(&config);
        assert!(
            flake.contains(r#"let pkgs = import nixpkgs { inherit system; config.allowUnfreePredicate = pkg: builtins.elem (nixpkgs.lib.getName pkg) [ "elasticsearch" "mongodb-ce" ]; config.allowInsecurePredicate = pkg: builtins.elem (nixpkgs.lib.getName pkg) [ "elasticsearch" ]; }; in"#),
            "{flake}"
        );
        let shell = generate_shell_nix(&config);
        assert!(
            shell.contains(r#"config.allowInsecurePredicate = pkg: builtins.elem ((import <nixpkgs/lib>).getName pkg) [ "elasticsearch" ]; } }:"#),
            "{shell}"
        );

        let config = config_from_yaml("dependencies:\n  - mongodb\n  - opensearch\n");
        assert!(!generate_flake_nix(&config).contains("allowInsecure"));
        assert!(!generate_shell_nix(&config).contains("allowInsecure"));
    }

    #[test]
    fn vault_export_allows_vault() {
        let config = config_from_yaml("dependencies:\n  - vault\n  - terraform\n");
        let shell = generate_shell_nix(&config);
        assert!(shell.contains(r#"[ "vault" "terraform" ]"#), "{shell}");
    }

    #[test]
    fn free_only_export_is_unchanged() {
        let config = config_from_yaml("name: app\ndependencies:\n  - redis\n  - node\n");
        assert_eq!(
            generate_shell_nix(&config),
            "# Generated by `devy export --format=shell`. Edit to taste.\n\
             { pkgs ? import <nixpkgs> {} }:\n\
             \n\
             pkgs.mkShell {\n\
             \n  packages = with pkgs; [\n    pkgs.redis\n    pkgs.nodejs\n  ];\n\n\
             \x20 shellHook = ''\n    echo 'Entered app dev shell'\n  '';\n}\n"
        );
        let flake = generate_flake_nix(&config);
        assert!(
            flake.contains("let pkgs = nixpkgs.legacyPackages.${system}; in {\n"),
            "{flake}"
        );
        assert!(!flake.contains("allowUnfree"), "{flake}");
    }

    #[test]
    fn unfree_exports_are_well_formed() {
        let config = config_from_yaml("dependencies:\n  - mongodb\n  - elasticsearch\n");
        for (out, file) in [
            (generate_shell_nix(&config), "shell.nix"),
            (generate_flake_nix(&config), "flake.nix"),
        ] {
            assert_eq!(
                out.matches('{').count(),
                out.matches('}').count(),
                "unbalanced braces in {file}:\n{out}"
            );
            assert_eq!(out.matches('[').count(), out.matches(']').count(), "{out}");
            if let Ok(bin) = which::which("nix-instantiate") {
                let dir = crate::test_support::tmp_dir();
                let path = dir.join(file);
                std::fs::write(&path, &out).unwrap();
                let status = std::process::Command::new(bin)
                    .arg("--parse")
                    .arg(&path)
                    .stdout(std::process::Stdio::null())
                    .status()
                    .unwrap();
                assert!(status.success(), "nix-instantiate --parse failed:\n{out}");
            }
        }
    }

    #[test]
    fn generate_shell_nix_is_well_formed() {
        let config =
            config_from_yaml("dependencies:\n  - redis\n  - node\nenvironment:\n  FOO: bar\n");
        let out = generate_shell_nix(&config);
        assert_eq!(
            out.matches('{').count(),
            out.matches('}').count(),
            "unbalanced braces in shell.nix:\n{out}"
        );
        assert!(
            out.ends_with("\n}\n"),
            "shell.nix must end with `}}`:\n{out}"
        );

        // When Nix is installed, confirm the expression actually parses.
        if let Ok(bin) = which::which("nix-instantiate") {
            let dir = crate::test_support::tmp_dir();
            let path = dir.join("shell.nix");
            std::fs::write(&path, &out).unwrap();
            let status = std::process::Command::new(bin)
                .arg("--parse")
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "nix-instantiate --parse failed:\n{out}");
        }
    }
}
