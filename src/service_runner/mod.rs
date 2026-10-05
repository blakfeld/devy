//! What runs a dependency's service: the package manager (`PackageRunner`) or a
//! per-project container (`DockerRunner`). Commands pick one per dependency with
//! `Runners::runner_for`, so docker-managed services coexist with packages installed by
//! brew, apt or nix in the same project.

pub(crate) mod docker;
pub(crate) mod host_id;
pub(crate) mod prune;

use anyhow::Result;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::{ContainerCli, Dependency, DevyConfig};
use crate::lock::LockFile;
use crate::modules::{self, DockerSpec, ImageRef};
use crate::output;
use crate::package_manager::{LogCommand, LogCommandKind, LogSource, PackageManager};
use docker::{ContainerRuntime, RunSpec};

/// Container label holding the project root.
pub const PROJECT_LABEL: &str = "sh.devy.project";
/// Container label holding the dependency's canonical name.
pub const SERVICE_LABEL: &str = "sh.devy.service";
/// Container label holding the id of the machine and user that created the container
/// (`host_id::current`; left off when there is none). `devy prune` only removes
/// containers with this machine's id;
/// other commands find containers by name and `sh.devy.project`, so containers created
/// before the label existed keep working.
pub const HOST_LABEL: &str = "sh.devy.host";
/// Container label holding a hash of everything the container was created with; a
/// container whose hash differs from the current configuration is recreated.
pub const CONFIG_LABEL: &str = "sh.devy.config";

/// Lock `source` recorded for docker-managed services.
pub const DOCKER_SOURCE: &str = "docker";

pub trait ServiceRunner {
    /// Shown after a service's name, e.g. `redis (docker)`. `None` for the package manager.
    fn label(&self) -> Option<&'static str>;
    fn is_installed(&self, dep: &Dependency) -> Result<bool>;
    fn install(&self, dep: &Dependency) -> Result<()>;
    /// `(resolved version, image digest)` to record in devy.lock.
    fn resolved(&self, dep: &Dependency) -> Result<(Option<String>, Option<String>)>;
    fn is_running(&self, dep: &Dependency) -> Result<bool>;
    fn start(&self, dep: &Dependency) -> Result<()>;
    fn stop(&self, dep: &Dependency) -> Result<()>;
    /// Moves the service off outdated unit names; called before it is stopped, never by
    /// read-only commands. Starting migrates by itself, once the new unit is ready to
    /// start. Containers have nothing to migrate.
    fn migrate(&self, _dep: &Dependency) -> Result<()> {
        Ok(())
    }
    /// Whether the service still has units under outdated names, which `start` would
    /// replace. Read-only. A service running only under such a name must still be
    /// started. Containers have nothing to migrate.
    fn needs_migration(&self, _dep: &Dependency) -> bool {
        false
    }
    /// Removes the service's container and, with `volumes`, its data volume. Returns
    /// whether there was anything this runner could remove (only containers can be).
    fn remove(&self, dep: &Dependency, volumes: bool) -> Result<bool>;
    /// Where the service's logs can be read: the last `lines` lines and, with `follow`,
    /// new output as it is written.
    fn log_source(&self, dep: &Dependency, lines: u32, follow: bool) -> Result<LogSource>;
    /// [`Self::log_source`] without follow, for callers that must not hang: any command
    /// run to build the source (e.g. the container owner check) is killed after `timeout`.
    fn log_source_within(
        &self,
        dep: &Dependency,
        lines: u32,
        _timeout: std::time::Duration,
    ) -> Result<LogSource> {
        self.log_source(dep, lines, false)
    }

    /// Polls `is_running` until the service has stopped or the module's shutdown
    /// attempts are exhausted.
    fn wait_for_stopped(&self, dep: &Dependency) -> Result<()> {
        let cfg = modules::get(&dep.name).service_config();
        let max = cfg.shutdown_max_attempts.max(1);
        for attempt in 1..=max {
            if !self.is_running(dep)? {
                return Ok(());
            }
            if attempt < max {
                std::thread::sleep(std::time::Duration::from_millis(cfg.shutdown_sleep_ms));
            }
        }
        anyhow::bail!(
            "{} did not stop after {max} attempts — try stopping it manually or run devy logs {}",
            dep.name,
            dep.name
        )
    }
}

