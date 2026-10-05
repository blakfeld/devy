//! The container CLI layer for docker-managed services: thin wrappers over `docker` or
//! `podman` (driven through its Docker-compatible CLI), parsing their JSON output.
//! Every command goes through a `CommandRunner`, so tests can assert the exact argv.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

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
    /// `output`, but the program is killed and an error returned when it hasn't finished
    /// within `timeout` (e.g. a wedged daemon). Fakes that answer at once keep this default.
    fn output_within(
        &self,
        program: &str,
        args: &[String],
        _timeout: Duration,
    ) -> Result<CmdOutput> {
        self.output(program, args)
    }
    /// Runs `program` with `args`, its output going to the terminal (e.g. pull progress).
    /// Returns whether it succeeded.
    fn status(&self, program: &str, args: &[String]) -> Result<bool>;
}

/// Runs real processes. The program is resolved with `package_manager::system_tool`, so a
/// `docker`/`podman` a repository puts on PATH is never run.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    #[cfg_attr(test, mutants::skip)] // spawns real processes
    fn output(&self, program: &str, args: &[String]) -> Result<CmdOutput> {
        let out = std::process::Command::new(crate::package_manager::require_system_tool(program)?)
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
    fn output_within(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<CmdOutput> {
        output_within_path(
            &crate::package_manager::require_system_tool(program)?,
            program,
            args,
            timeout,
        )
    }

    #[cfg_attr(test, mutants::skip)] // spawns real processes
    fn status(&self, program: &str, args: &[String]) -> Result<bool> {
        let status =
            std::process::Command::new(crate::package_manager::require_system_tool(program)?)
                .args(args)
                .status()
                .with_context(|| format!("Failed to run `{program}`"))?;
        Ok(status.success())
    }
}

/// Runs the executable at `path` (shown as `program`) with `args`, capturing its output,
/// and kills it when it hasn't finished within `timeout`.
fn output_within_path(
    path: &Path,
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<CmdOutput> {
    let mut child = std::process::Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run `{program}`"))?;
    let deadline = Instant::now() + timeout;
    // The results are awaited only until `deadline` (plus a grace): a process the CLI
    // started that keeps the pipes open after the CLI exits must not hold devy past it.
    let stdout = read_pipe(Box::new(child.stdout.take().expect("stdout is piped")));
    let stderr = read_pipe(Box::new(child.stderr.take().expect("stderr is piped")));
    let timed_out = || {
        anyhow::anyhow!(
            "`{program}` did not answer within {}",
            format_timeout(timeout)
        )
    };
    let Some(status) = wait_within(&mut child, Some(timeout))
        .with_context(|| format!("Failed to wait for `{program}`"))?
    else {
        return Err(timed_out());
    };
    Ok(CmdOutput {
        success: status.success(),
        stdout: recv_pipe(&stdout, Some(deadline)).ok_or_else(timed_out)?,
        stderr: recv_pipe(&stderr, Some(deadline)).ok_or_else(timed_out)?,
    })
}

/// How long a pipe read may run past its deadline. A child that exits just before the
/// deadline leaves its readers almost no time to drain the pipes; without this a
/// finished run would be reported as timed out.
const PIPE_GRACE: Duration = Duration::from_millis(100);

/// Reads all of `pipe` on its own (detached) thread, which sends what it read.
pub(crate) fn read_pipe(
    mut pipe: Box<dyn std::io::Read + Send>,
) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    rx
}

/// What a [`read_pipe`] thread read, waiting until `deadline` (at least [`PIPE_GRACE`]
/// from now), or without limit when there is none. `None` when it timed out; a reader
/// that panicked counts as having read nothing.
pub(crate) fn recv_pipe(
    rx: &std::sync::mpsc::Receiver<String>,
    deadline: Option<Instant>,
) -> Option<String> {
    let received = match deadline {
        Some(deadline) => rx.recv_timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .max(PIPE_GRACE),
        ),
        None => rx
            .recv()
            .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
    };
    match received {
        Ok(text) => Some(text),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Some(String::new()),
    }
}

/// `timeout` for messages: whole seconds as `5 s`, anything else in milliseconds
/// (`300 ms`, `1500 ms`), so a sub-second timeout never reads as `0 s`.
pub(crate) fn format_timeout(timeout: Duration) -> String {
    if timeout.subsec_nanos() == 0 {
        format!("{} s", timeout.as_secs())
    } else {
        format!("{} ms", timeout.as_millis())
    }
}

