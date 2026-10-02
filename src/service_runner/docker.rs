//! The container CLI layer for docker-managed services: thin wrappers over `docker` or
//! `podman` (driven through its Docker-compatible CLI), parsing their JSON output.
//! Every command goes through a `CommandRunner`, so tests can assert the exact argv.

use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::config::ContainerCli;

/// The captured result of one CLI invocation.
#[derive(Debug, Clone, Default)]
pub struct CmdOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    /// Runs `program` with `args`, capturing its output. Errors only when the program
    /// can't be started (e.g. it isn't on PATH).
    fn output(&self, program: &str, args: &[String]) -> Result<CmdOutput>;
    /// Runs `program` with `args`, its output going to the terminal (e.g. pull progress).
    /// Returns whether it succeeded.
    fn status(&self, program: &str, args: &[String]) -> Result<bool>;
}

/// Runs real processes.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    #[cfg_attr(test, mutants::skip)] // spawns real processes
    fn output(&self, program: &str, args: &[String]) -> Result<CmdOutput> {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run `{program}`"))?;
        Ok(CmdOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    #[cfg_attr(test, mutants::skip)] // spawns real processes
    fn status(&self, program: &str, args: &[String]) -> Result<bool> {
        let status = std::process::Command::new(program)
            .args(args)
            .status()
            .with_context(|| format!("Failed to run `{program}`"))?;
        Ok(status.success())
    }
}

static SYSTEM_RUNNER: SystemRunner = SystemRunner;

/// A container's state, from `<cli> container inspect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerState {
    pub running: bool,
    pub labels: HashMap<String, String>,
}

/// Everything `<cli> run` needs to create a service container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    pub name: String,
    pub hostname: String,
    pub labels: Vec<(String, String)>,
    /// `(host, container)`; host ports are published on 127.0.0.1 only.
    pub ports: Vec<(u16, u16)>,
    /// `(volume name, mount path)`.
    pub volume: Option<(String, String)>,
    pub env: Vec<(String, String)>,
    pub image: String,
    pub args: Vec<String>,
}

pub struct ContainerRuntime<'a> {
    cli: ContainerCli,
    runner: &'a dyn CommandRunner,
}

/// True when a CLI error says the object (container, volume) doesn't exist. Docker says
/// "No such container", Podman "no such container".
fn is_missing(out: &CmdOutput) -> bool {
    out.stderr.to_ascii_lowercase().contains("no such")
}

/// A repository name without the implicit Docker Hub prefixes, so `redis`,
/// `library/redis` and `docker.io/library/redis` compare equal.
fn normalize_repository(repo: &str) -> &str {
    let repo = repo
        .strip_prefix("docker.io/")
        .or_else(|| repo.strip_prefix("index.docker.io/"))
        .unwrap_or(repo);
    repo.strip_prefix("library/").unwrap_or(repo)
}

impl<'a> ContainerRuntime<'a> {
    pub fn new(cli: ContainerCli, runner: &'a dyn CommandRunner) -> Self {
        Self { cli, runner }
    }

