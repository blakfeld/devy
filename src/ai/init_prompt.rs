//! The request behind `devy init`: devy's schema, module catalog and rules as the
//! prompt's preamble, then the detected draft and redacted project files.

use std::path::Path;

use super::{Request, redact};
use crate::init_detect::{self, DETECT_HEADER};
use crate::modules;

/// Project files sent to the model, besides `.github/workflows/*.yml`. Nothing outside
/// this list is read.
const FILES: &[&str] = &[
    "package.json",
    ".nvmrc",
    ".node-version",
    ".tool-versions",
    ".ruby-version",
    ".python-version",
    "rust-toolchain.toml",
    "rust-toolchain",
    "go.mod",
    "Gemfile",
    "pyproject.toml",
    "requirements.txt",
    "Cargo.toml",
    "compose.yaml",
    "compose.yml",
    "docker-compose.yml",
    "docker-compose.yaml",
    ".env.example",
    ".env.sample",
    ".env.template",
    "Makefile",
    "Procfile",
    "README.md",
];

const SCHEMA: &str = r#"devy.yml schema:

name: <string>                     # project name
package_manager: auto|nix|brew|apt # optional; omit unless the project clearly requires one
service_manager: package|docker    # optional; docker runs every service as a per-project container
container_cli: docker|podman       # optional; only matters when a service is docker-managed
dependencies:                      # list; each item is a bare name or a one-key map
  - redis
  - node:
      version: "22"                # always a quoted string
  - mysql:
      port: 3307                   # only keys listed in the module's extra_keys
environment:                       # map of string -> string; ${VAR} references other vars
  DATABASE_URL: "mysql://root@${MYSQL_HOST}:${MYSQL_PORT}/app"
commands:                          # map of name -> shell command, or {cmd, cwd, shell}
  dev: "npm run dev"
  migrate:
    cmd: "bundle exec rails db:migrate"
    shell: bash                    # sh, bash, zsh, fish, cmd or powershell
hooks:                             # optional: before_up, after_up, before_down, after_down
  after_up: "bundle install"       # a command, {cmd, shell}, or a list of them

Every dependency item also accepts: version, after_install, shell, tap (brew only).
Service dependencies (service: true in the catalog) also accept service_manager (package|docker,
overriding the top-level setting) and image (a replacement image repository, e.g. a registry
mirror); these are rejected on anything that is not a service.
No other top-level keys exist."#;

const RULES: &str = "Rules:
- Reply with only the complete devy.yml, in a single ```yaml fenced block.
- Use canonical module names from the catalog. Names not in the catalog are installed as plain packages; use them only for real tools the project needs.
- For a dependency, use only the extra keys listed in its catalog extra_keys (null means any key is accepted).
- devy injects <NAME>_HOST and <NAME>_PORT for every service (see each entry's env). Build connection strings from them; never hardcode service hosts or ports.
- Never write secret values. Leave a secret empty and add a `# TODO: set` comment.
- Do not name a command after a devy subcommand: {builtins}.
- Keep the detected draft's facts unless the project files contradict them. Put anything uncertain in a `# TODO:` comment.";

/// Kept short: it travels on the command line, where Windows limits length. The schema,
/// catalog and rules go on stdin with the project content.
const SYSTEM: &str = "You write devy.yml files for devy, a declarative developer environment manager. \
Follow the schema, catalog and rules in the prompt exactly.";

/// Builds the request for `dir`. A `model` of `None` uses claude's configured default.
pub fn request(dir: &Path, model: Option<&str>) -> Request {
    Request::new(model, SYSTEM.to_string(), user_content(dir))
}

