//! `environment:` from example dotenv files, pointed at devy's injected service variables.
//!
//! Only example files are read. `.env`, `.env.local` and friends hold real values.
//!
//! Only keys whose value was rewritten to a detected service's `${<NAME>_HOST}` /
//! `${<NAME>_PORT}` are written. Every other key becomes a `# TODO: review suggested
//! environment <KEY>` comment: the file is repository text, and keys like
//! `NODE_OPTIONS`, `LD_PRELOAD` or `BASH_ENV` change what runs in the project shell.

use std::path::Path;

use super::{Detector, Draft, read, snippet};
use crate::ai::redact;
use crate::modules;

pub(crate) const FILES: &[&str] = &[".env.example", ".env.sample", ".env.template"];

const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1"];

/// A detected service with a known port: its variable prefix and default port.
struct Service {
    prefix: String,
    names: Vec<String>,
    port: u16,
}

pub struct EnvExample;

impl Detector for EnvExample {
    fn detect(&self, dir: &Path, draft: &mut Draft) {
        let services = detected_services(draft);
        for file in FILES {
            let Some(content) = read(dir, file) else {
                continue;
            };
            for (key, value) in parse(&content) {
                let todo = format!("review suggested environment {}", snippet(&key));
                let rewritten = rewrite(&key, &value, &services);
                if rewritten != value
                    && connection_key(&key)
                    && points_at_service(&rewritten)
                    && !redact::is_secret(&key, &rewritten)
                {
                    draft.add_env(&key, Some(rewritten));
                    // An earlier example file may have listed it for review.
                    draft.todos.retain(|t| *t != todo);
                } else if !draft.env.iter().any(|(k, _)| *k == key) {
                    draft.todo(todo);
                }
            }
        }
    }
}

/// Keys that name where a service is (`DATABASE_URL`, `REDIS_HOST`, `PGPORT`), the only
/// keys whose rewritten value is written: a host and port spliced into `NODE_OPTIONS`
/// or `LD_PRELOAD` must not carry the rest of that value into `devy.yml`. Matched on the
/// key's last word (`PGHOST`-style compounds count; `PASSPORT` and `TRANSPORT` do not).
fn connection_key(key: &str) -> bool {
    const WORDS: &[&str] = &[
        "HOST", "PORT", "URL", "URI", "ADDR", "ADDRESS", "SERVER", "ENDPOINT", "DSN",
    ];
    const COMPOUNDS: &[&str] = &["PGHOST", "PGPORT", "DBHOST", "DBPORT", "DBURL"];
    redact::key_words(key)
        .last()
        .is_some_and(|w| WORDS.contains(&w.as_str()) || COMPOUNDS.contains(&w.as_str()))
}

/// Whether `value` (already rewritten) points only at a detected service: exactly
/// `${X_HOST}`, `${X_PORT}` or `${X_HOST}:${X_PORT}`, or a URL
/// `scheme://[user[:password]@]${X_HOST}[:${X_PORT}][/path]` with a plain path and no
/// query, so `https://evil.example/simple?x=localhost:6379` is not written.
fn points_at_service(value: &str) -> bool {
    static BARE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"^(?:\$\{[A-Z0-9_]+_(?:HOST|PORT)\}|\$\{[A-Z0-9_]+_HOST\}:\$\{[A-Z0-9_]+_PORT\})$",
        )
        .expect("valid regex")
    });
    static URL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"^[a-z][a-z0-9+.\-]*://(?:[A-Za-z0-9._~%\-]+(?::[^@/\s]*)?@)?\$\{[A-Z0-9_]+_HOST\}(?::\$\{[A-Z0-9_]+_PORT\})?(?:/[A-Za-z0-9._~%/\-]*)?$",
        )
        .expect("valid regex")
    });
    BARE.is_match(value) || URL.is_match(value)
}

fn detected_services(draft: &Draft) -> Vec<Service> {
    modules::catalog()
        .into_iter()
        .filter(|e| e.service && draft.has_dep(e.name))
        .filter_map(|e| {
            let port = e.default_port?;
            let mut names: Vec<String> = std::iter::once(e.name)
                .chain(e.aliases.iter().copied())
                .map(|n| n.replace('-', "_").to_ascii_uppercase())
                .collect();
            names.dedup();
            Some(Service {
                prefix: e.name.replace('-', "_").to_ascii_uppercase(),
                names,
                port,
            })
        })
        .collect()
}

