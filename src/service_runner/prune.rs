//! Docker-managed services of removed checkouts, for `devy prune`: containers labeled
//! `sh.devy.project` whose recorded root no longer has a `devy.yml` and whose
//! `sh.devy.host` is this machine's, each with the data volume of the same name.

use anyhow::{Result, bail};

use super::docker::{ContainerRuntime, ContainerState};
use super::{HOST_LABEL, PROJECT_LABEL};
use crate::config::{ContainerCli, is_removed_checkout};

/// A devy container whose project root is a removed checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrphanedContainer {
    /// The container's name, which is also its volume's.
    pub name: String,
    /// The recorded `sh.devy.project` label.
    pub root: String,
    /// The container's ID, which removal checks and removes it by, so a container
    /// created under the same name after it was listed is never removed.
    pub id: String,
    /// Whether it mounts the volume named like it (services without data have none).
    pub has_volume: bool,
}

/// The container CLIs `devy prune` tries, in order: `docker`, then `podman` (when
/// `docker` is missing or can't list containers). There may be no `devy.yml` to read
/// `container_cli` from. `installed` says whether a CLI exists.
pub(crate) fn prune_clis(installed: impl Fn(&str) -> bool) -> Vec<ContainerCli> {
    [ContainerCli::Docker, ContainerCli::Podman]
        .into_iter()
        .filter(|cli| installed(cli.binary()))
        .collect()
}

/// Whether daemon endpoint `endpoint` is a local socket (`unix://`, `npipe://`).
fn is_local_endpoint(endpoint: &str) -> bool {
    let endpoint = endpoint.trim();
    endpoint.starts_with("unix://") || endpoint.starts_with("npipe://")
}

/// The environment variable that points `cli` at a daemon or connection that may be on
/// another machine: `DOCKER_HOST` with a non-local endpoint for docker; `CONTAINER_HOST`
/// with a non-local endpoint, or any `CONTAINER_CONNECTION`, for podman. `var` reads an
/// environment variable.
fn remote_daemon_var(
    cli: ContainerCli,
    var: impl Fn(&str) -> Option<String>,
) -> Option<&'static str> {
    let set = |k: &str| var(k).filter(|v| !v.trim().is_empty());
    match cli {
        ContainerCli::Docker => set("DOCKER_HOST")
            .is_some_and(|v| !is_local_endpoint(&v))
            .then_some("DOCKER_HOST"),
        ContainerCli::Podman => {
            if set("CONTAINER_HOST").is_some_and(|v| !is_local_endpoint(&v)) {
                Some("CONTAINER_HOST")
            } else if set("CONTAINER_CONNECTION").is_some() {
                Some("CONTAINER_CONNECTION")
            } else {
                None
            }
        }
    }
}

/// Fails unless `runtime` talks to a daemon on this machine. A remote daemon's containers
/// record roots on other filesystems, where devy can't tell a removed checkout from a live
/// one. Docker's endpoint comes from its current context (`DOCKER_CONTEXT`, `docker
/// context use`). Podman on Linux must report a local service; elsewhere it is judged by
/// its environment only, since a macOS or Windows podman machine is always "remote".
pub(crate) fn ensure_local_daemon(
    runtime: &ContainerRuntime,
    var: impl Fn(&str) -> Option<String>,
) -> Result<()> {
    ensure_local_daemon_on(runtime, var, cfg!(target_os = "linux"))
}

/// `ensure_local_daemon`, with whether this is Linux injected.
fn ensure_local_daemon_on(
    runtime: &ContainerRuntime,
    var: impl Fn(&str) -> Option<String>,
    linux: bool,
) -> Result<()> {
    let cli = runtime.cli();
    if let Some(name) = remote_daemon_var(cli, var) {
        bail!(
            "{name} points {} at a daemon that may be remote",
            cli.binary()
        );
    }
    if cli == ContainerCli::Docker {
        let endpoint = runtime.context_endpoint()?;
        if !is_local_endpoint(&endpoint) {
            bail!(
                "docker's current context uses {}, which may be remote",
                crate::output::clean_line(&endpoint)
            );
        }
    }
    if cli == ContainerCli::Podman && linux {
        let remote = runtime.podman_service_is_remote()?;
        if remote != "false" {
            bail!(
                "podman reports its service as remote ({})",
                crate::output::clean_line(&remote)
            );
        }
    }
    Ok(())
}