/// Today's behavior: the module installs through the package manager and runs its
/// service through the package manager's service backend.
pub struct PackageRunner<'a> {
    pm: &'a dyn PackageManager,
    project_root: &'a Path,
}

impl<'a> PackageRunner<'a> {
    pub fn new(pm: &'a dyn PackageManager, project_root: &'a Path) -> Self {
        Self { pm, project_root }
    }

    pub fn pm(&self) -> &'a dyn PackageManager {
        self.pm
    }
}

impl ServiceRunner for PackageRunner<'_> {
    fn label(&self) -> Option<&'static str> {
        None
    }
    fn is_installed(&self, dep: &Dependency) -> Result<bool> {
        modules::get(&dep.name).is_installed(self.pm, dep)
    }
    fn install(&self, dep: &Dependency) -> Result<()> {
        modules::get(&dep.name).install(self.pm, dep)
    }
    fn resolved(&self, dep: &Dependency) -> Result<(Option<String>, Option<String>)> {
        Ok((
            modules::get(&dep.name).resolved_version(self.pm, dep)?,
            None,
        ))
    }
    fn is_running(&self, dep: &Dependency) -> Result<bool> {
        modules::get(&dep.name).is_running(self.pm, dep)
    }
    fn start(&self, dep: &Dependency) -> Result<()> {
        modules::get(&dep.name).start(self.pm, dep, self.project_root)
    }
    fn stop(&self, dep: &Dependency) -> Result<()> {
        modules::get(&dep.name).stop(self.pm, dep)
    }
    fn migrate(&self, dep: &Dependency) -> Result<()> {
        self.pm
            .migrate_service(&modules::get(&dep.name).service_name(dep))
    }
    fn needs_migration(&self, dep: &Dependency) -> bool {
        self.pm
            .needs_service_migration(&modules::get(&dep.name).service_name(dep))
    }
    fn remove(&self, _dep: &Dependency, _volumes: bool) -> Result<bool> {
        Ok(false)
    }
    fn log_source(&self, dep: &Dependency, lines: u32, follow: bool) -> Result<LogSource> {
        modules::get(&dep.name).log_source(self.pm, dep, lines, follow)
    }
    fn wait_for_stopped(&self, dep: &Dependency) -> Result<()> {
        modules::get(&dep.name).wait_for_stopped(self.pm, dep)
    }
}

/// Runs services as per-project containers named `devy-<project>-<service>`, each with a
/// named data volume of the same name.
pub struct DockerRunner<'a> {
    runtime: ContainerRuntime<'a>,
    cli: ContainerCli,
    project_root: PathBuf,
    slug: String,
    lock: Option<LockFile>,
    update: bool,
    /// Canonical name → image reference resolved during this run (the pulled digest), so
    /// containers are created from exactly what devy.lock records.
    resolved_refs: RefCell<HashMap<String, String>>,
    /// This machine's `sh.devy.host` id (`host_id::current`), set in tests.
    host: Option<&'static str>,
}

impl<'a> DockerRunner<'a> {
    /// `lock` supplies pinned image digests; with `update` they are ignored and tags are
    /// pulled again.
    pub fn new(
        runtime: ContainerRuntime<'a>,
        config: &DevyConfig,
        project_root: &Path,
        lock: Option<&LockFile>,
        update: bool,
    ) -> Self {
        let name = crate::package_manager::project_name(config);
        Self {
            runtime,
            cli: config.container_cli,
            project_root: project_root.to_path_buf(),
            slug: modules::helpers::project_slug(name, project_root),
            lock: lock.cloned(),
            update,
            resolved_refs: RefCell::new(HashMap::new()),
            host: host_id::current(),
        }
    }

    /// Acts as the machine with host id `host` (`None`: one devy can't identify).
    #[cfg(test)]
    pub(crate) fn set_host(&mut self, host: Option<&'static str>) {
        self.host = host;
    }

