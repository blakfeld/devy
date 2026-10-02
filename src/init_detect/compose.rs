//! Service dependencies from Docker Compose images.

use std::path::Path;

use super::{Detector, Draft, read, version_of};
use crate::modules;

const FILES: &[&str] = &[
    "compose.yaml",
    "compose.yml",
    "docker-compose.yml",
    "docker-compose.yaml",
];

/// Image repositories whose name differs from a devy module name or alias.
const IMAGE_NAMES: &[(&str, &str)] = &[
    ("bitnami/kafka", "kafka"),
    ("apache/kafka", "kafka"),
    ("confluentinc/cp-kafka", "kafka"),
    ("elasticsearch/elasticsearch", "elasticsearch"),
    ("opensearchproject/opensearch", "opensearch"),
    ("getmeili/meilisearch", "meilisearch"),
    ("minio/minio", "minio"),
    ("mailhog/mailhog", "mailhog"),
    ("hashicorp/vault", "vault"),
];

pub struct Compose;

impl Detector for Compose {
    fn detect(&self, dir: &Path, draft: &mut Draft) {
        let Some((file, content)) = FILES.iter().find_map(|f| Some((*f, read(dir, f)?))) else {
            return;
        };
        let doc: serde_yml::Value = match serde_yml::from_str(&content) {
            Ok(doc) => doc,
            Err(_) => {
                draft.todo(format!(
                    "could not parse `{file}`; add its services by hand"
                ));
                return;
            }
        };
        let Some(services) = doc.get("services").and_then(|s| s.as_mapping()) else {
            return;
        };
        let native = native_tooling(dir);
        // Services this detector added; ones found earlier stay as they were detected.
        let mut added: Vec<&str> = Vec::new();
        for (service, spec) in services {
            // Build-only services are the project itself, not a dependency.
            let Some(image) = spec.get("image").and_then(|i| i.as_str()) else {
                continue;
            };
            match map_image(image) {
                Some((name, version)) => {
                    if !draft.has_dep(name) {
                        added.push(name);
                    }
                    draft.add_dep(name, version);
                }
                None => draft.todo(format!(
                    "compose service `{}` uses image `{image}`, which devy has no module for",
                    service.as_str().unwrap_or("?")
                )),
            }
        }
        if let Some(evidence) = native {
            if !added.is_empty() {
                draft.todo(format!(
                    "compose services are left on the package manager because of `{evidence}`; \
                     add `service_manager: docker` to run them as containers"
                ));
            }
            return;
        }
        for dep in draft
            .deps
            .iter_mut()
            .filter(|d| added.contains(&d.name.as_str()))
        {
            dep.docker = can_run_in_docker(&dep.name);
        }
    }
}

/// The first sign that the project already manages tools natively (Nix, devbox, brew).
fn native_tooling(dir: &Path) -> Option<&'static str> {
    const FILES: &[&str] = &[
        "flake.nix",
        "shell.nix",
        "default.nix",
        "devbox.json",
        "Brewfile",
    ];
    if let Some(file) = FILES.iter().find(|f| dir.join(f).is_file()) {
        return Some(file);
    }
    // Only searched for direnv's nix directives; nothing else from .envrc is used.
    let envrc = read(dir, ".envrc")?;
    envrc
        .lines()
        .map(str::trim)
        .any(|l| l.starts_with("use nix") || l.starts_with("use flake"))
        .then_some(".envrc")
}

fn can_run_in_docker(name: &str) -> bool {
    let dep = crate::config::Dependency::simple(name);
    matches!(modules::get(name).docker_spec(&dep), Ok(Some(_)))
}

