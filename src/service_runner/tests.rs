use super::docker::{FakeRunner, fail, ok};
use super::*;
use crate::config::ExtraValue;
use crate::lock::LockedDep;
use crate::package_manager::MockPackageManager;
use std::collections::BTreeMap;

const ROOT: &str = "/src/app";

fn config(yaml: &str) -> DevyConfig {
    serde_yml::from_str(yaml).unwrap()
}

fn docker_dep(name: &str, port: Option<u16>) -> Dependency {
    let mut dep = Dependency::simple(name);
    dep.docker = true;
    if let Some(p) = port {
        dep.extra
            .insert("port".into(), ExtraValue::Number(p.into()));
    }
    dep
}

fn docker_runner<'a>(
    fake: &'a FakeRunner,
    yaml: &str,
    lock: Option<&LockFile>,
    update: bool,
) -> DockerRunner<'a> {
    let config = config(yaml);
    DockerRunner::new(
        ContainerRuntime::new(config.container_cli, fake),
        &config,
        Path::new(ROOT),
        lock,
        update,
    )
}

fn slug() -> String {
    modules::helpers::project_slug("app", Path::new(ROOT))
}

fn redis_lock(tag: &str, digest: &str) -> LockFile {
    let mut deps = BTreeMap::new();
    deps.insert(
        "redis".into(),
        LockedDep {
            resolved_version: Some(tag.into()),
            source: DOCKER_SOURCE.into(),
            assigned_port: Some(51000),
            image_digest: Some(digest.into()),
        },
    );
    LockFile {
        dependencies: deps,
        ..Default::default()
    }
}

/// `container inspect` output for a container with the given running state and labels.
fn inspect_json(running: bool, labels: &[(String, String)]) -> String {
    let labels: serde_json::Map<String, serde_json::Value> = labels
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    serde_json::json!({"State": {"Running": running}, "Config": {"Labels": labels}}).to_string()
}

// ── runner selection ─────────────────────────────────────────────────────────

fn labels_for(yaml: &str) -> Vec<(String, Option<&'static str>)> {
    let config = config(yaml);
    let pm = MockPackageManager::default();
    let fake = FakeRunner::ok();
    let runners = Runners::new(
        &pm,
        ContainerRuntime::new(config.container_cli, &fake),
        &config,
        Path::new(ROOT),
        None,
        false,
    );
    let labels = config
        .normalized_dependencies()
        .unwrap()
        .iter()
        .map(|d| (d.name.clone(), runners.runner_for(d).label()))
        .collect();
    assert!(fake.lines().is_empty(), "selection must not run the CLI");
    labels
}

#[test]
fn runner_for_top_level_docker() {
    assert_eq!(
        labels_for("service_manager: docker\ndependencies:\n  - redis\n  - jq\n"),
        vec![("redis".into(), Some("docker")), ("jq".into(), None)]
    );
}

#[test]
fn runner_for_per_dependency_opt_in() {
    assert_eq!(
        labels_for("dependencies:\n  - postgres: { service_manager: docker }\n  - redis\n"),
        vec![("postgres".into(), Some("docker")), ("redis".into(), None)]
    );
}

#[test]
fn runner_for_per_dependency_opt_out() {
    assert_eq!(
        labels_for(
            "service_manager: docker\ndependencies:\n  - redis: { service_manager: package }\n"
        ),
        vec![("redis".into(), None)]
    );
}

#[test]
fn package_runner_delegates_to_module_and_pm() {
    let pm = MockPackageManager {
        service_running: true,
        installed: true,
        ..Default::default()
    };
    let runner = PackageRunner::new(&pm, Path::new(ROOT));
    let dep = Dependency::simple("redis");
    assert!(runner.is_installed(&dep).unwrap());
    assert!(runner.is_running(&dep).unwrap());
    runner.stop(&dep).unwrap();
    assert_eq!(*pm.stopped_services.borrow(), vec!["redis".to_string()]);
    assert!(!runner.remove(&dep, true).unwrap(), "nothing to remove");
    assert_eq!(runner.label(), None);
}