    pub fn runtime(&self) -> &ContainerRuntime<'a> {
        &self.runtime
    }

    /// `devy-<project>-<canonical-name>`: the container's and its volume's name.
    pub fn container_name(&self, dep: &Dependency) -> String {
        format!("devy-{}-{}", self.slug, modules::canonical_name(&dep.name))
    }

    /// The `sh.devy.project` label value marking this project's containers. A root that
    /// isn't valid UTF-8 can't be recorded faithfully, and a lossy label would let
    /// `devy prune` mistake a live project's container for a removed checkout's, so no
    /// container is created for it.
    fn project_label(&self) -> Result<&str> {
        self.project_root.to_str().ok_or_else(|| {
            anyhow::anyhow!(
                "devy cannot run containers for a project whose path is not valid UTF-8: {}",
                self.project_root.display()
            )
        })
    }

    /// Who else a container belongs to, or `None` when it is this project's on this
    /// machine. See `foreign_owner_for`.
    fn foreign_owner(&self, state: &docker::ContainerState) -> Option<Foreign> {
        foreign_owner_for(&self.project_root, self.host, state)
    }

    fn is_foreign(&self, state: &docker::ContainerState) -> bool {
        self.foreign_owner(state).is_some()
    }

    /// The error refusing to use container `name`, which belongs to `owner`: for another
    /// machine's, with how to recover when it is really this user's from before the id
    /// changed (a rebuilt devcontainer, a WSL session without `WSL_DISTRO_NAME`).
    fn foreign_error(&self, name: &str, owner: Foreign) -> anyhow::Error {
        let cli = self.cli.binary();
        let hint = format!(
            "; if it's yours (from before a devcontainer rebuild, or from another session), \
             check its project path and creation time with `{cli} inspect {name}` first, \
             then remove it with `{cli} rm -f {name}` and run devy again: the new container \
             reuses its data volume, {name}, so only do this when that data is yours"
        );
        match owner {
            Foreign::Project => anyhow::anyhow!("container {name} belongs to another project"),
            Foreign::Host => {
                anyhow::anyhow!("container {name} belongs to {}{hint}", owner.describe())
            }
            Foreign::Unidentified => anyhow::anyhow!(
                "container {name} has a {HOST_LABEL} label, but devy can't identify this \
                 machine (under WSL, run devy from a wsl.exe session){hint}"
            ),
        }
    }

    /// Refuses to touch container `name` when it exists but another project (or anyone
    /// else) created it. Stopping or removing it, or its same-named volume, would act on
    /// someone else's data.
    fn ensure_not_foreign(&self, name: &str) -> Result<()> {
        self.inspect_not_foreign_within(name, None).map(drop)
    }

    /// [`Self::ensure_not_foreign`], returning the inspected state (`None` when there is
    /// no such container) so the caller can act on that exact container by ID, and
    /// failing when the CLI hasn't answered within `timeout` (when one is given).
    fn inspect_not_foreign_within(
        &self,
        name: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<Option<docker::ContainerState>> {
        let state = self.runtime.inspect_container_within(name, timeout)?;
        if let Some(owner) = state.as_ref().and_then(|s| self.foreign_owner(s)) {
            return Err(self.foreign_error(name, owner));
        }
        Ok(state)
    }

    fn spec(&self, dep: &Dependency) -> Result<(DockerSpec, ImageRef)> {
        let spec = modules::get(&dep.name)
            .docker_spec(dep)?
            .ok_or_else(|| anyhow::anyhow!("{}: no container image is defined", dep.name))?;
        let image = modules::docker_image(&spec, dep);
        Ok((spec, image))
    }

    /// The image reference to pull and run: the digest resolved earlier in this run, else
    /// the module's built-in digest pin for its default image, else the locked digest
    /// while the repository and tag are unchanged (and not `--update`), else
    /// `<repository>:<tag>`.
    pub fn reference(&self, dep: &Dependency) -> Result<String> {
        let canonical = modules::canonical_name(&dep.name);
        if let Some(r) = self.resolved_refs.borrow().get(canonical) {
            return Ok(r.clone());
        }
        let (_, image) = self.spec(dep)?;
        // A built-in pin outranks the lock: the lock can't swap in another digest for it.
        if image.digest.is_some() {
            return Ok(image.reference());
        }
        if !self.update
            && let Some(locked) = self.lock.as_ref().and_then(|l| l.get(canonical))
            && locked.source == DOCKER_SOURCE
            && locked.resolved_version.as_deref() == Some(image.tag.as_str())
            && let Some(digest) = locked.image_digest.as_deref()
            && digest
                .split_once('@')
                .is_some_and(|(repo, _)| repo == image.repository)
        {
            return Ok(digest.to_string());
        }
        Ok(image.reference())
    }

    /// Everything the container is created with, including the `sh.devy.config` hash.
    pub fn run_spec(&self, dep: &Dependency) -> Result<RunSpec> {
        let module = modules::get(&dep.name);
        let (spec, _) = self.spec(dep)?;
        let host_port = match module.port_key() {
            Some(key) => modules::helpers::extra_port(
                dep,
                key,
                module.default_port().unwrap_or(spec.container_port),
            )?,
            None => spec.container_port,
        };
        let name = self.container_name(dep);
        let mut ports = vec![(host_port, spec.container_port)];
        ports.extend(spec.extra_ports.iter().copied());
        let mut run = RunSpec {
            name: name.clone(),
            hostname: modules::canonical_name(&dep.name).to_string(),
            labels: vec![
                (PROJECT_LABEL.into(), self.project_label()?.to_string()),
                (
                    SERVICE_LABEL.into(),
                    modules::canonical_name(&dep.name).into(),
                ),
            ],
            ports,
            volume: spec.data_path.map(|path| (name, path)),
            env: spec.env,
            secret_env: spec.secret_env,
            image: self.reference(dep)?,
            args: spec.args,
        };
        // Without an id the label is left off, so `devy prune` never removes the
        // container (see `host_id`).
        if let Some(host) = self.host {
            run.labels.push((HOST_LABEL.into(), host.into()));
        }
        run.labels
            .push((CONFIG_LABEL.into(), format!("{:016x}", config_hash(&run))));
        Ok(run)
    }
}