    /// The runtime that runs the real `docker` or `podman`.
    pub fn system(cli: ContainerCli) -> ContainerRuntime<'static> {
        ContainerRuntime::new(cli, &SYSTEM_RUNNER)
    }

    pub fn cli_name(&self) -> &'static str {
        self.cli.binary()
    }

    fn output(&self, args: &[&str]) -> Result<CmdOutput> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        self.runner.output(self.cli_name(), &args)
    }

    /// Fails unless the CLI is on PATH and its daemon (or Podman service) answers.
    pub fn ensure_available(&self) -> Result<()> {
        let ok = self
            .output(&["info", "--format", "{{json .ServerVersion}}"])
            .is_ok_and(|out| out.success);
        if !ok {
            anyhow::bail!(
                "{} is not available — install it or start its daemon, or set service_manager: package",
                self.cli_name()
            );
        }
        Ok(())
    }

    /// Whether `reference` is present locally.
    pub fn image_present(&self, reference: &str) -> Result<bool> {
        Ok(self.output(&["image", "inspect", reference])?.success)
    }

    /// Pulls `reference`, showing the CLI's progress output.
    pub fn pull(&self, reference: &str) -> Result<()> {
        let args = vec!["pull".to_string(), reference.to_string()];
        if !self.runner.status(self.cli_name(), &args)? {
            anyhow::bail!("Failed to pull {reference}");
        }
        Ok(())
    }

    /// The repository digest of the local image `reference`, as
    /// `<repository>@sha256:<hex>`. `None` for images that were never pushed to or pulled
    /// from a registry (e.g. built locally).
    pub fn image_digest(&self, reference: &str, repository: &str) -> Result<Option<String>> {
        let out = self.output(&[
            "image",
            "inspect",
            "--format",
            "{{json .RepoDigests}}",
            reference,
        ])?;
        if !out.success {
            anyhow::bail!(
                "{} image inspect {reference} failed: {}",
                self.cli_name(),
                out.stderr.trim()
            );
        }
        let digests: Option<Vec<String>> = serde_json::from_str(out.stdout.trim())
            .with_context(|| format!("Unexpected image inspect output for {reference}"))?;
        let wanted = normalize_repository(repository);
        Ok(digests.unwrap_or_default().iter().find_map(|entry| {
            let (repo, digest) = entry.split_once('@')?;
            (normalize_repository(repo) == wanted).then(|| format!("{repository}@{digest}"))
        }))
    }

    /// The state of container `name`, or `None` when it doesn't exist.
    pub fn inspect_container(&self, name: &str) -> Result<Option<ContainerState>> {
        let out = self.output(&["container", "inspect", "--format", "{{json .}}", name])?;
        if !out.success {
            if is_missing(&out) {
                return Ok(None);
            }
            anyhow::bail!(
                "{} container inspect {name} failed: {}",
                self.cli_name(),
                out.stderr.trim()
            );
        }
        let value: serde_json::Value = serde_json::from_str(out.stdout.trim())
            .with_context(|| format!("Unexpected container inspect output for {name}"))?;
        // `--format {{json .}}` prints one object; tolerate the list plain `inspect` prints.
        let value = match value {
            serde_json::Value::Array(mut items) if !items.is_empty() => items.swap_remove(0),
            other => other,
        };
        let running = value["State"]["Running"].as_bool().unwrap_or(false);
        let labels = value["Config"]["Labels"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(ContainerState { running, labels }))
    }

    /// The argv (after the CLI name) that creates and starts `spec`'s container.
    pub fn run_args(spec: &RunSpec) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            spec.name.clone(),
            "--hostname".into(),
            spec.hostname.clone(),
        ];
        for (k, v) in &spec.labels {
            args.push("--label".into());
            args.push(format!("{k}={v}"));
        }
        for (host, container) in &spec.ports {
            args.push("-p".into());
            args.push(format!("127.0.0.1:{host}:{container}"));
        }
        if let Some((volume, path)) = &spec.volume {
            args.push("-v".into());
            args.push(format!("{volume}:{path}"));
        }
        for (k, v) in &spec.env {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        args.push(spec.image.clone());
        args.extend(spec.args.iter().cloned());
        args
    }

    /// Creates and starts a container. Fails with the CLI's error output.
    pub fn run(&self, spec: &RunSpec) -> Result<()> {
        let out = self.runner.output(self.cli_name(), &Self::run_args(spec))?;
        if !out.success {
            anyhow::bail!("{}", out.stderr.trim());
        }
        Ok(())
    }

    fn simple(&self, args: &[&str], tolerate_missing: bool) -> Result<()> {
        let out = self.output(args)?;
        let tolerated = tolerate_missing && is_missing(&out);
        if !(out.success || tolerated) {
            anyhow::bail!(
                "{} {} failed: {}",
                self.cli_name(),
                args.join(" "),
                out.stderr.trim()
            );
        }
        Ok(())
    }

    pub fn start(&self, name: &str) -> Result<()> {
        self.simple(&["start", name], false)
    }

    pub fn stop(&self, name: &str) -> Result<()> {
        self.simple(&["stop", name], false)
    }

    /// Force-removes container `name`; a missing container is not an error.
    pub fn remove_container(&self, name: &str) -> Result<()> {
        self.simple(&["rm", "-f", name], true)
    }

    /// Removes volume `name`; a missing volume is not an error.
    pub fn remove_volume(&self, name: &str) -> Result<()> {
        self.simple(&["volume", "rm", name], true)
    }
}

/// Answers one recorded call (program first) with its output.
#[cfg(test)]
type Respond = dyn Fn(&[String]) -> CmdOutput;

/// A scripted `CommandRunner` for tests: records every call (program first) and answers
/// each with `respond`.
#[cfg(test)]
pub(crate) struct FakeRunner {
    pub calls: std::cell::RefCell<Vec<Vec<String>>>,
    respond: Box<Respond>,
}

#[cfg(test)]
impl FakeRunner {
    pub fn new(respond: impl Fn(&[String]) -> CmdOutput + 'static) -> Self {
        Self {
            calls: std::cell::RefCell::new(Vec::new()),
            respond: Box::new(respond),
        }
    }

    /// Every call succeeds with empty output.
    pub fn ok() -> Self {
        Self::new(|_| ok(""))
    }

