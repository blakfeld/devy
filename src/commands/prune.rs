//! `devy prune`: removes the service resources of removed checkouts — nix units and
//! docker-managed containers (and, with `--volumes`, their volumes) whose recorded
//! project root no longer has a `devy.yml`. Needs no `devy.yml` of its own: every
//! resource records its root.

use anyhow::{Result, anyhow, bail};
use std::io::{BufRead, IsTerminal, Write};

use crate::output;
use crate::service_runner::docker::ContainerRuntime;
use crate::service_runner::prune::{
    OrphanedContainer, ensure_local_daemon, find_orphaned_containers, prune_clis,
    remove_orphaned_container,
};

/// One resource `devy prune` found, as listed to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Resource {
    kind: String,
    name: String,
    /// The recorded project root.
    root: String,
}

/// A kind of resource `devy prune` finds and removes.
trait Source {
    /// What the source finds, for the note when it can't look: `containers`.
    fn what(&self) -> &'static str;
    /// Finds the resources of removed checkouts. An error skips this source with a note.
    fn find(&mut self) -> Result<Vec<Resource>>;
    /// Removes the `index`th resource `find` returned.
    fn remove(&self, index: usize) -> Result<()>;
    /// Called once after this source's last removal.
    fn finish(&self) -> Result<()>;
    /// A note shown below the list of what will be removed.
    fn note(&self) -> Option<String> {
        None
    }
}

/// nix-run services' launchd agents or systemd user units.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Units {
    pruner: crate::package_manager::UnitPruner,
    found: Vec<crate::package_manager::OrphanedUnit>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Source for Units {
    fn what(&self) -> &'static str {
        "service units"
    }

    fn find(&mut self) -> Result<Vec<Resource>> {
        self.found = self.pruner.find()?;
        let kind = self.pruner.kind_name();
        Ok(self
            .found
            .iter()
            .map(|u| Resource {
                kind: kind.into(),
                name: u.id.clone(),
                root: u.root.clone(),
            })
            .collect())
    }

    fn remove(&self, index: usize) -> Result<()> {
        self.pruner.remove(&self.found[index])
    }

    fn finish(&self) -> Result<()> {
        self.pruner.finish()
    }
}

/// Docker-managed services' containers and, with `volumes`, their volumes, through
/// `docker` or `podman`.
struct Containers<'a> {
    /// The installed CLIs, in the order they are tried.
    runtimes: Vec<ContainerRuntime<'a>>,
    /// This machine's `sh.devy.host` id: only containers labeled with it are removed.
    /// `None` when it can't be identified: then none are.
    host: Option<&'a str>,
    /// Whether each removed container's same-named volume is removed too.
    volumes: bool,
    /// Reads an environment variable (see `ensure_local_daemon`).
    env: fn(&str) -> Option<String>,
    /// The runtime that listed `found`.
    chosen: Option<usize>,
    found: Vec<OrphanedContainer>,
}