/// Whether `name`, read from the container CLI's output, is named like a devy container
/// (`devy-<project>-<service>`) and uses only the characters container names allow, so
/// passing it back as an argument can never read as a flag.
fn is_devy_container_name(name: &str) -> bool {
    name.strip_prefix("devy-")
        .is_some_and(|rest| !rest.is_empty())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

/// Whether `id`, read from the container CLI's output, looks like a container ID (hex),
/// so passing it back as an argument can never read as a flag.
pub(crate) fn is_container_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// What `devy prune` makes of a container.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// This machine created it, for a checkout that has since been removed.
    Orphaned(OrphanedContainer),
    /// An older devy created it, without a `sh.devy.host` label, for a root that is a
    /// removed checkout here. It may be another machine's on a shared daemon.
    Unlabelled(String),
    /// Another machine or user created it (or this machine has no id). Its root names a
    /// path on that machine's filesystem, so it isn't looked at.
    OtherHost,
    /// Gone, without a project label, or a live checkout's.
    Keep,
}

/// Classifies container `name` for this machine's host id `host`.
fn classify(runtime: &ContainerRuntime, name: &str, host: Option<&str>) -> Result<Verdict> {
    let Some(state) = runtime.inspect_container(name)? else {
        return Ok(Verdict::Keep);
    };
    let Some(root) = state.labels.get(PROJECT_LABEL) else {
        return Ok(Verdict::Keep);
    };
    let labeled = match state.labels.get(HOST_LABEL) {
        None => false,
        Some(h) if Some(h.as_str()) == host => true,
        Some(_) => return Ok(Verdict::OtherHost),
    };
    if !is_removed_checkout(root) {
        return Ok(Verdict::Keep);
    }
    if !labeled {
        return Ok(Verdict::Unlabelled(root.clone()));
    }
    if !is_container_id(&state.id) {
        bail!("{} reported no valid ID for it", runtime.cli_name());
    }
    Ok(Verdict::Orphaned(orphaned(name, root, &state)))
}

fn orphaned(name: &str, root: &str, state: &ContainerState) -> OrphanedContainer {
    OrphanedContainer {
        name: name.to_string(),
        root: root.to_string(),
        id: state.id.clone(),
        has_volume: state.volumes.iter().any(|v| v == name),
    }
}

/// The devy containers of removed checkouts that `devy prune` found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ContainerScan {
    /// Created on this machine (`sh.devy.host` is this host's id): these are removed.
    pub orphaned: Vec<OrphanedContainer>,
    /// `(name, root)` of each created by an older devy, without `sh.devy.host`: left
    /// alone, since they may be another machine's on a daemon several machines share.
    pub unlabelled: Vec<(String, String)>,
    /// How many devy containers another machine or user created: left alone, unlisted.
    pub other_host: usize,
}

/// The devy containers whose `sh.devy.project` root is a removed checkout, split by
/// whether this machine (host id `host`) created them, and a count of other machines'
/// containers. Containers named unlike devy's are skipped, and so, with a warning, is one
/// that can't be inspected.
pub(crate) fn find_orphaned_containers(
    runtime: &ContainerRuntime,
    host: Option<&str>,
) -> Result<ContainerScan> {
    let mut scan = ContainerScan::default();
    for name in runtime.containers_with_label(PROJECT_LABEL)? {
        if !is_devy_container_name(&name) {
            continue;
        }
        match classify(runtime, &name, host) {
            Ok(Verdict::Orphaned(container)) => scan.orphaned.push(container),
            Ok(Verdict::Unlabelled(root)) => scan.unlabelled.push((name, root)),
            Ok(Verdict::OtherHost) => scan.other_host += 1,
            Ok(Verdict::Keep) => {}
            Err(e) => crate::output::warn(&format!("skipping container {name}: {e:#}")),
        }
    }
    scan.orphaned.sort_by(|a, b| a.name.cmp(&b.name));
    scan.unlabelled.sort();
    Ok(scan)
}