    /// Each recorded call as one space-joined string, e.g. `docker start devy-app-redis`.
    pub fn lines(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c.join(" ")).collect()
    }

    fn record(&self, program: &str, args: &[String]) -> CmdOutput {
        let mut call = vec![program.to_string()];
        call.extend(args.iter().cloned());
        let out = (self.respond)(&call);
        self.calls.borrow_mut().push(call);
        out
    }
}

#[cfg(test)]
impl CommandRunner for FakeRunner {
    fn output(&self, program: &str, args: &[String]) -> Result<CmdOutput> {
        Ok(self.record(program, args))
    }
    fn status(&self, program: &str, args: &[String]) -> Result<bool> {
        Ok(self.record(program, args).success)
    }
}

#[cfg(test)]
pub(crate) fn ok(stdout: &str) -> CmdOutput {
    CmdOutput {
        success: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

#[cfg(test)]
pub(crate) fn fail(stderr: &str) -> CmdOutput {
    CmdOutput {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(cli: ContainerCli, runner: &FakeRunner) -> ContainerRuntime<'_> {
        ContainerRuntime::new(cli, runner)
    }

    const BOTH: [ContainerCli; 2] = [ContainerCli::Docker, ContainerCli::Podman];

    #[test]
    fn availability_runs_info() {
        for cli in BOTH {
            let fake = FakeRunner::new(|_| ok("\"27.0.3\""));
            runtime(cli, &fake).ensure_available().unwrap();
            assert_eq!(
                fake.lines(),
                vec![format!(
                    "{} info --format {{{{json .ServerVersion}}}}",
                    cli.binary()
                )]
            );
        }
    }

    #[test]
    fn availability_error_when_info_fails() {
        for cli in BOTH {
            let fake = FakeRunner::new(|_| fail("Cannot connect to the Docker daemon"));
            let err = runtime(cli, &fake).ensure_available().unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "{} is not available — install it or start its daemon, or set service_manager: package",
                    cli.binary()
                )
            );
        }
    }

    #[test]
    fn availability_error_when_cli_missing() {
        struct Missing;
        impl CommandRunner for Missing {
            fn output(&self, program: &str, _: &[String]) -> Result<CmdOutput> {
                anyhow::bail!("Failed to run `{program}`")
            }
            fn status(&self, program: &str, _: &[String]) -> Result<bool> {
                anyhow::bail!("Failed to run `{program}`")
            }
        }
        let err = ContainerRuntime::new(ContainerCli::Podman, &Missing)
            .ensure_available()
            .unwrap_err();
        assert!(err.to_string().starts_with("podman is not available"));
    }

    #[test]
    fn image_present_and_pull_argv() {
        for cli in BOTH {
            let fake = FakeRunner::new(|call| {
                if call[1] == "image" {
                    fail("No such image")
                } else {
                    ok("")
                }
            });
            let rt = runtime(cli, &fake);
            assert!(!rt.image_present("redis:7").unwrap());
            rt.pull("redis:7").unwrap();
            let bin = cli.binary();
            assert_eq!(
                fake.lines(),
                vec![
                    format!("{bin} image inspect redis:7"),
                    format!("{bin} pull redis:7")
                ]
            );
        }
    }

    #[test]
    fn pull_failure_names_reference() {
        let fake = FakeRunner::new(|_| fail("manifest unknown"));
        let err = runtime(ContainerCli::Docker, &fake)
            .pull("redis:nope")
            .unwrap_err();
        assert_eq!(err.to_string(), "Failed to pull redis:nope");
    }

    #[test]
    fn image_digest_matches_repository() {
        // Docker reports the short name, Podman the fully qualified one.
        for (cli, stdout) in [
            (
                ContainerCli::Docker,
                r#"["other/redis@sha256:000","redis@sha256:abc"]"#,
            ),
            (
                ContainerCli::Podman,
                r#"["docker.io/library/redis@sha256:abc"]"#,
            ),
        ] {
            let fake = FakeRunner::new(move |_| ok(stdout));
            let rt = runtime(cli, &fake);
            assert_eq!(
                rt.image_digest("redis:7", "redis").unwrap().as_deref(),
                Some("redis@sha256:abc")
            );
            assert_eq!(
                fake.lines(),
                vec![format!(
                    "{} image inspect --format {{{{json .RepoDigests}}}} redis:7",
                    cli.binary()
                )]
            );
        }
    }

    #[test]
    fn image_digest_none_for_local_images() {
        for stdout in ["[]", "null"] {
            let fake = FakeRunner::new(move |_| ok(stdout));
            assert_eq!(
                runtime(ContainerCli::Docker, &fake)
                    .image_digest("mine:dev", "mine")
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn image_digest_for_mirror_repository() {
        let fake = FakeRunner::new(|_| ok(r#"["registry.corp.example/mirror/redis@sha256:def"]"#));
        assert_eq!(
            runtime(ContainerCli::Docker, &fake)
                .image_digest(
                    "registry.corp.example/mirror/redis:7",
                    "registry.corp.example/mirror/redis"
                )
                .unwrap()
                .as_deref(),
            Some("registry.corp.example/mirror/redis@sha256:def")
        );
    }

    const RUNNING: &str = r#"{"State":{"Running":true,"Status":"running"},"Config":{"Labels":{"sh.devy.config":"abc","sh.devy.service":"redis"}}}"#;
    const STOPPED: &str =
        r#"[{"State":{"Running":false,"Status":"exited"},"Config":{"Labels":null}}]"#;

    #[test]
    fn inspect_container_running() {
        for cli in BOTH {
            let fake = FakeRunner::new(|_| ok(RUNNING));
            let state = runtime(cli, &fake)
                .inspect_container("devy-app-1234abcd-redis")
                .unwrap()
                .unwrap();
            assert!(state.running);
            assert_eq!(state.labels["sh.devy.config"], "abc");
            assert_eq!(
                fake.lines(),
                vec![format!(
                    "{} container inspect --format {{{{json .}}}} devy-app-1234abcd-redis",
                    cli.binary()
                )]
            );
        }
    }

    #[test]
    fn inspect_container_stopped_without_labels() {
        let fake = FakeRunner::new(|_| ok(STOPPED));
        let state = runtime(ContainerCli::Podman, &fake)
            .inspect_container("c")
            .unwrap()
            .unwrap();
        assert!(!state.running);
        assert!(state.labels.is_empty());
    }

    #[test]
    fn inspect_container_missing_is_none() {
        for (cli, stderr) in [
            (ContainerCli::Docker, "Error: No such container: c"),
            (
                ContainerCli::Podman,
                "Error: inspecting object: no such container \"c\"",
            ),
        ] {
            let fake = FakeRunner::new(move |_| fail(stderr));
            assert_eq!(runtime(cli, &fake).inspect_container("c").unwrap(), None);
        }
    }

    #[test]
    fn inspect_container_other_failure_is_an_error() {
        let fake = FakeRunner::new(|_| fail("permission denied"));
        assert!(
            runtime(ContainerCli::Docker, &fake)
                .inspect_container("c")
                .is_err()
        );
    }

    fn spec() -> RunSpec {
        RunSpec {
            name: "devy-app-1234abcd-minio".into(),
            hostname: "minio".into(),
            labels: vec![("sh.devy.service".into(), "minio".into())],
            ports: vec![(51000, 9000), (51001, 9001)],
            volume: Some(("devy-app-1234abcd-minio".into(), "/data".into())),
            env: vec![("MINIO_ROOT_USER".into(), "me".into())],
            image: "minio/minio:latest".into(),
            args: vec!["server".into(), "/data".into()],
        }
    }

    #[test]
    fn run_argv() {
        for cli in BOTH {
            let fake = FakeRunner::ok();
            runtime(cli, &fake).run(&spec()).unwrap();
            assert_eq!(
                fake.lines(),
                vec![format!(
                    "{} run -d --name devy-app-1234abcd-minio --hostname minio \
                     --label sh.devy.service=minio -p 127.0.0.1:51000:9000 \
                     -p 127.0.0.1:51001:9001 -v devy-app-1234abcd-minio:/data \
                     -e MINIO_ROOT_USER=me minio/minio:latest server /data",
                    cli.binary()
                )]
            );
        }
    }

    #[test]
    fn run_failure_reports_cli_stderr() {
        let fake = FakeRunner::new(|_| fail("port is already allocated\n"));
        let err = runtime(ContainerCli::Docker, &fake)
            .run(&spec())
            .unwrap_err();
        assert_eq!(err.to_string(), "port is already allocated");
    }

    #[test]
    fn lifecycle_argv() {
        for cli in BOTH {
            let fake = FakeRunner::ok();
            let rt = runtime(cli, &fake);
            rt.start("c").unwrap();
            rt.stop("c").unwrap();
            rt.remove_container("c").unwrap();
            rt.remove_volume("c").unwrap();
            let bin = cli.binary();
            assert_eq!(
                fake.lines(),
                vec![
                    format!("{bin} start c"),
                    format!("{bin} stop c"),
                    format!("{bin} rm -f c"),
                    format!("{bin} volume rm c"),
                ]
            );
        }
    }

    #[test]
    fn removing_missing_objects_is_not_an_error() {
        let fake = FakeRunner::new(|_| fail("Error: No such volume: c"));
        let rt = runtime(ContainerCli::Docker, &fake);
        rt.remove_container("c").unwrap();
        rt.remove_volume("c").unwrap();
        assert!(rt.stop("c").is_err(), "stop must not hide failures");
    }
}