/// Whom a container that isn't this project's on this machine belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Foreign {
    /// It lacks this project's `sh.devy.project` label.
    Project,
    /// Its `sh.devy.host` label is another machine's or user's.
    Host,
    /// It has a `sh.devy.host` label, but this machine has no id to match it against.
    Unidentified,
}

impl Foreign {
    /// "another project", "another machine or user …", for "container X belongs to …".
    fn describe(self) -> String {
        match self {
            Foreign::Project => "another project".into(),
            Foreign::Host => format!(
                "another machine or user sharing this container daemon (its {HOST_LABEL} \
                 label isn't this machine's)"
            ),
            Foreign::Unidentified => format!(
                "another machine or user, as far as devy can tell (it has a {HOST_LABEL} \
                 label, but devy can't identify this machine)"
            ),
        }
    }
}

/// Whom a container with `state` belongs to when it isn't the project at
/// `project_root` on the machine with host id `host`: `None` when it is that project's.
/// - `Project` when it lacks `sh.devy.project` = `project_root` (always, for a root that
///   can't be a label; see `project_label`).
/// - `Host` when its `sh.devy.host` label is present but isn't `host`, or `Unidentified`
///   when this machine has no id. Two machines sharing a daemon with the same project
///   path would otherwise act on each other's containers.
///
/// A container without the label (from an older devy, or created while the machine had
/// no id) stays this project's, on every machine sharing the daemon.
fn foreign_owner_for(
    project_root: &Path,
    host: Option<&str>,
    state: &docker::ContainerState,
) -> Option<Foreign> {
    let ours = project_root
        .to_str()
        .is_some_and(|root| state.labels.get(PROJECT_LABEL).map(String::as_str) == Some(root));
    if !ours {
        return Some(Foreign::Project);
    }
    match (state.labels.get(HOST_LABEL), host) {
        (None, _) => None,
        (Some(_), None) => Some(Foreign::Unidentified),
        (Some(label), Some(host)) if label != host => Some(Foreign::Host),
        _ => None,
    }
}

/// A stable hash of what a container is created from: image reference, ports, volume,
/// environment (sorted, with credentials hashed like any other variable, since the
/// container's config records them either way) and arguments.
fn config_hash(run: &RunSpec) -> u64 {
    let mut lines = vec![format!("image {}", run.image)];
    lines.extend(run.ports.iter().map(|(h, c)| format!("port {h}:{c}")));
    lines.extend(run.volume.iter().map(|(v, p)| format!("volume {v}:{p}")));
    let mut env: Vec<String> = run
        .env
        .iter()
        .chain(&run.secret_env)
        .map(|(k, v)| format!("env {k}={v}"))
        .collect();
    env.sort();
    lines.extend(env);
    lines.extend(run.args.iter().map(|a| format!("arg {a}")));
    modules::fnv1a(&lines.join("\n"))
}