impl Source for Containers<'_> {
    fn what(&self) -> &'static str {
        "containers"
    }

    fn find(&mut self) -> Result<Vec<Resource>> {
        // The first CLI that can list a local daemon's containers: `docker` may be
        // installed without a running daemon while the user's projects run on podman.
        let mut errors = Vec::new();
        let mut unlabelled = Vec::new();
        let mut other_host = 0;
        for (index, runtime) in self.runtimes.iter().enumerate() {
            let listed = ensure_local_daemon(runtime, self.env)
                .and_then(|()| find_orphaned_containers(runtime, self.host));
            match listed {
                Ok(scan) => {
                    self.found = scan.orphaned;
                    unlabelled = scan.unlabelled;
                    other_host = scan.other_host;
                    self.chosen = Some(index);
                    break;
                }
                Err(e) => errors.push(format!("{}: {e:#}", runtime.cli_name())),
            }
        }
        if self.chosen.is_none() {
            if errors.is_empty() {
                bail!("neither docker nor podman was found");
            }
            bail!("{}", errors.join("; "));
        }
        for error in errors {
            output::skip(&format!("skipping containers of {error}"));
        }
        let cli = self.runtime()?.cli_name();
        if self.host.is_none() {
            output::skip(
                "devy can't identify this machine, so it removes no containers \
                 (see `sh.devy.host` in the README)",
            );
        }
        // Without an id, every labeled container is counted here, this machine's too, and
        // the note above already says why nothing is removed.
        if other_host > 0 && self.host.is_some() {
            let count = plural(other_host, "container", "containers");
            output::skip(&format!(
                "ignoring {count} labeled by another machine or user, whether or not their checkouts exist"
            ));
        }
        if !unlabelled.is_empty() {
            let (count, it) = match unlabelled.len() {
                1 => ("1 container of a removed checkout".to_string(), "it"),
                n => (format!("{n} containers of removed checkouts"), "they"),
            };
            output::skip(&format!(
                "skipping {count} without a sh.devy.host label (created by an older devy, \
                 or when devy couldn't identify the machine): {it} may be another \
                 machine's on a shared daemon"
            ));
            output::skip(&format!(
                "if this machine created {}, remove each with `{cli} rm -f <name>`, and its \
                 volume, if it has one, with `{cli} volume rm <name>`:",
                if unlabelled.len() == 1 { "it" } else { "them" }
            ));
            for (name, root) in &unlabelled {
                output::skip(&format!(
                    "  {} ({})",
                    output::clean_line(name),
                    output::clean_line(root)
                ));
            }
        }
        Ok(self
            .found
            .iter()
            .map(|c| Resource {
                kind: if self.volumes && c.has_volume {
                    format!("{cli} container and volume")
                } else {
                    format!("{cli} container")
                },
                name: c.name.clone(),
                root: c.root.clone(),
            })
            .collect())
    }

    fn remove(&self, index: usize) -> Result<()> {
        remove_orphaned_container(self.runtime()?, &self.found[index], self.host, self.volumes)
    }

    fn finish(&self) -> Result<()> {
        Ok(())
    }

    fn note(&self) -> Option<String> {
        (!self.volumes && self.found.iter().any(|c| c.has_volume))
            .then(|| "container volumes are kept; use --volumes to remove them too".into())
    }
}

/// `1 <one>` or `<n> <many>`.
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

impl<'a> Containers<'a> {
    fn runtime(&self) -> Result<&ContainerRuntime<'a>> {
        self.chosen
            .and_then(|i| self.runtimes.get(i))
            .ok_or_else(|| anyhow!("no container CLI could list containers"))
    }
}

#[cfg_attr(test, mutants::skip)] // binds stdin and the real service manager and container CLI
pub fn run(yes: bool, volumes: bool) -> Result<()> {
    // No devy.yml is needed, but when run inside a project, record its root so docker,
    // podman and systemctl are never resolved from that project's PATH entries.
    if let Ok(cwd) = std::env::current_dir() {
        let _ = crate::config::DevyConfig::locate_config(&cwd);
    }
    let mut sources: Vec<Box<dyn Source>> = Vec::new();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    sources.push(Box::new(Units {
        pruner: crate::package_manager::UnitPruner::native(),
        found: Vec::new(),
    }));
    sources.push(Box::new(Containers {
        runtimes: prune_clis(|name| crate::package_manager::system_tool(name).is_some())
            .into_iter()
            .map(ContainerRuntime::system)
            .collect(),
        host: crate::service_runner::host_id::current(),
        volumes,
        env: |k| std::env::var(k).ok(),
        chosen: None,
        found: Vec::new(),
    }));
    let stdin = std::io::stdin();
    let is_tty = stdin.is_terminal();
    run_with(&mut sources, yes, is_tty, &mut stdin.lock())
}