#[test]
fn ensure_docker_available_only_with_docker_deps() {
    let pm = MockPackageManager::default();
    let fake = FakeRunner::new(|_| fail("daemon down"));
    let config = config("dependencies: []\n");
    let runners = Runners::new(
        &pm,
        ContainerRuntime::new(config.container_cli, &fake),
        &config,
        Path::new(ROOT),
        None,
        false,
    );
    runners
        .ensure_docker_available(&[Dependency::simple("redis")])
        .unwrap();
    assert!(fake.lines().is_empty());
    let err = runners
        .ensure_docker_available(&[docker_dep("redis", None)])
        .unwrap_err();
    assert!(err.to_string().starts_with("docker is not available"));
}

// ── naming and image references ──────────────────────────────────────────────

#[test]
fn container_name_is_per_project() {
    let fake = FakeRunner::ok();
    let runner = docker_runner(&fake, "name: app\n", None, false);
    assert_eq!(
        runner.container_name(&docker_dep("postgres", None)),
        format!("devy-{}-postgresql", slug())
    );
}

#[test]
fn reference_uses_locked_digest_when_tag_unchanged() {
    let fake = FakeRunner::ok();
    let lock = redis_lock("7", "redis@sha256:abc");
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), false);
    assert_eq!(
        runner.reference(&docker_dep("redis", None)).unwrap(),
        "redis@sha256:abc"
    );
}

#[test]
fn reference_resolves_tag_again_when_tag_or_repository_changes() {
    let fake = FakeRunner::ok();
    let lock = redis_lock("7", "redis@sha256:abc");
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), false);
    let mut dep = docker_dep("redis", None);
    dep.version = Some("7.2".into());
    assert_eq!(runner.reference(&dep).unwrap(), "redis:7.2");
    let mut dep = docker_dep("redis", None);
    dep.image = Some("mirror/redis".into());
    assert_eq!(runner.reference(&dep).unwrap(), "mirror/redis:7");
}

#[test]
fn reference_ignores_lock_with_update() {
    let fake = FakeRunner::ok();
    let lock = redis_lock("7", "redis@sha256:abc");
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), true);
    assert_eq!(
        runner.reference(&docker_dep("redis", None)).unwrap(),
        "redis:7"
    );
}

#[test]
fn reference_ignores_lock_entries_from_the_package_manager() {
    let fake = FakeRunner::ok();
    let mut lock = redis_lock("7", "redis@sha256:abc");
    lock.dependencies.get_mut("redis").unwrap().source = "homebrew".into();
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), false);
    assert_eq!(
        runner.reference(&docker_dep("redis", None)).unwrap(),
        "redis:7"
    );
}

// ── install and resolve ──────────────────────────────────────────────────────

#[test]
fn install_pulls_and_resolved_records_digest() {
    let fake = FakeRunner::new(|call| match call[1].as_str() {
        "image" if call.len() == 4 => fail("No such image"),
        "image" => ok(r#"["redis@sha256:abc"]"#),
        _ => ok(""),
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let dep = docker_dep("redis", None);
    assert!(!runner.is_installed(&dep).unwrap());
    runner.install(&dep).unwrap();
    assert_eq!(
        runner.resolved(&dep).unwrap(),
        (Some("7".into()), Some("redis@sha256:abc".into()))
    );
    // Containers are created from the digest just recorded in the lock.
    assert_eq!(runner.reference(&dep).unwrap(), "redis@sha256:abc");
    assert_eq!(
        fake.lines(),
        vec![
            "docker image inspect redis:7",
            "docker pull redis:7",
            "docker image inspect --format {{json .RepoDigests}} redis:7",
        ]
    );
}

#[test]
fn teammate_pulls_the_locked_digest() {
    let fake = FakeRunner::new(|call| {
        if call[1] == "image" {
            fail("No such image")
        } else {
            ok("")
        }
    });
    let lock = redis_lock("7", "redis@sha256:abc");
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), false);
    let dep = docker_dep("redis", None);
    assert!(!runner.is_installed(&dep).unwrap());
    runner.install(&dep).unwrap();
    assert_eq!(
        runner.resolved(&dep).unwrap(),
        (Some("7".into()), Some("redis@sha256:abc".into()))
    );
    assert_eq!(
        fake.lines(),
        vec![
            "docker image inspect redis@sha256:abc",
            "docker pull redis@sha256:abc",
        ]
    );
}

#[test]
fn update_always_pulls_the_tag() {
    let fake = FakeRunner::new(|call| match call[1].as_str() {
        "image" => ok(r#"["redis@sha256:new"]"#),
        _ => ok(""),
    });
    let lock = redis_lock("7", "redis@sha256:old");
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), true);
    let dep = docker_dep("redis", None);
    assert!(!runner.is_installed(&dep).unwrap(), "--update re-pulls");
    runner.install(&dep).unwrap();
    assert_eq!(
        runner.resolved(&dep).unwrap().1.as_deref(),
        Some("redis@sha256:new")
    );
    assert_eq!(fake.lines()[0], "docker pull redis:7");
}