impl ServiceRunner for DockerRunner<'_> {
    fn label(&self) -> Option<&'static str> {
        Some("docker")
    }

    fn is_installed(&self, dep: &Dependency) -> Result<bool> {
        // `--update` re-resolves the tag, so it always pulls.
        if self.update {
            return Ok(false);
        }
        self.runtime.image_present(&self.reference(dep)?)
    }

    fn install(&self, dep: &Dependency) -> Result<()> {
        self.runtime.pull(&self.reference(dep)?)
    }

    fn resolved(&self, dep: &Dependency) -> Result<(Option<String>, Option<String>)> {
        let (_, image) = self.spec(dep)?;
        let reference = self.reference(dep)?;
        let digest = if reference.contains('@') {
            Some(reference)
        } else {
            self.runtime.image_digest(&reference, &image.repository)?
        };
        if let Some(d) = &digest {
            self.resolved_refs
                .borrow_mut()
                .insert(modules::canonical_name(&dep.name).to_string(), d.clone());
        }
        Ok((Some(image.tag), digest))
    }

    fn is_running(&self, dep: &Dependency) -> Result<bool> {
        Ok(self
            .runtime
            .inspect_container(&self.container_name(dep))?
            // Another project's container under our name isn't this service running;
            // `start` refuses it, and `down` skips it instead of failing.
            .is_some_and(|s| s.running && !self.is_foreign(&s)))
    }

    fn start(&self, dep: &Dependency) -> Result<()> {
        let run = self.run_spec(dep)?;
        if self.cli == ContainerCli::Podman
            && let Some((port, _)) = run.ports.iter().find(|(h, _)| *h < 1024)
        {
            output::warn(&format!(
                "{}: rootless podman cannot publish port {port} — set a port >= 1024 in devy.yml if this fails",
                dep.name
            ));
        }
        let hash = run
            .labels
            .iter()
            .find(|(k, _)| k == CONFIG_LABEL)
            .map(|(_, v)| v.as_str());
        let existing = self.runtime.inspect_container(&run.name)?;
        // Never start, reuse or replace a container another project (or anyone else)
        // created under this name.
        if let Some(owner) = existing.as_ref().and_then(|s| self.foreign_owner(s)) {
            return Err(self.foreign_error(&run.name, owner));
        }
        match existing {
            Some(state) if state.labels.get(CONFIG_LABEL).map(String::as_str) == hash => {
                if state.running {
                    Ok(())
                } else {
                    self.runtime.start(&run.name)
                }
            }
            Some(_) => {
                // Configuration changed: recreate the container. The volume is kept.
                self.runtime.remove_container(&run.name)?;
                self.runtime.run(&run)
            }
            None => self.runtime.run(&run),
        }
    }

    fn stop(&self, dep: &Dependency) -> Result<()> {
        let name = self.container_name(dep);
        self.ensure_not_foreign(&name)?;
        self.runtime.stop(&name)
    }

    fn remove(&self, dep: &Dependency, volumes: bool) -> Result<bool> {
        let name = self.container_name(dep);
        // Leave another project's container (and its volume) alone, but let `down`
        // carry on with the remaining services.
        if let Some(state) = self.runtime.inspect_container(&name)?
            && let Some(owner) = self.foreign_owner(&state)
        {
            output::warn(&format!(
                "{}: container {name} belongs to {} — not removing it",
                dep.name,
                owner.describe()
            ));
            return Ok(false);
        }
        self.runtime.remove_container(&name)?;
        if volumes {
            self.runtime.remove_volume(&name)?;
        }
        Ok(true)
    }

    fn log_source(&self, dep: &Dependency, lines: u32, follow: bool) -> Result<LogSource> {
        self.container_log_source(dep, lines, follow, None)
    }

    fn log_source_within(
        &self,
        dep: &Dependency,
        lines: u32,
        timeout: std::time::Duration,
    ) -> Result<LogSource> {
        self.container_log_source(dep, lines, false, Some(timeout))
    }
}

