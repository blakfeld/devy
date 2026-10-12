use super::docker::{FakeRunner, fail, ok};
use super::*;
use crate::config::ExtraValue;
use crate::lock::LockedDep;
use crate::package_manager::MockPackageManager;
use serde_norway as yaml;
use std::collections::BTreeMap;

const ROOT: &str = "/src/app";

fn config(yaml: &str) -> DevyConfig {
    yaml::from_str(yaml).unwrap()
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
    let mut runner = DockerRunner::new(
        ContainerRuntime::new(config.container_cli, fake),
        &config,
        Path::new(ROOT),
        lock,
        update,
    );
    // Pinned, so the tests don't depend on (or fail without) this machine's id.
    runner.set_host(Some(TEST_HOST));
    runner
}

/// The host id the tests run as.
const TEST_HOST: &str = "0123456789abcdef";

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
                 --label sh.devy.host={host} --label sh.devy.config={hash} \
                 -p 127.0.0.1:51000:6379 -v {name}:/data redis:7",
                host = TEST_HOST,
            ),
        ]
    );
}

#[test]
fn the_host_label_does_not_change_the_config_hash() {
    let fake = FakeRunner::ok();
    let run = docker_runner(&fake, "name: app\n", None, false)
        .run_spec(&docker_dep("redis", Some(51000)))
        .unwrap();
    assert!(
        run.labels
            .contains(&(HOST_LABEL.to_string(), TEST_HOST.to_string())),
        "{:?}",
        run.labels
    );
    let mut other_host = run.clone();
    for (k, v) in &mut other_host.labels {
        if k == HOST_LABEL {
            *v = "0000000000000000".into();
        }
    }
    assert_eq!(config_hash(&run), config_hash(&other_host));
}

