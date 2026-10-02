//! What runs a dependency's service: the package manager (`PackageRunner`) or a
//! per-project container (`DockerRunner`). Commands pick one per dependency with
//! `Runners::runner_for`, so docker-managed services coexist with packages installed by
//! brew, apt or nix in the same project.

pub(crate) mod docker;

use anyhow::Result;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::{ContainerCli, Dependency, DevyConfig};
use crate::lock::LockFile;
use crate::modules::{self, DockerSpec, ImageRef};
use crate::output;
use crate::package_manager::PackageManager;
use docker::{ContainerRuntime, RunSpec};

/// Container label holding the project root.
pub const PROJECT_LABEL: &str = "sh.devy.project";
/// Container label holding the dependency's canonical name.
pub const SERVICE_LABEL: &str = "sh.devy.service";
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
    /// Removes the service's container and, with `volumes`, its data volume. Returns
    /// whether there was anything this runner could remove (only containers can be).
    fn remove(&self, dep: &Dependency, volumes: bool) -> Result<bool>;

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
            "{} did not stop after {max} attempts — try stopping it manually or check its logs",
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
    fn remove(&self, _dep: &Dependency, _volumes: bool) -> Result<bool> {
        Ok(false)
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
        let name = config.name.as_deref().unwrap_or("project");
        Self {
            runtime,
            cli: config.container_cli,
            project_root: project_root.to_path_buf(),
            slug: modules::helpers::project_slug(name, project_root),
            lock: lock.cloned(),
            update,
            resolved_refs: RefCell::new(HashMap::new()),
        }
    }

    pub fn runtime(&self) -> &ContainerRuntime<'a> {
        &self.runtime
    }

    /// `devy-<project>-<canonical-name>`: the container's and its volume's name.
    pub fn container_name(&self, dep: &Dependency) -> String {
        format!("devy-{}-{}", self.slug, modules::canonical_name(&dep.name))
    }

    fn spec(&self, dep: &Dependency) -> Result<(DockerSpec, ImageRef)> {
        let spec = modules::get(&dep.name)
            .docker_spec(dep)?
            .ok_or_else(|| anyhow::anyhow!("{}: no container image is defined", dep.name))?;
        let image = modules::docker_image(&spec, dep);
        Ok((spec, image))
    }

    /// The image reference to pull and run: the digest resolved earlier in this run, else
    /// the locked digest while the repository and tag are unchanged (and not `--update`),
    /// else `<repository>:<tag>`.
    pub fn reference(&self, dep: &Dependency) -> Result<String> {
        let canonical = modules::canonical_name(&dep.name);
        if let Some(r) = self.resolved_refs.borrow().get(canonical) {
            return Ok(r.clone());
        }
        let (_, image) = self.spec(dep)?;
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
                (
                    PROJECT_LABEL.into(),
                    self.project_root.to_string_lossy().into_owned(),
                ),
                (
                    SERVICE_LABEL.into(),
                    modules::canonical_name(&dep.name).into(),
                ),
            ],
            ports,
            volume: spec.data_path.map(|path| (name, path)),
            env: spec.env,
            image: self.reference(dep)?,
            args: spec.args,
        };
        run.labels
            .push((CONFIG_LABEL.into(), format!("{:016x}", config_hash(&run))));
        Ok(run)
    }
}

/// A stable hash of what a container is created from: image reference, ports, volume,
/// environment (sorted) and arguments.
fn config_hash(run: &RunSpec) -> u64 {
    let mut lines = vec![format!("image {}", run.image)];
    lines.extend(run.ports.iter().map(|(h, c)| format!("port {h}:{c}")));
    lines.extend(run.volume.iter().map(|(v, p)| format!("volume {v}:{p}")));
    let mut env: Vec<String> = run
        .env
        .iter()
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
            .is_some_and(|s| s.running))
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
        match self.runtime.inspect_container(&run.name)? {
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
        self.runtime.stop(&self.container_name(dep))
    }

    fn remove(&self, dep: &Dependency, volumes: bool) -> Result<bool> {
        let name = self.container_name(dep);
        self.runtime.remove_container(&name)?;
        if volumes {
            self.runtime.remove_volume(&name)?;
        }
        Ok(true)
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
