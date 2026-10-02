//! `environment:` from example dotenv files, pointed at devy's injected service variables.
//!
//! Only example files are read. `.env`, `.env.local` and friends hold real values.

use std::path::Path;

use super::{Detector, Draft, read};
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
                let value = rewrite(&key, &value, &services);
                let keep = !value.is_empty() && !redact::is_secret(&key, &value);
                draft.add_env(&key, keep.then_some(value));
            }
        }
    }
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
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
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
        // Two services and a key that names neither: left as written.
        assert_eq!(get(&e, "PGHOST").as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn single_service_claims_bare_host() {
        let e = env(&["postgresql"], &[(".env.example", "DB_HOST=localhost\n")]);
        assert_eq!(get(&e, "DB_HOST").as_deref(), Some("${POSTGRESQL_HOST}"));
    }

    #[test]
    fn values_without_a_detected_service_are_kept() {
        let content = "APP_URL=http://localhost:3000\nLOG_LEVEL=debug # verbose\nNAME=\"my app\"\n";
        let e = env(&["postgresql"], &[(".env.example", content)]);
        assert_eq!(get(&e, "APP_URL").as_deref(), Some("http://localhost:3000"));
        assert_eq!(get(&e, "LOG_LEVEL").as_deref(), Some("debug"));
        assert_eq!(get(&e, "NAME").as_deref(), Some("my app"));
    }

    #[test]
    fn port_inside_longer_number_not_rewritten() {
        let e = env(
            &["redis"],
            &[(".env.example", "X=http://localhost:63790\n")],
        );
        assert_eq!(get(&e, "X").as_deref(), Some("http://localhost:63790"));
    }

    #[test]
    fn secret_and_empty_values_become_todos() {
        let content = "STRIPE_SECRET_KEY=sk_test_abc\nSENTRY_DSN=\nURL=postgres://app:pw@db/app\n";
        let e = env(&[], &[(".env.example", content)]);
        assert_eq!(
            e,
            vec![
                ("STRIPE_SECRET_KEY".into(), None),
                ("SENTRY_DSN".into(), None),
                ("URL".into(), None),
            ]
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
        assert_eq!(e, vec![("PORT".into(), Some("3000".into()))]);
    }

    #[test]
    fn sample_and_template_files_are_read() {
        let e = env(
            &[],
            &[(".env.sample", "A=1\n"), (".env.template", "B=2\nA=3\n")],
        );
        assert_eq!(
            e,
            vec![
                ("A".into(), Some("1".into())),
                ("B".into(), Some("2".into()))
            ]
        );
    }
}