fn run_with(
    sources: &mut [Box<dyn Source + '_>],
    yes: bool,
    is_tty: bool,
    input: &mut dyn BufRead,
) -> Result<()> {
    // (source, index within the source, resource)
    let mut found: Vec<(usize, usize, Resource)> = Vec::new();
    for (s, source) in sources.iter_mut().enumerate() {
        match source.find() {
            Ok(resources) => {
                found.extend(resources.into_iter().enumerate().map(|(i, r)| (s, i, r)))
            }
            Err(e) => output::skip(&format!("skipping {}: {e:#}", source.what())),
        }
    }
    if found.is_empty() {
        output::skip("nothing to prune");
        return Ok(());
    }

    output::header("Resources of removed checkouts");
    for (_, _, r) in &found {
        // A recorded root comes from a unit file or container label: one line each.
        output::info(&format!(
            "{} {} ({})",
            r.kind,
            output::clean_line(&r.name),
            output::clean_line(&r.root)
        ));
    }
    for note in sources.iter().filter_map(|s| s.note()) {
        output::skip(&note);
    }
    if !yes && !confirm(found.len(), is_tty, input)? {
        output::info("nothing removed");
        return Ok(());
    }

    output::blank_line();
    let mut failed = 0;
    let mut removed_from = vec![false; sources.len()];
    for (s, i, r) in &found {
        match sources[*s].remove(*i) {
            Ok(()) => {
                removed_from[*s] = true;
                output::success(&format!("removed {} {}", r.kind, r.name));
            }
            Err(e) => {
                failed += 1;
                output::warn(&format!("could not remove {} {}: {e:#}", r.kind, r.name));
            }
        }
    }
    for (source, removed) in sources.iter().zip(removed_from) {
        if removed && let Err(e) = source.finish() {
            failed += 1;
            output::warn(&format!(
                "could not finish removing {}: {e:#}",
                source.what()
            ));
        }
    }
    if failed > 0 {
        bail!("failed to remove {failed} of {} resources", found.len());
    }
    Ok(())
}

/// Asks `Remove N resources? [y/N]`, defaulting to no. Fails when stdin isn't a terminal.
fn confirm(count: usize, is_tty: bool, input: &mut dyn BufRead) -> Result<bool> {
    if !is_tty {
        bail!("refusing to prune without --yes when not interactive");
    }
    let noun = if count == 1 { "resource" } else { "resources" };
    print!("\n  Remove {count} {noun}? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A source with fixed resources that records removals in `log`.
    struct Fake {
        what: &'static str,
        found: Result<Vec<&'static str>, &'static str>,
        fail: Vec<&'static str>,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Source for Fake {
        fn what(&self) -> &'static str {
            self.what
        }
        fn find(&mut self) -> Result<Vec<Resource>> {
            let names = self.found.clone().map_err(|e| anyhow!(e))?;
            Ok(names
                .into_iter()
                .map(|n| Resource {
                    kind: "unit".into(),
                    name: n.into(),
                    root: "/src/gone".into(),
                })
                .collect())
        }
        fn remove(&self, index: usize) -> Result<()> {
            let name = self.found.as_ref().unwrap()[index];
            if self.fail.contains(&name) {
                bail!("boom");
            }
            self.log.borrow_mut().push(format!("remove {name}"));
            Ok(())
        }
        fn finish(&self) -> Result<()> {
            self.log.borrow_mut().push(format!("finish {}", self.what));
            Ok(())
        }
    }

    fn sources(
        log: &Rc<RefCell<Vec<String>>>,
        units: Result<Vec<&'static str>, &'static str>,
        containers: Result<Vec<&'static str>, &'static str>,
    ) -> Vec<Box<dyn Source>> {
        vec![
            Box::new(Fake {
                what: "service units",
                found: units,
                fail: vec!["bad"],
                log: log.clone(),
            }),
            Box::new(Fake {
                what: "containers",
                found: containers,
                fail: vec![],
                log: log.clone(),
            }),
        ]
    }

    fn run(
        units: Result<Vec<&'static str>, &'static str>,
        containers: Result<Vec<&'static str>, &'static str>,
        yes: bool,
        is_tty: bool,
        answer: &str,
    ) -> (Result<()>, Vec<String>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut sources = sources(&log, units, containers);
        let result = run_with(&mut sources, yes, is_tty, &mut answer.as_bytes());
        let calls = log.borrow().clone();
        (result, calls)
    }

    #[test]
    fn nothing_found_succeeds_without_asking() {
        let (result, calls) = run(Ok(vec![]), Err("no CLI"), false, false, "");
        result.unwrap();
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test]
    fn non_interactive_without_yes_refuses_and_removes_nothing() {
        let (result, calls) = run(Ok(vec!["a"]), Ok(vec![]), false, false, "y\n");
        let err = result.unwrap_err().to_string();
        assert_eq!(err, "refusing to prune without --yes when not interactive");
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test]
    fn the_prompt_defaults_to_no() {
        for answer in ["\n", "n\n", "nope\n", ""] {
            let (result, calls) = run(Ok(vec!["a"]), Ok(vec![]), false, true, answer);
            result.unwrap();
            assert!(calls.is_empty(), "{answer:?}: {calls:?}");
        }
    }

    #[test]
    fn yes_at_the_prompt_or_flag_removes_everything() {
        for (yes, is_tty, answer) in [
            (false, true, "y\n"),
            (false, true, "YES\n"),
            (true, false, ""),
        ] {
            let (result, calls) = run(Ok(vec!["a"]), Ok(vec!["c"]), yes, is_tty, answer);
            result.unwrap();
            assert_eq!(
                calls,
                [
                    "remove a",
                    "remove c",
                    "finish service units",
                    "finish containers"
                ]
            );
        }
    }

    #[test]
    fn an_unavailable_source_is_skipped() {
        let (result, calls) = run(
            Ok(vec!["a"]),
            Err("neither docker nor podman"),
            true,
            false,
            "",
        );
        result.unwrap();
        assert_eq!(calls, ["remove a", "finish service units"]);
    }

    #[test]
    fn a_failed_removal_continues_and_fails_at_the_end() {
        let (result, calls) = run(Ok(vec!["bad", "a"]), Ok(vec![]), true, false, "");
        let err = result.unwrap_err().to_string();
        assert_eq!(err, "failed to remove 1 of 2 resources");
        assert_eq!(calls, ["remove a", "finish service units"]);
    }

    #[test]
    fn containers_fall_back_to_podman_when_docker_cannot_list() {
        use crate::config::ContainerCli;
        use crate::service_runner::docker::{FakeRunner, fail, ok};
        let dir = crate::test_support::tmp_dir();
        // A removed checkout beside a live one.
        std::fs::create_dir(dir.join("app")).unwrap();
        let gone = dir.join("app-feat");
        let labels = serde_json::json!({
            "Id": "c0ffee",
            "Config": {"Labels": {
                "sh.devy.project": gone.to_string_lossy(),
                "sh.devy.host": HOST,
            }},
            "Mounts": [{"Type": "volume", "Name": "devy-app-1-redis"}],
        })
        .to_string();
        let docker = FakeRunner::new(|call| match call[1].as_str() {
            "context" => ok("unix:///var/run/docker.sock"),
            _ => fail("Cannot connect to the Docker daemon"),
        });
        let podman = FakeRunner::new(move |call| match call[1].as_str() {
            "ps" => ok("devy-app-1-redis\n"),
            "info" => ok("false\n"),
            _ => ok(&labels),
        });
        let mut containers = Containers {
            runtimes: vec![
                ContainerRuntime::new(ContainerCli::Docker, &docker),
                ContainerRuntime::new(ContainerCli::Podman, &podman),
            ],
            host: Some(HOST),
            volumes: true,
            env: |_| None,
            chosen: None,
            found: Vec::new(),
        };
        let found = containers.find().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, "podman container and volume");
        assert_eq!(containers.note(), None);
        containers.remove(0).unwrap();
        let calls = podman.lines();
        assert!(
            calls.contains(&"podman rm -f c0ffee".to_string()),
            "{calls:?}"
        );
        assert!(
            calls.contains(&"podman volume rm devy-app-1-redis".to_string()),
            "{calls:?}"
        );
    }

    const HOST: &str = "0123456789abcdef";

    /// A docker CLI on a local socket listing `devy-app-1-redis` (this host's) and
    /// `devy-app-2-redis` (unlabeled), both of removed checkout `gone`.
    fn local_docker(gone: &std::path::Path) -> crate::service_runner::docker::FakeRunner {
        use crate::service_runner::docker::{FakeRunner, ok};
        let root = gone.to_string_lossy().into_owned();
        FakeRunner::new(move |call| match call[1].as_str() {
            "context" => ok("unix:///var/run/docker.sock"),
            "ps" => ok("devy-app-1-redis\ndevy-app-2-redis\n"),
            "container" => {
                let name = call.last().unwrap();
                let mut labels = serde_json::json!({"sh.devy.project": root});
                let mut id = "beef";
                if name == "devy-app-1-redis" {
                    labels["sh.devy.host"] = HOST.into();
                    id = "c0ffee";
                }
                ok(&serde_json::json!({
                    "Id": id,
                    "Config": {"Labels": labels},
                    "Mounts": [{"Type": "volume", "Name": name}],
                })
                .to_string())
            }
            _ => ok(""),
        })
    }

    #[test]
    fn containers_keep_volumes_without_the_volumes_flag() {
        use crate::config::ContainerCli;
        let dir = crate::test_support::tmp_dir();
        std::fs::create_dir(dir.join("app")).unwrap();
        let docker = local_docker(&dir.join("app-feat"));
        let mut containers = Containers {
            runtimes: vec![ContainerRuntime::new(ContainerCli::Docker, &docker)],
            host: Some(HOST),
            volumes: false,
            env: |_| None,
            chosen: None,
            found: Vec::new(),
        };
        let found = containers.find().unwrap();
        // Only this host's container; the unlabeled one is reported, not listed.
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "devy-app-1-redis");
        assert_eq!(found[0].kind, "docker container");
        assert_eq!(
            containers.note().as_deref(),
            Some("container volumes are kept; use --volumes to remove them too")
        );
        containers.remove(0).unwrap();
        let calls = docker.lines();
        assert!(
            calls.contains(&"docker rm -f c0ffee".to_string()),
            "{calls:?}"
        );
        assert!(!calls.iter().any(|c| c.contains("volume rm")), "{calls:?}");
        assert!(!calls.iter().any(|c| c.contains("rm -f beef")), "{calls:?}");
    }

    #[test]
    fn plural_counts() {
        assert_eq!(plural(1, "container", "containers"), "1 container");
        assert_eq!(plural(3, "container", "containers"), "3 containers");
    }

    #[test]
    fn containers_are_skipped_for_a_remote_daemon_or_no_cli() {
        use crate::config::ContainerCli;
        use crate::service_runner::docker::{FakeRunner, ok};
        let docker = FakeRunner::new(|_| ok("ssh://u@build-box"));
        let mut remote = Containers {
            runtimes: vec![ContainerRuntime::new(ContainerCli::Docker, &docker)],
            host: Some(HOST),
            volumes: false,
            env: |_| None,
            chosen: None,
            found: Vec::new(),
        };
        let err = remote.find().unwrap_err().to_string();
        assert!(
            err.contains("docker: docker's current context uses ssh://u@build-box"),
            "{err}"
        );
        // Only the context was asked: no containers were listed.
        assert_eq!(docker.lines().len(), 1, "{:?}", docker.lines());
        let mut none = Containers {
            runtimes: Vec::new(),
            host: Some(HOST),
            volumes: false,
            env: |_| None,
            chosen: None,
            found: Vec::new(),
        };
        let err = none.find().unwrap_err().to_string();
        assert_eq!(err, "neither docker nor podman was found");
    }
}