impl DockerRunner<'_> {
    /// `<cli> logs` for the service's container, by ID, once it is known not to be
    /// another owner's; the owner check fails after `timeout`, when one is given. With
    /// no container there is nothing to read: an empty file list, which reads as "no
    /// logs yet".
    fn container_log_source(
        &self,
        dep: &Dependency,
        lines: u32,
        follow: bool,
        timeout: Option<std::time::Duration>,
    ) -> Result<LogSource> {
        let name = self.container_name(dep);
        // Another owner's output must not be shown (or sent to `--explain`) as this service's.
        let Some(state) = self.inspect_not_foreign_within(&name, timeout)? else {
            // No container, so nothing has logged yet. The CLI isn't run by name: a
            // container someone else creates under it after the check would be read.
            return Ok(LogSource::Files(vec![]));
        };
        // Read the container that was checked, by ID, so one swapped in under the name
        // after the check isn't read instead.
        if !prune::is_container_id(&state.id) {
            anyhow::bail!(
                "`{}` did not report an ID for container {name}",
                self.runtime.cli_name()
            );
        }
        let mut args: Vec<String> = vec!["logs".into(), "--tail".into(), lines.to_string()];
        if follow {
            args.push("-f".into());
        }
        args.push(state.id);
        Ok(LogSource::Command(LogCommand {
            program: self.runtime.cli_name().into(),
            args,
            kind: LogCommandKind::Container,
        }))
    }
}

/// One runner per backend, and the choice between them for each dependency.
pub struct Runners<'a> {
    pub package: PackageRunner<'a>,
    pub docker: DockerRunner<'a>,
}

impl<'a> Runners<'a> {
    pub fn new(
        pm: &'a dyn PackageManager,
        runtime: ContainerRuntime<'a>,
        config: &DevyConfig,
        project_root: &'a Path,
        lock: Option<&LockFile>,
        update: bool,
    ) -> Self {
        Self {
            package: PackageRunner::new(pm, project_root),
            docker: DockerRunner::new(runtime, config, project_root, lock, update),
        }
    }

    /// The runner for `dep`: docker for docker-managed services (see
    /// `DevyConfig::normalized_dependencies`), the package manager for everything else.
    pub fn runner_for(&self, dep: &Dependency) -> &dyn ServiceRunner {
        if dep.docker {
            &self.docker
        } else {
            &self.package
        }
    }

    /// Checks the container runtime when any of `deps` is docker-managed.
    pub fn ensure_docker_available<'d>(
        &self,
        deps: impl IntoIterator<Item = &'d Dependency>,
    ) -> Result<()> {
        if deps.into_iter().any(|d| d.docker) {
            self.docker.runtime.ensure_available()?;
        }
        Ok(())
    }
}

/// Warnings about running `dep` as a container; empty unless it is docker-managed.
pub fn docker_warnings(dep: &Dependency) -> Vec<String> {
    if !dep.docker {
        return vec![];
    }
    modules::docker_image_warning(dep)
        .into_iter()
        .chain(modules::get(&dep.name).docker_warnings(dep))
        .collect()
}

/// `<name>` or `<name> (docker)`.
pub fn display_name(name: &str, runner: &dyn ServiceRunner) -> String {
    match runner.label() {
        Some(label) => format!("{name} ({label})"),
        None => name.to_string(),
    }
}

/// A container CLI that fails the test if it is ever invoked.
#[cfg(test)]
struct NoContainers;

#[cfg(test)]
impl docker::CommandRunner for NoContainers {
    fn output(&self, program: &str, args: &[String]) -> Result<docker::CmdOutput> {
        panic!("unexpected container CLI call: {program} {args:?}")
    }
    fn status(&self, program: &str, args: &[String]) -> Result<bool> {
        panic!("unexpected container CLI call: {program} {args:?}")
    }
}

/// Runners for tests whose dependencies all use the package manager.
#[cfg(test)]
pub(crate) fn package_runners<'a>(
    pm: &'a dyn PackageManager,
    project_root: &'a Path,
) -> Runners<'a> {
    static NO_CONTAINERS: NoContainers = NoContainers;
    Runners::new(
        pm,
        ContainerRuntime::new(ContainerCli::Docker, &NO_CONTAINERS),
        &crate::test_support::make_config(&[], HashMap::new()),
        project_root,
        None,
        false,
    )
}

#[cfg(test)]
mod tests;