/// `KEY=value` lines, with `export`, quotes and trailing ` # comments` removed.
fn parse(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if !crate::validate::env_key(key) {
                return None;
            }
            let value = value.trim();
            let value = match value.chars().next() {
                Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or_default(),
                _ => value.split(" #").next().unwrap_or_default().trim(),
            };
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Replaces a local host and/or a service's default port with that service's injected
/// `${<PREFIX>_HOST}` / `${<PREFIX>_PORT}` variables.
fn rewrite(key: &str, value: &str, services: &[Service]) -> String {
    let host_var = |s: &Service| format!("${{{}_HOST}}", s.prefix);
    let port_var = |s: &Service| format!("${{{}_PORT}}", s.prefix);

    // A bare port.
    if let Some(s) = services.iter().find(|s| value == s.port.to_string()) {
        return port_var(s);
    }
    // A bare host: the service the key names, or the only service there is.
    if LOCAL_HOSTS.contains(&value) {
        let upper = key.to_ascii_uppercase();
        let named: Vec<&Service> = services
            .iter()
            .filter(|s| s.names.iter().any(|n| upper.contains(n.as_str())))
            .collect();
        let target = match (named.as_slice(), services) {
            ([one], _) => Some(*one),
            ([], [only]) => Some(only),
            _ => None,
        };
        return target.map_or_else(|| value.to_string(), host_var);
    }
    // `host:port` anywhere, such as inside a URL.
    let mut out = value.to_string();
    for s in services {
        for host in LOCAL_HOSTS {
            let needle = format!("{host}:{}", s.port);
            let mut rewritten = String::with_capacity(out.len());
            let mut rest = out.as_str();
            while let Some(i) = rest.find(&needle) {
                let end = i + needle.len();
                let boundary_before =
                    !rest[..i].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '.');
                let boundary_after = !rest[end..].starts_with(|c: char| c.is_ascii_digit());
                rewritten.push_str(&rest[..i]);
                if boundary_before && boundary_after {
                    rewritten.push_str(&format!("{}:{}", host_var(s), port_var(s)));
                } else {
                    rewritten.push_str(&needle);
                }
                rest = &rest[end..];
            }
            rewritten.push_str(rest);
            out = rewritten;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::test_util::fixture;
    use super::*;

    fn env(services: &[&str], files: &[(&str, &str)]) -> Vec<(String, Option<String>)> {
        let dir = fixture(files);
        let mut draft = Draft::default();
        for s in services {
            draft.add_dep(s, None);
        }
        EnvExample.detect(&dir, &mut draft);
        draft.env
    }

    fn detect_env(
        services: &[&str],
        files: &[(&str, &str)],
    ) -> (Vec<(String, Option<String>)>, Vec<String>) {
        let dir = fixture(files);
        let mut draft = Draft::default();
        for s in services {
            draft.add_dep(s, None);
        }
        EnvExample.detect(&dir, &mut draft);
        (draft.env, draft.todos)
    }

    fn get(env: &[(String, Option<String>)], key: &str) -> Option<String> {
        env.iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.clone())
    }

    #[test]
    fn database_url_rewritten_to_injected_vars() {
        let e = env(
            &["postgres"],
            &[(
                ".env.example",
                "DATABASE_URL=postgres://localhost:5432/app\n",
            )],
        );
        assert_eq!(
            get(&e, "DATABASE_URL").as_deref(),
            Some("postgres://${POSTGRESQL_HOST}:${POSTGRESQL_PORT}/app")
        );
    }

    #[test]
    fn bare_host_and_port_rewritten() {
        let content = "REDIS_HOST=localhost\nPGHOST=127.0.0.1\nPGPORT=5432\nREDIS_URL=redis://127.0.0.1:6379/0\n";
        let e = env(&["redis", "postgresql"], &[(".env.example", content)]);
        assert_eq!(get(&e, "REDIS_HOST").as_deref(), Some("${REDIS_HOST}"));
        assert_eq!(get(&e, "PGPORT").as_deref(), Some("${POSTGRESQL_PORT}"));
        assert_eq!(
            get(&e, "REDIS_URL").as_deref(),
            Some("redis://${REDIS_HOST}:${REDIS_PORT}/0")
        );
        // Two services and a key that names neither: not rewritten, so not written.
        assert!(e.iter().all(|(k, _)| k != "PGHOST"), "{e:?}");
    }

    #[test]
    fn keys_that_are_not_valid_env_names_are_skipped() {
        let (e, todos) = detect_env(&["postgresql"], &[(".env.example", "1BAD=x\nGOOD=y\n")]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(todos, ["review suggested environment GOOD"]);
    }

    #[test]
    fn single_service_claims_bare_host() {
        let e = env(&["postgresql"], &[(".env.example", "DB_HOST=localhost\n")]);
        assert_eq!(get(&e, "DB_HOST").as_deref(), Some("${POSTGRESQL_HOST}"));
    }

    /// Scenario "Env example keys that are not rewritten become TODOs".
    #[test]
    fn values_without_a_detected_service_become_review_todos() {
        let content = "APP_URL=http://localhost:3000\nNODE_OPTIONS=--require ./x.js\nLD_PRELOAD=./evil.so\nBASH_ENV=./x.sh\nDB_URL=postgres://localhost:5432/app\n";
        let (e, todos) = detect_env(&["postgresql"], &[(".env.example", content)]);
        assert_eq!(
            e,
            vec![(
                "DB_URL".to_string(),
                Some("postgres://${POSTGRESQL_HOST}:${POSTGRESQL_PORT}/app".to_string())
            )]
        );
        assert_eq!(
            todos,
            [
                "review suggested environment APP_URL",
                "review suggested environment NODE_OPTIONS",
                "review suggested environment LD_PRELOAD",
                "review suggested environment BASH_ENV",
            ]
        );
    }

    #[test]
    fn port_inside_longer_number_not_rewritten() {
        let e = env(
            &["redis"],
            &[(".env.example", "X=http://localhost:63790\n")],
        );
        assert_eq!(get(&e, "X"), None);
    }

    #[test]
    fn secret_and_empty_values_become_todos() {
        let content = "STRIPE_SECRET_KEY=sk_test_abc\nSENTRY_DSN=\nURL=postgres://app:pw@localhost:5432/app\n";
        let (e, todos) = detect_env(&["postgresql"], &[(".env.example", content)]);
        // URL is rewritten, but still carries a password.
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(todos.len(), 3, "{todos:?}");
        assert!(
            todos
                .iter()
                .all(|t| !t.contains("sk_test") && !t.contains("pw"))
        );
    }

    #[test]
    fn real_dotenv_is_never_read() {
        let e = env(
            &[],
            &[
                (".env", "SENTINEL=do-not-read-me\n"),
                (".env.local", "SENTINEL2=do-not-read-me\n"),
                (".env.example", "PORT=3000\n"),
            ],
        );
        assert!(e.is_empty(), "{e:?}");
    }

    #[test]
    fn sample_and_template_files_are_read() {
        let (e, todos) = detect_env(
            &["redis"],
            &[
                (".env.sample", "A_PORT=6379\nD_URL=x\n"),
                (
                    ".env.template",
                    "B_HOST=localhost\nA_PORT=3\nC=x\nD_URL=redis://localhost:6379\n",
                ),
            ],
        );
        assert_eq!(
            e,
            vec![
                ("A_PORT".into(), Some("${REDIS_PORT}".into())),
                ("B_HOST".into(), Some("${REDIS_HOST}".into())),
                (
                    "D_URL".into(),
                    Some("redis://${REDIS_HOST}:${REDIS_PORT}".into())
                ),
            ]
        );
        // D_URL was listed for review by the first file, then rewritten by the second.
        assert_eq!(todos, ["review suggested environment C"]);
    }

    /// A host and port spliced into an execution-affecting value does not carry the
    /// rest of it into devy.yml.
    #[test]
    fn rewrites_into_non_connection_keys_or_commands_become_todos() {
        let content = concat!(
            "NODE_OPTIONS=--require ./evil.js --inspect=localhost:6379\n",
            "REDIS_URL=redis://localhost:6379;id\n",
            "PIP_INDEX_URL=https://evil.example/simple?x=localhost:6379\n",
            "UV_INDEX_URL=https://evil.example/localhost:6379\n",
            "DOCKER_HOST=ssh://attacker.example/localhost:6379\n",
            "TRANSPORT=localhost:6379\n",
        );
        let (e, todos) = detect_env(&["redis"], &[(".env.example", content)]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(todos.len(), 6, "{todos:?}");
    }
}