#[test]
fn pull_failure_names_reference() {
    let fake = FakeRunner::new(|_| fail("denied"));
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let err = runner.install(&docker_dep("redis", None)).unwrap_err();
    assert_eq!(err.to_string(), "Failed to pull redis:7");
}

// ── start / stop / remove ────────────────────────────────────────────────────

#[test]
fn start_creates_missing_container() {
    let fake = FakeRunner::new(|call| {
        if call[1] == "container" {
            fail("Error: No such container: x")
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let dep = docker_dep("redis", Some(51000));
    runner.start(&dep).unwrap();
    let name = format!("devy-{}-redis", slug());
    let run = runner.run_spec(&dep).unwrap();
    let hash = &run
        .labels
        .iter()
        .find(|(k, _)| k == CONFIG_LABEL)
        .unwrap()
        .1;
    assert_eq!(
        fake.lines(),
        vec![
            format!("docker container inspect --format {{{{json .}}}} {name}"),
            format!(
                "docker run -d --name {name} --hostname redis \
                 --label sh.devy.project={ROOT} --label sh.devy.service=redis \
                 --label sh.devy.config={hash} -p 127.0.0.1:51000:6379 \
                 -v {name}:/data redis:7"
            ),
        ]
    );
}

#[test]
fn start_reuses_container_with_same_config() {
    let fake_for_spec = FakeRunner::ok();
    let dep = docker_dep("redis", Some(51000));
    let labels = docker_runner(&fake_for_spec, "name: app\n", None, false)
        .run_spec(&dep)
        .unwrap()
        .labels;
    let stopped = inspect_json(false, &labels);
    let fake = FakeRunner::new(move |call| {
        if call[1] == "container" {
            ok(&stopped)
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    runner.start(&dep).unwrap();
    let lines = fake.lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1], format!("docker start devy-{}-redis", slug()));
}

#[test]
fn start_leaves_running_container_with_same_config() {
    let fake_for_spec = FakeRunner::ok();
    let dep = docker_dep("redis", Some(51000));
    let labels = docker_runner(&fake_for_spec, "name: app\n", None, false)
        .run_spec(&dep)
        .unwrap()
        .labels;
    let running = inspect_json(true, &labels);
    let fake = FakeRunner::new(move |_| ok(&running));
    docker_runner(&fake, "name: app\n", None, false)
        .start(&dep)
        .unwrap();
    assert_eq!(fake.lines().len(), 1, "only the inspect");
}

#[test]
fn start_recreates_container_when_port_changes_and_keeps_volume() {
    let fake_for_spec = FakeRunner::ok();
    let old_labels = docker_runner(&fake_for_spec, "name: app\n", None, false)
        .run_spec(&docker_dep("redis", Some(51000)))
        .unwrap()
        .labels;
    let running = inspect_json(true, &old_labels);
    let fake = FakeRunner::new(move |call| {
        if call[1] == "container" {
            ok(&running)
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    runner.start(&docker_dep("redis", Some(6380))).unwrap();
    let name = format!("devy-{}-redis", slug());
    let lines = fake.lines();
    assert_eq!(lines[1], format!("docker rm -f {name}"));
    assert!(lines[2].starts_with("docker run -d"), "{lines:?}");
    assert!(lines[2].contains("-p 127.0.0.1:6380:6379"), "{lines:?}");
    assert!(lines[2].contains(&format!("-v {name}:/data")), "{lines:?}");
    assert!(
        !lines.iter().any(|l| l.contains("volume rm")),
        "the volume must be kept"
    );
}

#[test]
fn config_hash_changes_with_env_and_image_but_not_env_order() {
    let fake = FakeRunner::ok();
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let base = runner.run_spec(&docker_dep("redis", Some(51000))).unwrap();
    let mut reordered = base.clone();
    reordered.env = vec![("B".into(), "2".into()), ("A".into(), "1".into())];
    let mut sorted = base.clone();
    sorted.env = vec![("A".into(), "1".into()), ("B".into(), "2".into())];
    assert_eq!(config_hash(&reordered), config_hash(&sorted));
    let mut other_image = base.clone();
    other_image.image = "redis@sha256:abc".into();
    assert_ne!(config_hash(&base), config_hash(&other_image));
    assert_ne!(config_hash(&base), config_hash(&sorted));
}

#[test]
fn start_publishes_minio_console_port() {
    let fake = FakeRunner::new(|call| {
        if call[1] == "container" {
            fail("No such container")
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let mut dep = docker_dep("minio", Some(51000));
    dep.extra
        .insert("console_port".into(), ExtraValue::Number(51001u64.into()));
    runner.start(&dep).unwrap();
    let run = &fake.lines()[1];
    assert!(
        run.contains("-p 127.0.0.1:51000:9000 -p 127.0.0.1:51001:9001"),
        "{run}"
    );
}

#[test]
fn podman_warns_for_privileged_host_ports() {
    let fake = FakeRunner::new(|call| {
        if call[1] == "container" {
            fail("no such container")
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\ncontainer_cli: podman\n", None, false);
    let warnings = crate::output::with_warn_messages(|| {
        runner.start(&docker_dep("nginx", Some(80))).unwrap();
    });
    assert_eq!(
        warnings,
        vec![
            "nginx: rootless podman cannot publish port 80 — set a port >= 1024 in devy.yml if this fails"
        ]
    );
    assert!(fake.lines()[1].starts_with("podman run -d"));
}

#[test]
fn run_failure_surfaces_cli_error() {
    let fake = FakeRunner::new(|call| match call[1].as_str() {
        "container" => fail("No such container"),
        _ => fail("Bind for 127.0.0.1:51000 failed: port is already allocated"),
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let err = runner.start(&docker_dep("redis", Some(51000))).unwrap_err();
    assert!(err.to_string().contains("port is already allocated"));
}

#[test]
fn is_running_reads_container_state() {
    let fake = FakeRunner::new(|_| ok(r#"{"State":{"Running":true},"Config":{"Labels":{}}}"#));
    assert!(
        docker_runner(&fake, "name: app\n", None, false)
            .is_running(&docker_dep("redis", None))
            .unwrap()
    );
    let fake = FakeRunner::new(|_| fail("No such container"));
    assert!(
        !docker_runner(&fake, "name: app\n", None, false)
            .is_running(&docker_dep("redis", None))
            .unwrap()
    );
}

#[test]
fn stop_and_remove() {
    let fake = FakeRunner::ok();
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let dep = docker_dep("postgres", None);
    runner.stop(&dep).unwrap();
    assert!(runner.remove(&dep, false).unwrap());
    assert!(runner.remove(&dep, true).unwrap());
    let name = format!("devy-{}-postgresql", slug());
    assert_eq!(
        fake.lines(),
        vec![
            format!("docker stop {name}"),
            format!("docker rm -f {name}"),
            format!("docker rm -f {name}"),
            format!("docker volume rm {name}"),
        ]
    );
}

#[test]
fn docker_warnings_only_for_docker_managed() {
    let mut kafka = Dependency::simple("kafka");
    assert!(docker_warnings(&kafka).is_empty());
    kafka.docker = true;
    assert_eq!(docker_warnings(&kafka).len(), 1);
}