/// Force-removes `container` by its ID and, with `volumes`, the same-named volume it
/// mounts. Fails, removing nothing, when the container is gone, was replaced under its
/// name, no longer records the same removed checkout, or is no longer labeled with host
/// id `host`: it changed since it was listed.
pub(crate) fn remove_orphaned_container(
    runtime: &ContainerRuntime,
    container: &OrphanedContainer,
    host: Option<&str>,
    volumes: bool,
) -> Result<()> {
    let current = match classify(runtime, &container.name, host)? {
        Verdict::Orphaned(c) if c.id == container.id && c.root == container.root => c,
        _ => bail!(
            "{} changed since it was listed; left it in place",
            container.name
        ),
    };
    runtime.remove_container(&current.id)?;
    if volumes && current.has_volume {
        runtime.remove_volume(&current.name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::docker::{FakeRunner, fail, ok};
    use super::*;
    use std::path::Path;

    /// The host id the tests prune as.
    const HOST: &str = "0123456789abcdef";

    /// The (fake, hex) ID of container `name`.
    fn id_of(name: &str) -> String {
        format!("{:016x}", crate::modules::fnv1a(name))
    }

    /// `container inspect` output for container `name` of this host recording `root`.
    fn inspect(name: &str, root: Option<&Path>) -> String {
        inspect_on(name, root, Some(HOST))
    }

    /// `container inspect` output for container `name` recording `root`, labeled with
    /// `host` and mounting its same-named volume.
    fn inspect_on(name: &str, root: Option<&Path>, host: Option<&str>) -> String {
        let mut labels = serde_json::Map::new();
        if let Some(r) = root {
            labels.insert(PROJECT_LABEL.into(), r.to_string_lossy().into());
        }
        if let Some(h) = host {
            labels.insert(HOST_LABEL.into(), h.into());
        }
        serde_json::json!({
            "Id": id_of(name),
            "State": {"Running": true},
            "Config": {"Labels": labels},
            "Mounts": [{"Type": "volume", "Name": name, "Destination": "/data"}],
        })
        .to_string()
    }

    /// The `OrphanedContainer` the fakes describe for `name` and removed root `root`.
    fn orphan(name: &str, root: &Path) -> OrphanedContainer {
        OrphanedContainer {
            name: name.into(),
            root: root.to_string_lossy().into_owned(),
            id: id_of(name),
            has_volume: true,
        }
    }

    /// A live checkout (with a devy.yml) and a removed one, in a temp dir.
    fn checkouts() -> (
        crate::test_support::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let dir = crate::test_support::tmp_dir();
        let live = dir.join("app");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("devy.yml"), "name: app\n").unwrap();
        let gone = dir.join("app-feat");
        (dir, live, gone)
    }

    /// A fake CLI listing `ps` names, answering `container inspect <name>` from `roots`
    /// (a name without an entry doesn't exist), and succeeding at everything else.
    fn fake(ps: &str, roots: Vec<(&str, Option<std::path::PathBuf>)>) -> FakeRunner {
        fake_on(
            ps,
            roots.into_iter().map(|(n, r)| (n, r, Some(HOST))).collect(),
        )
    }

    /// `fake`, with each container's `sh.devy.host` label (or none) given.
    fn fake_on(
        ps: &str,
        roots: Vec<(&str, Option<std::path::PathBuf>, Option<&'static str>)>,
    ) -> FakeRunner {
        let ps = ps.to_string();
        let roots: Vec<(String, Option<std::path::PathBuf>, Option<&str>)> = roots
            .into_iter()
            .map(|(n, r, h)| (n.to_string(), r, h))
            .collect();
        FakeRunner::new(move |call| match call.get(1).map(String::as_str) {
            Some("ps") => ok(&ps),
            Some("container") => {
                let name = call.last().unwrap();
                match roots.iter().find(|(n, _, _)| n == name) {
                    Some((_, root, host)) => ok(&inspect_on(name, root.as_deref(), *host)),
                    None => fail(&format!("Error: No such container: {name}")),
                }
            }
            _ => ok(""),
        })
    }

    #[test]
    fn prune_cli_prefers_docker_then_podman() {
        assert_eq!(
            prune_clis(|_| true),
            [ContainerCli::Docker, ContainerCli::Podman]
        );
        assert_eq!(prune_clis(|c| c == "podman"), [ContainerCli::Podman]);
        assert_eq!(prune_clis(|_| false), []);
    }

    #[test]
    fn finds_containers_of_removed_checkouts_only() {
        let (_dir, live, gone) = checkouts();
        let runner = fake(
            "devy-app-1-redis\ndevy-app-2-redis\ndevy-legacy\n-rm\ndevy-x;y\nother\n\n",
            vec![
                ("devy-app-1-redis", Some(gone.clone())),
                ("devy-app-2-redis", Some(live)),
                ("devy-legacy", None),
                ("-rm", Some(gone.clone())),
                ("devy-x;y", Some(gone.clone())),
                ("other", Some(gone.clone())),
            ],
        );
        let runtime = ContainerRuntime::new(ContainerCli::Podman, &runner);
        let found = find_orphaned_containers(&runtime, Some(HOST)).unwrap();
        assert_eq!(found.orphaned, [orphan("devy-app-1-redis", &gone)]);
        assert_eq!(found.unlabelled, []);
        let calls = runner.lines();
        assert_eq!(
            calls[0],
            "podman ps -a --filter label=sh.devy.project --format {{.Names}}"
        );
        // Names devy never uses are not passed back to the CLI.
        for name in ["-rm", "devy-x;y", "other"] {
            assert!(
                !calls.iter().any(|c| c.ends_with(&format!(" {name}"))),
                "{calls:?}"
            );
        }
    }

    #[test]
    fn only_this_hosts_containers_are_orphaned() {
        let (_dir, live, gone) = checkouts();
        let runner = fake_on(
            "devy-a-redis\ndevy-b-redis\ndevy-c-redis\ndevy-d-redis\ndevy-e-redis\n",
            vec![
                ("devy-a-redis", Some(gone.clone()), Some(HOST)),
                // Another machine on a shared daemon: never pruned, whatever its root.
                ("devy-b-redis", Some(gone.clone()), Some("fedcba9876543210")),
                ("devy-c-redis", Some(gone.clone()), Some("")),
                // Created by an older devy: reported, not pruned.
                ("devy-d-redis", Some(gone.clone()), None),
                // An older devy's container of a live checkout: nothing to report.
                ("devy-e-redis", Some(live), None),
            ],
        );
        let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
        let found = find_orphaned_containers(&runtime, Some(HOST)).unwrap();
        assert_eq!(
            found,
            ContainerScan {
                orphaned: vec![orphan("devy-a-redis", &gone)],
                unlabelled: vec![("devy-d-redis".into(), gone.to_string_lossy().into_owned())],
                other_host: 2,
            }
        );
        // Removal re-checks the host: another machine's or an unlabeled container is
        // never removed, even when asked to.
        for name in ["devy-b-redis", "devy-d-redis"] {
            let err = remove_orphaned_container(&runtime, &orphan(name, &gone), Some(HOST), true)
                .unwrap_err()
                .to_string();
            assert!(err.contains("changed since it was listed"), "{err}");
        }
        // A machine without an id removes nothing: no label can be its own.
        let found = find_orphaned_containers(&runtime, None).unwrap();
        assert_eq!(found.orphaned, []);
        assert_eq!(found.other_host, 3);
        assert_eq!(found.unlabelled.len(), 1);
        let err = remove_orphaned_container(&runtime, &orphan("devy-a-redis", &gone), None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("changed since it was listed"), "{err}");
        assert!(
            !runner.lines().iter().any(|l| l.contains(" rm ")),
            "{:?}",
            runner.lines()
        );
    }

    #[test]
    fn a_container_without_a_valid_id_is_skipped() {
        let (_dir, _live, gone) = checkouts();
        let root = gone.to_string_lossy().into_owned();
        let runner = FakeRunner::new(move |call| match call[1].as_str() {
            "ps" => ok("devy-a-redis\n"),
            _ => ok(&serde_json::json!({
                "Id": "--force",
                "Config": {"Labels": {PROJECT_LABEL: root, HOST_LABEL: HOST}},
            })
            .to_string()),
        });
        let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
        let warnings = crate::output::with_warn_messages(|| {
            assert_eq!(
                find_orphaned_containers(&runtime, Some(HOST))
                    .unwrap()
                    .orphaned,
                []
            );
        });
        assert!(warnings[0].contains("reported no valid ID"), "{warnings:?}");
    }

    #[test]
    fn remote_daemons_are_detected() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let (d, p) = (ContainerCli::Docker, ContainerCli::Podman);
        assert_eq!(remote_daemon_var(d, env(&[])), None);
        assert_eq!(remote_daemon_var(p, env(&[])), None);
        let colima = &[("DOCKER_HOST", "unix:///Users/u/.colima/docker.sock")];
        assert_eq!(remote_daemon_var(d, env(colima)), None);
        assert_eq!(remote_daemon_var(d, env(&[("DOCKER_HOST", "")])), None);
        let tcp = &[("DOCKER_HOST", "tcp://10.0.0.5:2376")];
        assert_eq!(remote_daemon_var(d, env(tcp)), Some("DOCKER_HOST"));
        // Each CLI is judged by its own variables only.
        assert_eq!(remote_daemon_var(p, env(tcp)), None);
        let ssh = &[("CONTAINER_HOST", "ssh://u@host/run/podman.sock")];
        assert_eq!(remote_daemon_var(p, env(ssh)), Some("CONTAINER_HOST"));
        assert_eq!(remote_daemon_var(d, env(ssh)), None);
        let conn = &[("CONTAINER_CONNECTION", "build-box")];
        assert_eq!(
            remote_daemon_var(p, env(conn)),
            Some("CONTAINER_CONNECTION")
        );
    }

    #[test]
    fn docker_must_use_a_local_context() {
        for (endpoint, local) in [
            ("unix:///var/run/docker.sock\n", true),
            ("npipe:////./pipe/docker_engine", true),
            ("ssh://u@build-box", false),
            ("tcp://10.0.0.5:2376", false),
            ("", false),
        ] {
            let runner = FakeRunner::new(move |_| ok(endpoint));
            let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
            let result = ensure_local_daemon(&runtime, |_| None);
            assert_eq!(result.is_ok(), local, "{endpoint:?}: {result:?}");
            assert_eq!(
                runner.lines(),
                ["docker context inspect --format {{.Endpoints.docker.Host}}"]
            );
        }
        let runner = FakeRunner::new(|_| fail("unknown command"));
        let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
        assert!(ensure_local_daemon(&runtime, |_| None).is_err());
        // Off Linux, podman has nothing to ask (its machine is always "remote"); an env
        // var pointing elsewhere is enough to refuse.
        let runner = FakeRunner::new(|_| panic!("no podman call expected"));
        let runtime = ContainerRuntime::new(ContainerCli::Podman, &runner);
        ensure_local_daemon_on(&runtime, |_| None, false).unwrap();
        let err = ensure_local_daemon_on(
            &runtime,
            |k| (k == "CONTAINER_CONNECTION").then(|| "box".to_string()),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("CONTAINER_CONNECTION points podman"), "{err}");
    }

    #[test]
    fn podman_on_linux_must_report_a_local_service() {
        for (answer, local) in [
            (ok("false\n"), true),
            (ok("true\n"), false),
            (ok(""), false),
            (fail("boom"), false),
        ] {
            let runner = FakeRunner::new(move |_| answer.clone());
            let runtime = ContainerRuntime::new(ContainerCli::Podman, &runner);
            let result = ensure_local_daemon_on(&runtime, |_| None, true);
            assert_eq!(result.is_ok(), local, "{result:?}");
            assert_eq!(
                runner.lines(),
                ["podman info --format {{.Host.ServiceIsRemote}}"]
            );
        }
    }

    #[test]
    fn a_container_that_cannot_be_inspected_is_skipped() {
        let (_dir, _live, gone) = checkouts();
        let runner = FakeRunner::new(move |call| match call.get(1).map(String::as_str) {
            Some("ps") => ok("devy-a-redis\ndevy-b-redis\n"),
            Some("container") if call.last().unwrap() == "devy-a-redis" => {
                fail("permission denied")
            }
            _ => ok(&inspect(call.last().unwrap(), Some(&gone))),
        });
        let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
        let warnings = crate::output::with_warn_messages(|| {
            let found = find_orphaned_containers(&runtime, Some(HOST))
                .unwrap()
                .orphaned;
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].name, "devy-b-redis");
        });
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("skipping container devy-a-redis"));
    }

    #[test]
    fn a_failing_ps_is_an_error() {
        let runner = FakeRunner::new(|_| fail("Cannot connect to the Docker daemon"));
        let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
        let err = find_orphaned_containers(&runtime, Some(HOST))
            .unwrap_err()
            .to_string();
        assert!(err.contains("docker ps failed: Cannot connect"), "{err}");
    }

    #[test]
    fn removes_the_container_by_id_and_with_volumes_its_volume() {
        let (_dir, _live, gone) = checkouts();
        let name = "devy-app-1-redis";
        let container = orphan(name, &gone);
        let id = id_of(name);
        let runner = fake("", vec![(name, Some(gone.clone()))]);
        let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
        remove_orphaned_container(&runtime, &container, Some(HOST), false).unwrap();
        assert_eq!(runner.lines()[1..], [format!("docker rm -f {id}")]);
        let runner = fake("", vec![(name, Some(gone.clone()))]);
        let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
        remove_orphaned_container(&runtime, &container, Some(HOST), true).unwrap();
        assert_eq!(
            runner.lines()[1..],
            [
                format!("docker rm -f {id}"),
                format!("docker volume rm {name}")
            ]
        );
    }

    #[test]
    fn volumes_never_removes_a_volume_the_container_does_not_mount() {
        let (_dir, _live, gone) = checkouts();
        let root = gone.to_string_lossy().into_owned();
        // A service without data (mailhog): no mounts.
        let runner = FakeRunner::new(move |call| match call[1].as_str() {
            "ps" => ok("devy-app-1-mailhog\n"),
            "container" => ok(&serde_json::json!({
                "Id": "abc123",
                "Config": {"Labels": {PROJECT_LABEL: root, HOST_LABEL: HOST}},
                "Mounts": [],
            })
            .to_string()),
            _ => ok(""),
        });
        let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
        let found = find_orphaned_containers(&runtime, Some(HOST))
            .unwrap()
            .orphaned;
        assert!(!found[0].has_volume, "{found:?}");
        remove_orphaned_container(&runtime, &found[0], Some(HOST), true).unwrap();
        assert_eq!(runner.lines().last().unwrap(), "docker rm -f abc123");
    }

    #[test]
    fn leaves_a_container_that_changed_since_it_was_listed() {
        let (_dir, live, gone) = checkouts();
        let name = "devy-app-1-redis";
        let container = orphan(name, &gone);
        // Relabeled to a live root, gone altogether, or replaced by another container
        // under the same name.
        let replaced = OrphanedContainer {
            id: "ffff".into(),
            ..container.clone()
        };
        for (listed, roots) in [
            (&container, vec![(name, Some(live.clone()))]),
            (&container, vec![]),
            (&replaced, vec![(name, Some(gone.clone()))]),
        ] {
            let runner = fake("", roots);
            let runtime = ContainerRuntime::new(ContainerCli::Docker, &runner);
            let err = remove_orphaned_container(&runtime, listed, Some(HOST), true)
                .unwrap_err()
                .to_string();
            assert!(err.contains("changed since it was listed"), "{err}");
            assert_eq!(runner.lines().len(), 1, "{:?}", runner.lines());
        }
    }
}