fn user_content(dir: &Path) -> String {
    let catalog = serde_json::to_string(&modules::catalog()).expect("catalog serializes");
    let rules = RULES.replace("{builtins}", &crate::cli::builtin_subcommands().join(", "));
    let draft = init_detect::detect(dir).render(DETECT_HEADER);
    let mut out = format!(
        "{SCHEMA}\n\nModule catalog (JSON):\n{catalog}\n\n{rules}\n\n\
         Write a devy.yml for this project.\n\n\
         Draft from devy's offline detection:\n```yaml\n{}```\n\nProject files:\n",
        redact::text(&draft)
    );
    for (path, content) in project_files(dir) {
        out.push_str(&format!(
            "\n=== {path} ===\n{}",
            redact::cap_file(&redact::text(&content))
        ));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    redact::cap_total(&out)
}

/// The allow-listed files present in `dir`, as (relative path, content).
fn project_files(dir: &Path) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = FILES
        .iter()
        .filter_map(|f| Some((f.to_string(), init_detect::read(dir, f)?)))
        .collect();
    let workflows = dir.join(".github").join("workflows");
    let mut names: Vec<String> = std::fs::read_dir(&workflows)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".yml") || n.ends_with(".yaml"))
        .collect();
    names.sort();
    for name in names {
        if let Some(content) = init_detect::read(&workflows, &name) {
            files.push((format!(".github/workflows/{name}"), content));
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_detect::test_util::fixture;

    fn sample() -> crate::test_support::TempDir {
        fixture(&[
            (".nvmrc", "22\n"),
            (
                "package.json",
                r#"{"name":"shop","scripts":{"dev":"vite"}}"#,
            ),
            (
                "docker-compose.yml",
                "services:\n  db:\n    image: postgres:16\n",
            ),
            (".env", "SENTINEL=do-not-send-me\n"),
            (".env.local", "SENTINEL2=do-not-send-me\n"),
            (".env.example", "STRIPE_SECRET_KEY=sk_test_abc\nPORT=3000\n"),
            ("Makefile", "test:\n\tnpm test\n"),
            ("Procfile", "web: npm start\n"),
            ("README.md", "# Shop\n"),
            (".github/workflows/ci.yml", "on: push\n"),
            (".github/workflows/notes.txt", "ignored\n"),
            ("id_rsa", "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n"),
        ])
    }

    #[test]
    fn prompt_contains_catalog_draft_and_files() {
        let dir = sample();
        let req = request(&dir, Some("claude-sonnet-5-5"));
        assert_eq!(req.model.as_deref(), Some("claude-sonnet-5-5"));
        assert!(
            req.system.len() < 512,
            "the system prompt is passed as an argument and must stay short"
        );
        let user = &req.messages[0].content;
        assert!(user.contains(r#"{"name":"postgresql","aliases":["postgres"]"#));
        assert!(user.contains("POSTGRESQL_HOST"));
        assert!(user.contains("service_manager: package|docker"));
        assert!(user.contains("devy subcommand: up, init,"), "{user}");
        assert!(
            user.contains("  - node:\n      version: \"22\"\n"),
            "{user}"
        );
        assert!(
            user.contains("  - postgresql:\n      version: \"16\"\n"),
            "{user}"
        );
        for path in [
            "package.json",
            ".nvmrc",
            "docker-compose.yml",
            ".env.example",
            "Makefile",
            "Procfile",
            "README.md",
            ".github/workflows/ci.yml",
        ] {
            assert!(
                user.contains(&format!("=== {path} ===\n")),
                "missing {path}"
            );
        }
    }

    #[test]
    fn prompt_omits_dotenv_and_unlisted_files() {
        let dir = sample();
        let req = request(&dir, None);
        let all = req.render_preview();
        assert!(!all.contains("do-not-send-me"));
        assert!(!all.contains("notes.txt") && !all.contains("ignored"));
        assert!(!all.contains("OPENSSH"));
    }

    #[test]
    fn prompt_redacts_secrets() {
        let dir = sample();
        let user = request(&dir, None).messages[0].content.clone();
        assert!(user.contains("STRIPE_SECRET_KEY=<redacted>"), "{user}");
        assert!(!user.contains("sk_test_abc"));
    }

    #[test]
    fn large_file_is_truncated() {
        let big = format!("{{\"description\":\"{}\"}}", "x".repeat(40 * 1024));
        let dir = fixture(&[("package.json", &big)]);
        let user = request(&dir, None).messages[0].content.clone();
        let section = user.split("=== package.json ===\n").nth(1).unwrap();
        let (kept, rest) = section.split_once("\n… [truncated]\n").unwrap();
        assert!(kept.len() <= redact::FILE_CAP, "{}", kept.len());
        assert!(rest.is_empty());
    }
}