/// Maps an image reference to a service module and, when the tag is a version, its version.
fn map_image(image: &str) -> Option<(&'static str, Option<String>)> {
    let image = image.split('@').next().unwrap_or(image);
    let (repo, tag) = match image.rfind(':') {
        Some(i) if !image[i..].contains('/') => (&image[..i], Some(&image[i + 1..])),
        _ => (image, None),
    };
    let mut segments: Vec<&str> = repo.split('/').collect();
    if segments.len() > 1 && (segments[0].contains(['.', ':']) || segments[0] == "localhost") {
        segments.remove(0);
    }
    if segments.first() == Some(&"library") {
        segments.remove(0);
    }
    let repo = segments.join("/");
    let name = IMAGE_NAMES
        .iter()
        .find(|(image, _)| *image == repo)
        .map(|(_, name)| *name)
        .or(match segments.as_slice() {
            [single] => Some(*single),
            ["bitnami", name] => Some(*name),
            _ => None,
        })?;
    let canonical = modules::catalog()
        .into_iter()
        .find(|e| e.name == modules::canonical_name(name) && e.service)?
        .name;
    let version = tag
        .filter(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        .and_then(|t| version_of(t.split('-').next().unwrap_or(t)));
    Some((canonical, version))
}

#[cfg(test)]
mod tests {
    use super::super::test_util::fixture;
    use super::*;

    /// `name@version`, or just `name` when the tag is not a version.
    fn mapped(image: &str) -> Option<String> {
        map_image(image).map(|(n, v)| match v {
            Some(v) => format!("{n}@{v}"),
            None => n.to_string(),
        })
    }

    #[test]
    fn maps_official_and_vendor_images() {
        assert_eq!(mapped("postgres:16"), Some("postgresql@16".into()));
        assert_eq!(mapped("redis:7-alpine"), Some("redis@7".into()));
        assert_eq!(mapped("mongo:7"), Some("mongodb@7".into()));
        assert_eq!(mapped("bitnami/kafka:3.7"), Some("kafka@3.7".into()));
        assert_eq!(
            mapped("docker.elastic.co/elasticsearch/elasticsearch:8.13.0"),
            Some("elasticsearch@8.13.0".into())
        );
        assert_eq!(
            mapped("docker.io/library/mysql:8.0"),
            Some("mysql@8.0".into())
        );
        assert_eq!(mapped("rabbitmq:3-management"), Some("rabbitmq@3".into()));
    }

    #[test]
    fn non_version_tags_are_dropped() {
        assert_eq!(mapped("redis:latest"), Some("redis".into()));
        assert_eq!(mapped("redis:alpine"), Some("redis".into()));
        assert_eq!(mapped("redis"), Some("redis".into()));
        assert_eq!(mapped("redis@sha256:abc"), Some("redis".into()));
        assert_eq!(mapped("localhost:5000/redis:7"), Some("redis@7".into()));
    }

    #[test]
    fn unknown_and_non_service_images_are_unmapped() {
        assert_eq!(mapped("myorg/api:latest"), None);
        assert_eq!(mapped("node:22"), None, "runtimes are not compose services");
        assert_eq!(mapped("myorg/postgres:16"), None);
    }

    const PG: &str = "services:\n  db:\n    image: postgres:16\n";

    fn detect(files: &[(&str, &str)]) -> Draft {
        let dir = fixture(files);
        let mut draft = Draft::default();
        Compose.detect(&dir, &mut draft);
        draft
    }

    #[test]
    fn compose_services_run_in_docker() {
        let draft = detect(&[("docker-compose.yml", PG)]);
        assert_eq!(draft.deps.len(), 1);
        assert!(draft.deps[0].docker, "{draft:?}");
        assert!(draft.todos.is_empty(), "{:?}", draft.todos);
    }

    #[test]
    fn native_tooling_keeps_package_manager() {
        for (file, content) in [
            ("flake.nix", "{}"),
            ("Brewfile", "brew \"jq\"\n"),
            (".envrc", "export FOO=1\nuse flake\n"),
        ] {
            let draft = detect(&[("docker-compose.yml", PG), (file, content)]);
            assert!(!draft.deps[0].docker, "{file}");
            assert_eq!(draft.todos.len(), 1, "{file}: {:?}", draft.todos);
            assert!(
                draft.todos[0].contains(file) && draft.todos[0].contains("service_manager: docker"),
                "{:?}",
                draft.todos
            );
        }
    }

    #[test]
    fn envrc_without_nix_is_not_native_tooling() {
        let draft = detect(&[("docker-compose.yml", PG), (".envrc", "export X=1\n")]);
        assert!(draft.deps[0].docker);
    }

    #[test]
    fn service_detected_earlier_is_left_alone() {
        let dir = fixture(&[("docker-compose.yml", PG)]);
        let mut draft = Draft::default();
        draft.add_dep("postgres", Some("16.2".into()));
        Compose.detect(&dir, &mut draft);
        assert_eq!(draft.deps.len(), 1);
        assert!(!draft.deps[0].docker);
        assert_eq!(draft.deps[0].version.as_deref(), Some("16.2"));
    }

    #[test]
    fn duplicate_compose_services_stay_docker() {
        let compose = "services:\n  a:\n    image: redis:7\n  b:\n    image: redis:7\n";
        let draft = detect(&[("compose.yaml", compose)]);
        assert_eq!(draft.deps.len(), 1);
        assert!(draft.deps[0].docker);
    }

    #[test]
    fn every_mappable_service_can_run_in_docker() {
        for entry in modules::catalog().into_iter().filter(|e| e.service) {
            assert!(can_run_in_docker(entry.name), "{}", entry.name);
        }
    }

    #[test]
    fn compose_file_yields_services_and_todos() {
        let compose = "services:\n  db:\n    image: postgres:16\n  cache:\n    image: redis:7-alpine\n  api:\n    image: myorg/api:latest\n  web:\n    build: .\n";
        let dir = fixture(&[("docker-compose.yml", compose)]);
        let mut draft = Draft::default();
        Compose.detect(&dir, &mut draft);
        let names: Vec<_> = draft.deps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["postgresql", "redis"]);
        assert_eq!(draft.todos.len(), 1);
        assert!(draft.todos[0].contains("`api`") && draft.todos[0].contains("myorg/api:latest"));
    }

    #[test]
    fn compose_yaml_is_preferred_name() {
        let dir = fixture(&[("compose.yaml", "services:\n  m:\n    image: mongo:7\n")]);
        let mut draft = Draft::default();
        Compose.detect(&dir, &mut draft);
        assert_eq!(draft.deps[0].name, "mongodb");
    }

    #[test]
    fn unparseable_compose_becomes_todo() {
        let dir = fixture(&[("docker-compose.yml", "services: [\n")]);
        let mut draft = Draft::default();
        Compose.detect(&dir, &mut draft);
        assert!(draft.deps.is_empty());
        assert!(draft.todos[0].contains("could not parse"));
    }
}