/// Waits for `child` to exit. With a `timeout`, a child still running when it passes is
/// killed and reaped, and `None` is returned.
pub(crate) fn wait_within(
    child: &mut Child,
    timeout: Option<Duration>,
) -> std::io::Result<Option<ExitStatus>> {
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(e) => {
                // Don't leave it running (or unreaped) behind the error.
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

static SYSTEM_RUNNER: SystemRunner = SystemRunner;

/// A container's state, from `<cli> container inspect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerState {
    /// The container's full ID (empty if the CLI didn't report one).
    pub id: String,
    pub running: bool,
    pub labels: HashMap<String, String>,
    /// The names of the named volumes it mounts.
    pub volumes: Vec<String>,
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
    /// Credentials, written to a mode-0600 file passed with `--env-file` (and deleted once
    /// `run` returns) so they never appear in the CLI's argv.
    pub secret_env: Vec<(String, String)>,
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

    pub fn cli(&self) -> ContainerCli {
        self.cli
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
        self.inspect_container_within(name, None)
    }

    /// [`Self::inspect_container`], failing when the CLI hasn't answered within `timeout`
    /// (when one is given).
    pub fn inspect_container_within(
        &self,
        name: &str,
        timeout: Option<Duration>,
    ) -> Result<Option<ContainerState>> {
        let args = ["container", "inspect", "--format", "{{json .}}", name];
        let out = match timeout {
            Some(t) => {
                let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                self.runner.output_within(self.cli_name(), &args, t)?
            }
            None => self.output(&args)?,
        };
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
        let id = value["Id"].as_str().unwrap_or_default().to_string();
        let volumes = value["Mounts"]
            .as_array()
            .map(|mounts| {
                mounts
                    .iter()
                    .filter(|m| m["Type"] == "volume")
                    .filter_map(|m| m["Name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(ContainerState {
            id,
            running,
            labels,
            volumes,
        }))
    }

    /// The daemon endpoint of docker's current context (honoring `DOCKER_CONTEXT`), e.g.
    /// `unix:///var/run/docker.sock` or `ssh://host`.
    pub fn context_endpoint(&self) -> Result<String> {
        let out = self.output(&[
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ])?;
        if !out.success {
            anyhow::bail!(
                "{} context inspect failed: {}",
                self.cli_name(),
                out.stderr.trim()
            );
        }
        Ok(out.stdout.trim().to_string())
    }

    /// What podman reports as `.Host.ServiceIsRemote`: `true` when it is podman-remote or
    /// set up (`containers.conf`, a default system connection) to use another host.
    pub fn podman_service_is_remote(&self) -> Result<String> {
        let out = self.output(&["info", "--format", "{{.Host.ServiceIsRemote}}"])?;
        if !out.success {
            anyhow::bail!("{} info failed: {}", self.cli_name(), out.stderr.trim());
        }
        Ok(out.stdout.trim().to_string())
    }

    /// The names of all containers, running or not, that carry label `key`.
    pub fn containers_with_label(&self, key: &str) -> Result<Vec<String>> {
        let filter = format!("label={key}");
        let out = self.output(&["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"])?;
        if !out.success {
            anyhow::bail!("{} ps failed: {}", self.cli_name(), out.stderr.trim());
        }
        Ok(out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }

    /// The argv (after the CLI name) that creates and starts `spec`'s container, reading
    /// its `secret_env` from `env_file` when given.
    pub fn run_args(spec: &RunSpec, env_file: Option<&Path>) -> Vec<String> {
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
        if let Some(path) = env_file {
            args.push("--env-file".into());
            args.push(path.to_string_lossy().into_owned());
        }
        args.push(spec.image.clone());
        args.extend(spec.args.iter().cloned());
        args
    }

    /// Creates and starts a container. Fails with the CLI's error output.
    pub fn run(&self, spec: &RunSpec) -> Result<()> {
        // Kept alive until the CLI returns; dropping it deletes the file.
        let env_file = if spec.secret_env.is_empty() {
            None
        } else {
            Some(write_env_file(&spec.secret_env)?)
        };
        let args = Self::run_args(spec, env_file.as_ref().map(|f| f.path()));
        let out = self.runner.output(self.cli_name(), &args);
        drop(env_file);
        let out = out?;
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

/// Writes `pairs` as a Docker env file (`KEY=value` lines, read literally) to a new
/// temporary file readable only by the current user. Rejects names that aren't plain
/// identifiers and values containing line breaks or NUL, which the format can't carry;
/// errors name the variable but never its value.
fn write_env_file(pairs: &[(String, String)]) -> Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut body = String::new();
    for (k, v) in pairs {
        let valid_name = k
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid_name {
            anyhow::bail!("invalid container environment variable name {k:?}");
        }
        if v.contains(['\n', '\r', '\0']) {
            anyhow::bail!("{k} must not contain line breaks or NUL characters");
        }
        body.push_str(&format!("{k}={v}\n"));
    }
    // tempfile creates the file with mode 0600 on Unix (set again below to be explicit);
    // on Windows it lives in the per-user temp directory.
    let mut file = tempfile::Builder::new()
        .prefix("devy-env-")
        .tempfile()
        .context("Failed to create a temporary env file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))
            .context("Failed to restrict the temporary env file")?;
    }
    file.write_all(body.as_bytes())
        .and_then(|()| file.flush())
        .context("Failed to write the temporary env file")?;
    Ok(file)
}

/// A container CLI whose daemon is wedged: every call runs a real process that never
/// answers, through the same timeout machinery as [`SystemRunner`].
#[cfg(all(test, unix))]
pub(crate) struct HangingRunner;

#[cfg(all(test, unix))]
impl CommandRunner for HangingRunner {
    fn output(&self, program: &str, args: &[String]) -> Result<CmdOutput> {
        // Untimed calls must not happen in the tests that use this; bound them anyway.
        self.output_within(program, args, Duration::from_secs(60))
    }
    fn output_within(&self, program: &str, _: &[String], timeout: Duration) -> Result<CmdOutput> {
        let args = ["-c".to_string(), "exec sleep 60".to_string()];
        output_within_path(Path::new("/bin/sh"), program, &args, timeout)
    }
    fn status(&self, program: &str, args: &[String]) -> Result<bool> {
        Ok(self.output(program, args)?.success)
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
        assert_eq!(state.id, "");
        assert!(state.volumes.is_empty());
    }

    #[test]
    fn inspect_container_reads_id_and_named_volumes() {
        let json = r#"{"Id":"4f1c9e","State":{"Running":true},"Config":{"Labels":{}},
            "Mounts":[{"Type":"volume","Name":"devy-app-1234abcd-redis","Destination":"/data"},
                      {"Type":"bind","Source":"/src","Destination":"/src"},
                      {"Type":"volume","Destination":"/anon"}]}"#;
        let fake = FakeRunner::new(move |_| ok(json));
        let state = runtime(ContainerCli::Docker, &fake)
            .inspect_container("c")
            .unwrap()
            .unwrap();
        assert_eq!(state.id, "4f1c9e");
        assert_eq!(state.volumes, ["devy-app-1234abcd-redis"]);
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
            env: vec![("TZ".into(), "UTC".into())],
            secret_env: vec![],
            image: "pgsty/silo:RELEASE.2026-09-16T00-00-00Z".into(),
            args: vec!["server".into(), "/data".into()],
        }
    }

    fn secret_spec() -> RunSpec {
        RunSpec {
            secret_env: vec![
                ("MINIO_ROOT_USER".into(), "me".into()),
                ("MINIO_ROOT_PASSWORD".into(), "hunter2-secret".into()),
            ],
            ..spec()
        }
    }

    /// The `--env-file` path in a recorded `run` call.
    fn env_file_arg(call: &[String]) -> Option<String> {
        call.iter()
            .position(|a| a == "--env-file")
            .map(|i| call[i + 1].clone())
    }

    #[test]
    fn run_passes_secrets_through_env_file_not_argv() {
        for cli in BOTH {
            // Captured while the CLI "runs": (path, contents, unix mode).
            let seen = std::rc::Rc::new(std::cell::RefCell::new(None));
            let seen_in = std::rc::Rc::clone(&seen);
            let fake = FakeRunner::new(move |call| {
                let path = env_file_arg(call).expect("--env-file passed");
                let contents = std::fs::read_to_string(&path).unwrap();
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777
                };
                #[cfg(not(unix))]
                let mode = 0o600;
                *seen_in.borrow_mut() = Some((path, contents, mode));
                ok("")
            });
            runtime(cli, &fake).run(&secret_spec()).unwrap();
            let line = &fake.lines()[0];
            assert!(!line.contains("hunter2-secret"), "{line}");
            assert!(!line.contains("MINIO_ROOT_PASSWORD"), "{line}");
            assert!(!line.contains("-e MINIO_ROOT_USER"), "{line}");
            let (path, contents, mode) = seen.borrow_mut().take().unwrap();
            assert_eq!(
                *line,
                format!(
                    "{} run -d --name devy-app-1234abcd-minio --hostname minio \
                     --label sh.devy.service=minio -p 127.0.0.1:51000:9000 \
                     -p 127.0.0.1:51001:9001 -v devy-app-1234abcd-minio:/data \
                     -e TZ=UTC --env-file {path} \
                     pgsty/silo:RELEASE.2026-09-16T00-00-00Z server /data",
                    cli.binary()
                )
            );
            assert_eq!(
                contents,
                "MINIO_ROOT_USER=me\nMINIO_ROOT_PASSWORD=hunter2-secret\n"
            );
            assert_eq!(mode, 0o600);
            assert!(
                !std::path::Path::new(&path).exists(),
                "the env file is deleted once run returns"
            );
        }
    }

    #[test]
    fn run_deletes_env_file_when_run_fails() {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen_in = std::rc::Rc::clone(&seen);
        let fake = FakeRunner::new(move |call| {
            *seen_in.borrow_mut() = env_file_arg(call);
            fail("boom")
        });
        assert!(
            runtime(ContainerCli::Docker, &fake)
                .run(&secret_spec())
                .is_err()
        );
        let path = seen.borrow_mut().take().unwrap();
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn run_rejects_secrets_the_env_file_format_cannot_carry() {
        for (k, v) in [
            ("MINIO_ROOT_PASSWORD", "a\nEVIL=1"),
            ("MINIO_ROOT_PASSWORD", "a\rb"),
            ("MINIO_ROOT_PASSWORD", "a\0b"),
            ("BAD NAME", "x"),
            ("1BAD", "x"),
            ("", "x"),
        ] {
            let fake = FakeRunner::ok();
            let spec = RunSpec {
                secret_env: vec![(k.into(), v.into())],
                ..spec()
            };
            let err = runtime(ContainerCli::Docker, &fake).run(&spec).unwrap_err();
            assert!(!err.to_string().contains("EVIL"), "{err}");
            assert!(fake.lines().is_empty(), "nothing runs for {k:?}");
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
                     -e TZ=UTC pgsty/silo:RELEASE.2026-09-16T00-00-00Z server /data",
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

    #[cfg(unix)]
    fn sh(script: &str, timeout: Duration) -> Result<CmdOutput> {
        let args = ["-c".to_string(), script.to_string()];
        output_within_path(Path::new("/bin/sh"), "docker", &args, timeout)
    }

    #[cfg(unix)]
    #[test]
    fn output_within_captures_both_streams() {
        let out = sh("echo out; echo err >&2; exit 3", Duration::from_secs(20)).unwrap();
        assert!(!out.success);
        assert_eq!(out.stdout, "out\n");
        assert_eq!(out.stderr, "err\n");
    }

    /// A process the CLI leaves behind holding its pipes open can't keep devy waiting
    /// past the timeout once the CLI itself has exited.
    #[cfg(unix)]
    #[test]
    fn output_within_does_not_wait_on_pipes_held_past_the_timeout() {
        let start = Instant::now();
        let err = sh("sleep 5 & echo started", Duration::from_millis(300)).unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(err.to_string(), "`docker` did not answer within 300 ms");
    }

    #[test]
    fn timeouts_are_formatted_without_rounding_to_zero() {
        assert_eq!(format_timeout(Duration::from_secs(5)), "5 s");
        assert_eq!(format_timeout(Duration::from_millis(300)), "300 ms");
        assert_eq!(format_timeout(Duration::from_millis(1500)), "1500 ms");
        assert_eq!(format_timeout(Duration::ZERO), "0 s");
    }

    /// A deadline that has (just) passed still leaves the readers a grace to deliver
    /// what the exited child wrote; a reader that never finishes still times out.
    #[test]
    fn pipe_reads_get_a_grace_past_the_deadline() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let _ = tx.send("done".to_string());
        });
        assert_eq!(
            recv_pipe(&rx, Some(Instant::now())).as_deref(),
            Some("done")
        );

        let (_held, rx) = std::sync::mpsc::channel::<String>();
        assert_eq!(recv_pipe(&rx, Some(Instant::now())), None);

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        drop(tx);
        assert_eq!(recv_pipe(&rx, None).as_deref(), Some(""));
    }
}