#[test]
fn containers_without_the_host_label_are_still_this_projects() {
    // Created by an older devy: same project and config, but no `sh.devy.host`.
    let fake_for_spec = FakeRunner::ok();
    let dep = docker_dep("redis", Some(51000));
    let labels: Vec<(String, String)> = docker_runner(&fake_for_spec, "name: app\n", None, false)
        .run_spec(&dep)
        .unwrap()
        .labels
        .into_iter()
        .filter(|(k, _)| k != HOST_LABEL)
        .collect();
    let state = inspect_json(true, &labels);
    let fake = FakeRunner::new(move |call| {
        if call[1] == "container" {
            ok(&state)
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    // Running with the same config: reused, not recreated.
    runner.start(&dep).unwrap();
    runner.stop(&dep).unwrap();
    assert!(runner.remove(&dep, true).unwrap());
    let name = format!("devy-{}-redis", slug());
    let acting: Vec<String> = fake
        .lines()
        .into_iter()
        .filter(|l| !l.contains("container inspect"))
        .collect();
    assert_eq!(
        acting,
        [
            format!("docker stop {name}"),
            format!("docker rm -f {name}"),
            format!("docker volume rm {name}"),
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
fn start_refuses_container_from_another_project() {
    let fake_for_spec = FakeRunner::ok();
    let dep = docker_dep("redis", Some(51000));
    let own = docker_runner(&fake_for_spec, "name: app\n", None, false)
        .run_spec(&dep)
        .unwrap()
        .labels;
    // Same name and even the same config hash, but another project's root (or none).
    let foreign: Vec<(String, String)> = own
        .iter()
        .map(|(k, v)| {
            if k == PROJECT_LABEL {
                (k.clone(), "/src/evil".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect();
    let unlabeled: Vec<(String, String)> = own
        .iter()
        .filter(|(k, _)| k != PROJECT_LABEL)
        .cloned()
        .collect();
    for (labels, running) in [
        (foreign.clone(), true),
        (foreign, false),
        (unlabeled, false),
    ] {
        let state = inspect_json(running, &labels);
        let fake = FakeRunner::new(move |_| ok(&state));
        let err = docker_runner(&fake, "name: app\n", None, false)
            .start(&dep)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("container devy-{}-redis belongs to another project", slug())
        );
        assert_eq!(
            fake.lines().len(),
            1,
            "only the inspect: no start, rm or run"
        );
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_project_root_runs_no_container() {
    use std::os::unix::ffi::OsStrExt;
    let config = config("name: app\n");
    let root = Path::new(std::ffi::OsStr::from_bytes(b"/src/app-\xff"));
    // Even a container labeled with the lossy rendering of the root isn't this project's.
    let lossy = vec![(
        PROJECT_LABEL.to_string(),
        root.to_string_lossy().into_owned(),
    )];
    let state = inspect_json(true, &lossy);
    let fake = FakeRunner::new(move |_| ok(&state));
    let runner = DockerRunner::new(
        ContainerRuntime::new(config.container_cli, &fake),
        &config,
        root,
        None,
        false,
    );
    let dep = docker_dep("redis", Some(51000));
    let err = runner.run_spec(&dep).unwrap_err();
    assert!(err.to_string().contains("not valid UTF-8"), "{err}");
    let err = runner.start(&dep).unwrap_err();
    assert!(err.to_string().contains("not valid UTF-8"), "{err}");
    assert!(fake.lines().is_empty(), "{:?}", fake.lines());
    assert!(!runner.is_running(&dep).unwrap());
    assert!(runner.stop(&dep).is_err());
}

#[test]
fn start_passes_minio_credentials_via_env_file() {
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
        .insert("access_key".into(), ExtraValue::String("me".into()));
    dep.extra.insert(
        "secret_key".into(),
        ExtraValue::String("top-secret-pw".into()),
    );
    runner.start(&dep).unwrap();
    let run = &fake.lines()[1];
    assert!(run.starts_with("docker run -d"), "{run}");
    assert!(run.contains("--env-file "), "{run}");
    assert!(!run.contains("top-secret-pw"), "{run}");
    assert!(!run.contains("MINIO_ROOT"), "{run}");
    assert!(run.contains(MINIO_PIN), "{run}");
}

// ── built-in digest pin ──────────────────────────────────────────────────────

const MINIO_PIN: &str =
    "pgsty/silo@sha256:635197cb9f36d01bee221d34d1c7d7960f6a95c48b0b6c01d99cd13bdae51a46";
const MINIO_TAG: &str = "RELEASE.2026-09-16T00-00-00Z";

fn minio_lock(tag: &str, digest: &str) -> LockFile {
    let mut lock = redis_lock(tag, digest);
    let entry = lock.dependencies.remove("redis").unwrap();
    lock.dependencies.insert("minio".into(), entry);
    lock
}

#[test]
fn default_image_runs_the_pinned_digest() {
    let fake = FakeRunner::new(|call| {
        if call[1] == "container" {
            fail("No such container")
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let dep = docker_dep("minio", Some(51000));
    assert_eq!(runner.reference(&dep).unwrap(), MINIO_PIN);
    runner.start(&dep).unwrap();
    let run = &fake.lines()[1];
    assert!(run.contains(&format!(" {MINIO_PIN} server /data")), "{run}");
}

#[test]
fn version_or_image_override_skips_the_digest_pin() {
    let fake = FakeRunner::ok();
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let mut dep = docker_dep("minio", None);
    dep.version = Some(MINIO_TAG.into());
    assert_eq!(
        runner.reference(&dep).unwrap(),
        format!("pgsty/silo:{MINIO_TAG}")
    );
    let mut dep = docker_dep("minio", None);
    dep.image = Some("mirror.example/pgsty/silo".into());
    assert_eq!(
        runner.reference(&dep).unwrap(),
        format!("mirror.example/pgsty/silo:{MINIO_TAG}")
    );
}

#[test]
fn pinned_digest_round_trips_through_the_lock() {
    // First run: no lock. The pin is pulled and recorded without asking the runtime.
    let fake = FakeRunner::new(|call| match call[1].as_str() {
        "image" => fail("No such image"),
        _ => ok(""),
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let dep = docker_dep("minio", None);
    assert!(!runner.is_installed(&dep).unwrap());
    runner.install(&dep).unwrap();
    let (version, digest) = runner.resolved(&dep).unwrap();
    assert_eq!(
        (version.as_deref(), digest.as_deref()),
        (Some(MINIO_TAG), Some(MINIO_PIN))
    );
    assert_eq!(
        fake.lines(),
        vec![
            format!("docker image inspect {MINIO_PIN}"),
            format!("docker pull {MINIO_PIN}"),
        ]
    );
    // Next run, from that lock entry (serialized and parsed back): still the pin.
    let lock =
        LockFile::parse(&yaml::to_string(&minio_lock(MINIO_TAG, MINIO_PIN)).unwrap()).unwrap();
    let fake = FakeRunner::ok();
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), false);
    assert_eq!(runner.reference(&dep).unwrap(), MINIO_PIN);
}

#[test]
fn lock_cannot_replace_the_pinned_digest() {
    let other = format!("pgsty/silo@sha256:{}", "a".repeat(64));
    let lock = minio_lock(MINIO_TAG, &other);
    let fake = FakeRunner::ok();
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), false);
    assert_eq!(
        runner.reference(&docker_dep("minio", None)).unwrap(),
        MINIO_PIN
    );
}

#[test]
fn update_keeps_the_pinned_digest() {
    // `--update` re-resolves tags, but a built-in pin changes only with a devy release.
    let other = format!("pgsty/silo@sha256:{}", "a".repeat(64));
    let lock = minio_lock("RELEASE.2026-01-01T00-00-00Z", &other);
    let fake = FakeRunner::ok();
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), true);
    let dep = docker_dep("minio", None);
    assert_eq!(runner.reference(&dep).unwrap(), MINIO_PIN);
    let (version, digest) = runner.resolved(&dep).unwrap();
    assert_eq!(
        (version.as_deref(), digest.as_deref()),
        (Some(MINIO_TAG), Some(MINIO_PIN))
    );
}

#[test]
fn locked_digest_for_another_repository_is_not_reused() {
    let lock = minio_lock(MINIO_TAG, MINIO_PIN);
    let fake = FakeRunner::ok();
    let runner = docker_runner(&fake, "name: app\n", Some(&lock), false);
    let mut dep = docker_dep("minio", None);
    dep.image = Some(format!("minio/minio:{MINIO_TAG}"));
    assert_eq!(
        runner.reference(&dep).unwrap(),
        format!("minio/minio:{MINIO_TAG}")
    );
}

#[test]
fn config_hash_covers_secret_env() {
    let fake = FakeRunner::ok();
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let base = runner.run_spec(&docker_dep("redis", Some(51000))).unwrap();
    let mut with_secret = base.clone();
    with_secret.secret_env = vec![("MEILI_MASTER_KEY".into(), "a".into())];
    let mut other_secret = base.clone();
    other_secret.secret_env = vec![("MEILI_MASTER_KEY".into(), "b".into())];
    assert_ne!(config_hash(&base), config_hash(&with_secret));
    assert_ne!(config_hash(&with_secret), config_hash(&other_secret));
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
    let state = own_inspect(&docker_dep("redis", None));
    let fake = FakeRunner::new(move |_| ok(&state));
    assert!(
        docker_runner(&fake, "name: app\n", None, false)
            .is_running(&docker_dep("redis", None))
            .unwrap()
    );
    // Another project's container under our name doesn't count as ours running.
    let fake = FakeRunner::new(|_| ok(r#"{"State":{"Running":true},"Config":{"Labels":{}}}"#));
    assert!(
        !docker_runner(&fake, "name: app\n", None, false)
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

/// Inspect output for `dep`'s container as this project created it.
fn own_inspect(dep: &Dependency) -> String {
    let spec_fake = FakeRunner::ok();
    let labels = docker_runner(&spec_fake, "name: app\n", None, false)
        .run_spec(dep)
        .unwrap()
        .labels;
    inspect_json(true, &labels)
}

#[test]
fn stop_and_remove() {
    let dep = docker_dep("postgres", None);
    let state = own_inspect(&dep);
    let fake = FakeRunner::new(move |call| {
        if call[1] == "container" {
            ok(&state)
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    runner.stop(&dep).unwrap();
    assert!(runner.remove(&dep, false).unwrap());
    assert!(runner.remove(&dep, true).unwrap());
    let name = format!("devy-{}-postgresql", slug());
    let actions: Vec<String> = fake
        .lines()
        .into_iter()
        .filter(|l| !l.contains("container inspect"))
        .collect();
    assert_eq!(
        actions,
        vec![
            format!("docker stop {name}"),
            format!("docker rm -f {name}"),
            format!("docker rm -f {name}"),
            format!("docker volume rm {name}"),
        ]
    );
}

#[test]
fn stop_and_remove_proceed_when_container_is_missing() {
    let fake = FakeRunner::new(|call| {
        if call[1] == "container" {
            fail("No such container")
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let dep = docker_dep("postgres", None);
    runner.stop(&dep).unwrap();
    assert!(runner.remove(&dep, true).unwrap());
    let name = format!("devy-{}-postgresql", slug());
    assert!(fake.lines().contains(&format!("docker volume rm {name}")));
}

#[test]
fn stop_refuses_and_remove_skips_container_from_another_project() {
    let state = inspect_json(
        true,
        &[(PROJECT_LABEL.to_string(), "/src/evil".to_string())],
    );
    let fake = FakeRunner::new(move |_| ok(&state));
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let dep = docker_dep("redis", None);
    let expected = format!("container devy-{}-redis belongs to another project", slug());
    assert_eq!(runner.stop(&dep).unwrap_err().to_string(), expected);
    let msgs = crate::output::with_warn_messages(|| {
        assert!(
            !runner.remove(&dep, true).unwrap(),
            "nothing of ours removed"
        );
    });
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("belongs to another project"), "{msgs:?}");
    assert!(
        fake.lines().iter().all(|l| l.contains("container inspect")),
        "no stop, rm or volume rm: {:?}",
        fake.lines()
    );
}

/// The labels of the container `run_spec` creates for redis, with `sh.devy.host` set to
/// `host` (or removed).
fn redis_labels_on(host: Option<&str>) -> Vec<(String, String)> {
    let fake = FakeRunner::ok();
    let mut labels: Vec<(String, String)> = docker_runner(&fake, "name: app\n", None, false)
        .run_spec(&docker_dep("redis", Some(51000)))
        .unwrap()
        .labels
        .into_iter()
        .filter(|(k, _)| k != HOST_LABEL)
        .collect();
    if let Some(h) = host {
        labels.push((HOST_LABEL.to_string(), h.to_string()));
    }
    labels
}

/// Runs start, stop and remove against a running redis container labeled with `labels`,
/// as the machine with host id `host`; returns the errors of start and stop, the
/// warnings of remove, and the CLI calls.
fn act_on(
    labels: Vec<(String, String)>,
    host: Option<&'static str>,
) -> (Result<()>, Result<()>, bool, Vec<String>, Vec<String>) {
    let state = inspect_json(true, &labels);
    let fake = FakeRunner::new(move |call| {
        if call[1] == "container" {
            ok(&state)
        } else {
            ok("")
        }
    });
    let mut runner = docker_runner(&fake, "name: app\n", None, false);
    runner.set_host(host);
    let dep = docker_dep("redis", Some(51000));
    let start = runner.start(&dep);
    let stop = runner.stop(&dep);
    let running = runner.is_running(&dep).unwrap();
    let mut removed = false;
    let warnings = crate::output::with_warn_messages(|| {
        removed = runner.remove(&dep, false).unwrap();
    });
    let acting = fake
        .lines()
        .into_iter()
        .filter(|l| !l.contains("container inspect"))
        .collect();
    assert_eq!(removed, start.is_ok(), "remove agrees with start");
    (start, stop, running, warnings, acting)
}

#[test]
fn start_stop_and_remove_refuse_another_hosts_container() {
    let name = format!("devy-{}-redis", slug());
    let (start, stop, running, warnings, acting) =
        act_on(redis_labels_on(Some("ffffffffffffffff")), Some(TEST_HOST));
    for err in [start.unwrap_err(), stop.unwrap_err()] {
        let err = err.to_string();
        assert!(
            err.starts_with(&format!(
                "container {name} belongs to another machine or user sharing this container daemon"
            )),
            "{err}"
        );
        // How to recover when it's really this user's, from before the id changed.
        assert!(
            err.contains(&format!("remove it with `docker rm -f {name}`")),
            "{err}"
        );
    }
    // Not this service running, so `down` skips stopping it.
    assert!(!running);
    assert!(
        warnings[0].contains("belongs to another machine or user"),
        "{warnings:?}"
    );
    assert!(acting.is_empty(), "only inspects: {acting:?}");
}

#[test]
fn a_machine_without_an_id_refuses_labeled_containers() {
    let (start, stop, running, warnings, acting) = act_on(redis_labels_on(Some(TEST_HOST)), None);
    for err in [start.unwrap_err(), stop.unwrap_err()] {
        let err = err.to_string();
        assert!(
            err.contains(
                "devy can't identify this machine (under WSL, run devy from a wsl.exe session)"
            ),
            "{err}"
        );
        assert!(err.contains("docker rm -f"), "{err}");
    }
    assert!(!running);
    assert!(
        warnings[0].contains("devy can't identify this machine"),
        "{warnings:?}"
    );
    assert!(acting.is_empty(), "only inspects: {acting:?}");
}

#[test]
fn same_host_and_unlabelled_containers_are_this_projects() {
    let name = format!("devy-{}-redis", slug());
    for (labels, host) in [
        (redis_labels_on(Some(TEST_HOST)), Some(TEST_HOST)),
        (redis_labels_on(None), Some(TEST_HOST)),
        // A container without the label is the project's even without an id.
        (redis_labels_on(None), None),
    ] {
        let (start, stop, running, warnings, acting) = act_on(labels, host);
        start.unwrap();
        stop.unwrap();
        assert!(running);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            acting,
            [
                format!("docker stop {name}"),
                format!("docker rm -f {name}")
            ]
        );
    }
}

#[test]
fn foreign_owner_rules() {
    let state = docker::ContainerState {
        id: String::new(),
        running: true,
        labels: [
            (PROJECT_LABEL.to_string(), ROOT.to_string()),
            (HOST_LABEL.to_string(), TEST_HOST.to_string()),
        ]
        .into(),
        volumes: Vec::new(),
    };
    let root = Path::new(ROOT);
    assert_eq!(foreign_owner_for(root, Some(TEST_HOST), &state), None);
    assert_eq!(
        foreign_owner_for(root, Some("ffffffffffffffff"), &state),
        Some(Foreign::Host)
    );
    assert_eq!(
        foreign_owner_for(root, None, &state),
        Some(Foreign::Unidentified)
    );
    // Without the label, ownership is by project alone, with or without an id.
    let mut unlabelled = state.clone();
    unlabelled.labels.remove(HOST_LABEL);
    assert_eq!(foreign_owner_for(root, None, &unlabelled), None);
    assert_eq!(foreign_owner_for(root, Some(TEST_HOST), &unlabelled), None);
    // The project label decides first.
    assert_eq!(
        foreign_owner_for(Path::new("/src/other"), Some(TEST_HOST), &state),
        Some(Foreign::Project)
    );
    assert_eq!(Foreign::Project.describe(), "another project");
}

#[test]
fn docker_warnings_only_for_docker_managed() {
    let mut kafka = Dependency::simple("kafka");
    assert!(docker_warnings(&kafka).is_empty());
    kafka.docker = true;
    assert_eq!(docker_warnings(&kafka).len(), 1);
}

#[test]
fn docker_logs_tail_the_container() {
    for (yaml, cli) in [
        ("name: app\n", "docker"),
        ("name: app\ncontainer_cli: podman\n", "podman"),
    ] {
        let mut state: serde_json::Value =
            serde_json::from_str(&inspect_json(true, &redis_labels_on(Some(TEST_HOST)))).unwrap();
        state["Id"] = "4f1c9e0a".into();
        let state = state.to_string();
        let fake = FakeRunner::new(move |_| ok(&state));
        let mut runner = docker_runner(&fake, yaml, None, false);
        runner.set_host(Some(TEST_HOST));
        let dep = docker_dep("redis", None);
        for (follow, expected) in [
            (false, vec!["logs", "--tail", "50", "4f1c9e0a"]),
            (true, vec!["logs", "--tail", "50", "-f", "4f1c9e0a"]),
        ] {
            let LogSource::Command(cmd) = runner.log_source(&dep, 50, follow).unwrap() else {
                panic!("expected a command");
            };
            assert_eq!(cmd.program, cli);
            assert_eq!(cmd.args, expected);
            assert_eq!(cmd.kind, LogCommandKind::Container);
        }
        assert!(
            fake.lines().iter().all(|l| l.contains("container inspect")),
            "building the command only checks the owner: {:?}",
            fake.lines()
        );
    }
}

/// With no container there is nothing to read, and the CLI is never run by name (a
/// container created under it after the check could be someone else's).
#[test]
fn docker_logs_without_a_container_are_empty() {
    let fake = FakeRunner::new(|call| {
        if call[1] == "container" {
            fail("No such container")
        } else {
            ok("")
        }
    });
    let runner = docker_runner(&fake, "name: app\n", None, false);
    let dep = docker_dep("redis", None);
    for follow in [false, true] {
        assert_eq!(
            runner.log_source(&dep, 50, follow).unwrap(),
            LogSource::Files(vec![])
        );
    }
    assert_eq!(
        runner
            .log_source_within(&dep, 50, std::time::Duration::from_secs(5))
            .unwrap(),
        LogSource::Files(vec![])
    );
    assert!(
        fake.lines().iter().all(|l| l.contains("container inspect")),
        "{:?}",
        fake.lines()
    );
}

#[test]
fn docker_logs_read_the_checked_container_by_id() {
    let name = format!("devy-{}-redis", slug());
    // A hex ID is used; anything else (a CLI that reports none, or one that could read
    // as a flag) is an error, never a fall back to the name.
    for (id, target) in [("4f1c9e0a", Some("4f1c9e0a")), ("", None), ("--all", None)] {
        let mut state: serde_json::Value =
            serde_json::from_str(&inspect_json(true, &redis_labels_on(Some(TEST_HOST)))).unwrap();
        state["Id"] = id.into();
        let state = state.to_string();
        let fake = FakeRunner::new(move |_| ok(&state));
        let mut runner = docker_runner(&fake, "name: app\n", None, false);
        runner.set_host(Some(TEST_HOST));
        let dep = docker_dep("redis", None);
        let followed = runner.log_source(&dep, 50, true);
        let within = runner.log_source_within(&dep, 50, std::time::Duration::from_secs(5));
        let Some(target) = target else {
            for result in [followed, within] {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    format!("`docker` did not report an ID for container {name}")
                );
            }
            continue;
        };
        let Ok(LogSource::Command(cmd)) = followed else {
            panic!("expected a command");
        };
        assert_eq!(cmd.args, ["logs", "--tail", "50", "-f", target]);
        let Ok(LogSource::Command(cmd)) = within else {
            panic!("expected a command");
        };
        assert_eq!(cmd.args, ["logs", "--tail", "50", target]);
    }
}

#[test]
fn docker_logs_refuse_another_owners_container() {
    for (labels, expected) in [
        (
            vec![(PROJECT_LABEL.to_string(), "/src/evil".to_string())],
            "belongs to another project",
        ),
        (
            redis_labels_on(Some("ffffffffffffffff")),
            "belongs to another machine or user",
        ),
    ] {
        let state = inspect_json(true, &labels);
        let fake = FakeRunner::new(move |_| ok(&state));
        let mut runner = docker_runner(&fake, "name: app\n", None, false);
        runner.set_host(Some(TEST_HOST));
        let err = runner
            .log_source(&docker_dep("redis", None), 50, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains(expected), "{err}");
    }
}

#[test]
fn package_logs_come_from_the_package_manager() {
    let pm = MockPackageManager {
        log_source_result: Some(LogSource::Files(vec!["/tmp/r.log".into()])),
        ..Default::default()
    };
    let runner = PackageRunner::new(&pm, Path::new(ROOT));
    assert_eq!(
        runner
            .log_source(&Dependency::simple("redis"), 10, false)
            .unwrap(),
        LogSource::Files(vec!["/tmp/r.log".into()])
    );
}
