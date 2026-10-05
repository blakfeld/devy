//! Service port resolution shared by `up`, `check`, `status`, `start`, `stop` and `restart`.
//!
//! Precedence: explicit port in devy.yml → the recorded port (when the backend can apply
//! a port) → a fresh OS-assigned port (`up` only, when the backend can apply a port) →
//! the module's default port. The recorded port comes from `devy.lock`'s `assigned_port`,
//! or, in a linked git worktree, from `.devy/worktree.yml` (see [`RecordedPorts`]).

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::{Dependency, ExtraValue};
use crate::lock::LockFile;
use crate::modules;
use crate::package_manager::PackageManager;
use crate::worktree::{LinkedWorktree, WorktreePorts};

/// Whether port resolution may pick new random ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortMode {
    /// `devy up`: unassigned ports get a free port from the OS.
    Assign,
    /// Every other command: never assigns, never writes devy.lock or `.devy/worktree.yml`.
    ReadOnly,
}

/// Where a service's effective port came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedPort {
    Explicit(u16),
    /// Recorded by an earlier `devy up`: in devy.lock, or in `.devy/worktree.yml` in a
    /// linked worktree.
    Locked(u16),
    Assigned(u16),
    Default(u16),
    /// The backend can apply a port, but none is recorded yet and the mode is read-only.
    Unassigned,
}

impl ResolvedPort {
    pub(crate) fn port(self) -> Option<u16> {
        match self {
            Self::Explicit(p) | Self::Locked(p) | Self::Assigned(p) | Self::Default(p) => Some(p),
            Self::Unassigned => None,
        }
    }

    /// Where the port came from, as reported by `--json` output. A freshly assigned
    /// port is reported as `lock`, since `up` records it there. A port recorded in
    /// `.devy/worktree.yml` is also `lock`: the JSON schema has one value for "recorded
    /// by `devy up`".
    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::Explicit(_) => "explicit",
            Self::Locked(_) | Self::Assigned(_) => "lock",
            Self::Default(_) => "default",
            Self::Unassigned => "unassigned",
        }
    }
}

/// Where recorded ports are read from (design D5). Obtain it from [`RecordedPorts`], so
/// every command makes the same lock-or-worktree choice.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PortSource<'a> {
    /// `assigned_port` in devy.lock (`None` when there is no lock yet).
    Lock(Option<&'a LockFile>),
    /// `.devy/worktree.yml`, in a linked git worktree. devy.lock's ports are never used
    /// for this checkout; the locks only list the ports a fresh assignment must avoid.
    Worktree {
        ports: &'a WorktreePorts,
        /// This worktree's own devy.lock (usually the main checkout's, as committed).
        lock: Option<&'a LockFile>,
        /// The main checkout's devy.lock, where the main checkout's services' ports are
        /// recorded, when it can be read.
        main_lock: Option<&'a LockFile>,
    },
}

impl PortSource<'_> {
    /// The port recorded for canonical dependency `canonical`.
    fn recorded(self, canonical: &str) -> Result<Option<u16>> {
        match self {
            Self::Lock(lock) => {
                let Some(p) = lock
                    .and_then(|l| l.get(canonical))
                    .and_then(|d| d.assigned_port)
                else {
                    return Ok(None);
                };
                // devy only assigns unprivileged ports and records an explicit port
                // verbatim, so a privileged locked port used here (no explicit port
                // overrides it) came from an edited lock.
                if p < 1024 {
                    return Err(crate::validate::invalid(
                        canonical,
                        "assigned_port",
                        &p.to_string(),
                    ))
                    .context("Failed to parse devy.lock")
                    .context(format!(
                        "ports below 1024 are only allowed when set with `port:` in devy.yml; \
                         remove the {canonical} entry from devy.lock and run `devy up` to assign a new port"
                    ));
                }
                Ok(Some(p))
            }
            // The file is devy's own and never committed, so a privileged port can only be
            // a since-removed explicit `port:`. It counts as unrecorded: `devy up` assigns
            // a fresh port in its place.
            Self::Worktree { ports, .. } => Ok(ports.get(canonical).filter(|p| *p >= 1024)),
        }
    }

    /// Ports a freshly assigned port must not take: in a linked worktree, every
    /// `assigned_port` in the main checkout's devy.lock, since the main checkout's
    /// services listen there, and in this worktree's own devy.lock (a committed or
    /// not-yet-pulled copy of it).
    fn reserved(self) -> HashSet<u16> {
        match self {
            Self::Lock(_) => HashSet::new(),
            Self::Worktree {
                lock, main_lock, ..
            } => lock
                .into_iter()
                .chain(main_lock)
                .flat_map(|l| l.dependencies.values())
                .filter_map(|d| d.assigned_port)
                .collect(),
        }
    }

    /// The error `devy start` and `devy restart` raise for a service whose port `devy up`
    /// hasn't recorded yet.
    pub(crate) fn unassigned_error(self, name: &str) -> anyhow::Error {
        match self {
            Self::Lock(_) => {
                anyhow::anyhow!("'{name}' has no port in devy.lock yet — run `devy up` first")
            }
            Self::Worktree { .. } => {
                anyhow::anyhow!("'{name}' has no port in this worktree yet — run `devy up` first")
            }
        }
    }
}

/// devy.lock and, in a linked git worktree, `.devy/worktree.yml`: everything port
/// resolution may read. Outside a linked worktree `.devy/worktree.yml` is never read.
pub(crate) struct RecordedPorts {
    lock: Option<LockFile>,
    /// `Some` exactly when the project is in a linked worktree: the worktree and its
    /// `.devy/worktree.yml`.
    worktree: Option<(LinkedWorktree, WorktreePorts)>,
    /// In a linked worktree, the main checkout's devy.lock, when it can be read.
    main_lock: Option<LockFile>,
}

impl RecordedPorts {
    /// Loads devy.lock next to devy.yml and, in a linked worktree, `.devy/worktree.yml`.
    pub(crate) fn load(project_root: &Path) -> Result<Self> {
        Ok(Self::with_lock(project_root, load_lock(project_root)?))
    }

    /// Like [`RecordedPorts::load`], with devy.lock already loaded by the caller. This is
    /// where worktree detection chooses the port source for every command.
    pub(crate) fn with_lock(project_root: &Path, lock: Option<LockFile>) -> Self {
        let worktree = crate::worktree::detect(project_root)
            .map(|linked| (linked, WorktreePorts::load(project_root)));
        // Best effort: it only lists ports to avoid, so a missing or unreadable lock
        // reserves nothing rather than failing the command.
        let main_lock = worktree
            .as_ref()
            .and_then(|(linked, _)| linked.main_project_root.as_deref())
            .and_then(|main| load_lock(main).ok().flatten());
        Self {
            lock,
            worktree,
            main_lock,
        }
    }

    /// devy.lock, for versions and image digests (and ports outside a worktree).
    pub(crate) fn lock(&self) -> Option<&LockFile> {
        self.lock.as_ref()
    }

    /// The linked git worktree the project is in, if any.
    pub(crate) fn worktree(&self) -> Option<&LinkedWorktree> {
        self.worktree.as_ref().map(|(linked, _)| linked)
    }

    /// Whether the project is in a linked git worktree.
    pub(crate) fn in_worktree(&self) -> bool {
        self.worktree().is_some()
    }

    /// The source recorded ports are read from.
    pub(crate) fn source(&self) -> PortSource<'_> {
        match &self.worktree {
            Some((_, ports)) => PortSource::Worktree {
                ports,
                lock: self.lock.as_ref(),
                main_lock: self.main_lock.as_ref(),
            },
            None => PortSource::Lock(self.lock.as_ref()),
        }
    }
}

/// Resolves the port of every service dep and writes it into `dep.extra[port_key]` so
/// health checks, config writers and env vars all see the same value.
///
/// Returns one entry per dep, `None` for deps without a configurable port.
pub(crate) fn resolve_ports(
    deps: &mut [Dependency],
    source: PortSource<'_>,
    pm: &dyn PackageManager,
    mode: PortMode,
) -> Result<Vec<Option<ResolvedPort>>> {
    let mut resolved = Vec::with_capacity(deps.len());
    for dep in deps.iter_mut() {
        let module = modules::get(&dep.name);
        let Some(key) = module.port_key() else {
            resolved.push(None);
            continue;
        };
        let r = resolve_one(dep, key, source, pm, mode)?;
        if let Some(p) = r.port() {
            dep.extra
                .insert(key.to_string(), ExtraValue::Number(p.into()));
        }
        resolved.push(Some(r));
    }
    Ok(resolved)
}

fn resolve_one(
    dep: &Dependency,
    key: &str,
    source: PortSource<'_>,
    pm: &dyn PackageManager,
    mode: PortMode,
) -> Result<ResolvedPort> {
    let module = modules::get(&dep.name);
    if let Some(raw) = dep.extra.get(key).and_then(|v| v.as_u64()) {
        return match u16::try_from(raw) {
            Ok(0) | Err(_) => anyhow::bail!(
                "'{}': port value {} is out of range (must be 1–65535)",
                dep.name,
                raw
            ),
            Ok(p) => Ok(ResolvedPort::Explicit(p)),
        };
    }
    if port_applicable(dep, pm) {
        if let Some(p) = source.recorded(modules::canonical_name(&dep.name))? {
            return Ok(ResolvedPort::Locked(p));
        }
        return match mode {
            PortMode::Assign => {
                let p = assign_port(&source.reserved(), modules::helpers::find_available_port)
                    .with_context(|| format!("Failed to find available port for {}", dep.name))?;
                Ok(ResolvedPort::Assigned(p))
            }
            PortMode::ReadOnly => Ok(ResolvedPort::Unassigned),
        };
    }
    match module.default_port() {
        Some(p) => Ok(ResolvedPort::Default(p)),
        None => Ok(ResolvedPort::Unassigned),
    }
}

/// How many free ports [`assign_port`] tries before giving up.
const ASSIGN_ATTEMPTS: usize = 32;

/// A free port from `find` that isn't in `reserved`. The OS can hand out a reserved
/// port while its owner (the main checkout's service) isn't running, so `find` is
/// retried, at most [`ASSIGN_ATTEMPTS`] times.
fn assign_port(reserved: &HashSet<u16>, mut find: impl FnMut() -> Result<u16>) -> Result<u16> {
    for _ in 0..ASSIGN_ATTEMPTS {
        let p = find()?;
        if !reserved.contains(&p) {
            return Ok(p);
        }
    }
    anyhow::bail!(
        "every port the OS offered in {ASSIGN_ATTEMPTS} attempts is already recorded in devy.lock"
    )
}

/// Whether `dep`'s service can be made to listen on a port devy chooses. Always true for
/// docker-managed services, which publish any host port to a fixed container port.
pub(crate) fn port_applicable(dep: &Dependency, pm: &dyn PackageManager) -> bool {
    dep.docker || modules::get(&dep.name).port_applicable(pm)
}

/// Fails if two service deps resolve to the same effective port. Unassigned ports are
/// skipped: `up` would give them distinct random ports.
pub(crate) fn check_port_conflicts(
    deps: &[Dependency],
    resolved: &[Option<ResolvedPort>],
) -> Result<()> {
    let mut seen: HashMap<u16, &str> = HashMap::new();
    for (dep, r) in deps.iter().zip(resolved) {
        if !modules::get(&dep.name).is_service() {
            continue;
        }
        let Some(port) = r.and_then(ResolvedPort::port) else {
            continue;
        };
        if let Some(other) = seen.insert(port, &dep.name) {
            anyhow::bail!(
                "port conflict: '{}' and '{}' both use port {}",
                other,
                dep.name,
                port
            );
        }
    }
    Ok(())
}

/// Resolves ports and checks for conflicts in one step.
pub(crate) fn resolve_and_check(
    deps: &mut [Dependency],
    source: PortSource<'_>,
    pm: &dyn PackageManager,
    mode: PortMode,
) -> Result<Vec<Option<ResolvedPort>>> {
    let resolved = resolve_ports(deps, source, pm, mode)?;
    check_port_conflicts(deps, &resolved)?;
    Ok(resolved)
}

/// Warning for an explicit, non-default port that the backend cannot make the service
/// listen on. `None` when the port is applicable, default, or not explicit.
pub(crate) fn unapplied_port_warning(dep: &Dependency, pm: &dyn PackageManager) -> Option<String> {
    let module = modules::get(&dep.name);
    let key = module.port_key()?;
    let raw = dep.extra.get(key)?.as_u64()?;
    let port = u16::try_from(raw).ok()?;
    if Some(port) == module.default_port() || port_applicable(dep, pm) {
        return None;
    }
    Some(format!(
        "devy cannot make {} listen on port {port} with {} — configure the service to listen on {port} manually",
        dep.name,
        pm.name()
    ))
}

/// The port `devy up` records for `dep`: its resolved port when the module has a port
/// key and the backend can apply it. Others always use the default and record nothing.
pub(crate) fn recordable_port(dep: &Dependency, pm: &dyn PackageManager) -> Option<u16> {
    modules::get(&dep.name)
        .port_key()
        .filter(|_| port_applicable(dep, pm))
        .and_then(|key| dep.extra.get(key))
        .and_then(|v| v.as_u64())
        .and_then(|raw| u16::try_from(raw).ok())
}

/// The `.devy/worktree.yml` content for `deps` after `devy up` resolved their ports
/// (`resolved` holds one entry per dep, from [`resolve_ports`]). Ports set explicitly in
/// devy.yml are left out: they win anyway, and recording one would keep a worktree on
/// the main checkout's port after the user removes `port:` to isolate it.
pub(crate) fn worktree_ports_for(
    deps: &[Dependency],
    resolved: &[Option<ResolvedPort>],
    pm: &dyn PackageManager,
) -> WorktreePorts {
    WorktreePorts {
        ports: deps
            .iter()
            .zip(resolved)
            .filter(|(_, r)| !matches!(r, Some(ResolvedPort::Explicit(_))))
            .filter_map(|(dep, _)| {
                recordable_port(dep, pm)
                    .map(|p| (modules::canonical_name(&dep.name).to_string(), p))
            })
            .collect(),
        ..Default::default()
    }
}

/// Warning for a service whose port is fixed in devy.yml and applied by the backend: in
/// a linked worktree it collides with the same service in the main checkout. `None`
/// otherwise (no explicit port, or one the backend can't apply, such as brew's).
pub(crate) fn shared_fixed_port_warning(
    dep: &Dependency,
    pm: &dyn PackageManager,
) -> Option<String> {
    let module = modules::get(&dep.name);
    if !module.is_service() || !port_applicable(dep, pm) {
        return None;
    }
    let raw = dep.extra.get(module.port_key()?)?.as_u64()?;
    let port = u16::try_from(raw).ok().filter(|p| *p != 0)?;
    Some(format!(
        "'{}' has a fixed port {port} in devy.yml, so it can't run in this worktree and the main checkout at the same time",
        dep.name
    ))
}

/// Loads devy.lock next to devy.yml for read-only port resolution.
pub(crate) fn load_lock(project_root: &Path) -> Result<Option<LockFile>> {
    LockFile::load(&project_root.join(crate::lock::PATH)).context("Failed to read devy.lock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockedDep;
    use crate::package_manager::MockPackageManager;
    use std::collections::BTreeMap;

    #[cfg(unix)]
    #[test]
    fn load_lock_refuses_a_symlink_without_echoing_its_target() {
        let dir = crate::test_support::tmp_dir();
        let outside = crate::test_support::tmp_dir();
        std::fs::write(outside.join("creds"), "https://user:SENTINEL@example.com\n").unwrap();
        std::os::unix::fs::symlink(outside.join("creds"), dir.join(crate::lock::PATH)).unwrap();
        let err = format!("{:#}", load_lock(&dir).unwrap_err());
        assert!(err.contains("not a regular file"), "{err}");
        assert!(!err.contains("SENTINEL"), "{err}");
        std::fs::remove_file(dir.join(crate::lock::PATH)).unwrap();
        assert!(load_lock(&dir).unwrap().is_none());
    }

    fn dep_with_port(name: &str, port: u64) -> Dependency {
        let mut extra = HashMap::new();
        extra.insert("port".into(), ExtraValue::Number(port.into()));
        Dependency::with_extra(name, extra)
    }

    fn pm(name: &'static str) -> MockPackageManager {
        MockPackageManager {
            name,
            ..Default::default()
        }
    }

    fn lock_with(entries: &[(&str, u16)]) -> LockFile {
        let mut deps = BTreeMap::new();
        for (name, port) in entries {
            deps.insert(
                name.to_string(),
                LockedDep {
                    resolved_version: None,
                    source: "nix".into(),
                    assigned_port: Some(*port),
                    image_digest: None,
                },
            );
        }
        LockFile {
            dependencies: deps,
            ..Default::default()
        }
    }

    fn port_of(dep: &Dependency) -> Option<u64> {
        dep.extra.get("port").and_then(|v| v.as_u64())
    }

    // ── resolve_ports ─────────────────────────────────────────────────────────

    #[test]
    fn privileged_locked_port_is_rejected_when_used() {
        let lock = lock_with(&[("redis", 22)]);
        let mut deps = vec![Dependency::simple("redis")];
        for mode in [PortMode::Assign, PortMode::ReadOnly] {
            let err = resolve_ports(&mut deps, PortSource::Lock(Some(&lock)), &pm("nix"), mode)
                .unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.ends_with("Failed to parse devy.lock: redis: invalid assigned_port \"22\""),
                "{msg}"
            );
            assert!(
                msg.contains("remove the redis entry from devy.lock"),
                "{msg}"
            );
        }
    }

    #[test]
    fn privileged_locked_port_is_rejected_through_alias() {
        let lock = lock_with(&[("postgresql", 543)]);
        let mut deps = vec![Dependency::simple("postgres")];
        assert!(
            resolve_ports(
                &mut deps,
                PortSource::Lock(Some(&lock)),
                &pm("nix"),
                PortMode::ReadOnly
            )
            .is_err()
        );
    }

    #[test]
    fn privileged_locked_port_is_ignored_when_explicit_port_set() {
        // A previous explicit `port: 543` was recorded in the lock, then changed to 5433.
        let lock = lock_with(&[("postgresql", 543)]);
        let mut deps = vec![dep_with_port("postgresql", 5433)];
        let r = resolve_ports(
            &mut deps,
            PortSource::Lock(Some(&lock)),
            &pm("nix"),
            PortMode::Assign,
        )
        .unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Explicit(5433)));
    }

    #[test]
    fn privileged_orphan_lock_entry_is_ignored() {
        let lock = lock_with(&[("redis", 22)]);
        let mut deps = vec![Dependency::simple("mysql")];
        resolve_ports(
            &mut deps,
            PortSource::Lock(Some(&lock)),
            &pm("nix"),
            PortMode::Assign,
        )
        .unwrap();
    }

    #[test]
    fn resolve_service_ports_injects_random_port_when_no_lock() {
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(
            &mut deps,
            PortSource::Lock(None),
            &pm("nix"),
            PortMode::Assign,
        )
        .unwrap();
        assert!(matches!(r[0], Some(ResolvedPort::Assigned(p)) if p > 0));
        assert!(port_of(&deps[0]).is_some(), "port must be injected");
    }

    #[test]
    fn resolve_service_ports_reuses_locked_port() {
        let lock = lock_with(&[("redis", 16379)]);
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(
            &mut deps,
            PortSource::Lock(Some(&lock)),
            &pm("nix"),
            PortMode::Assign,
        )
        .unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Locked(16379)));
        assert_eq!(port_of(&deps[0]), Some(16379));
    }

    #[test]
    fn resolve_service_ports_respects_user_configured_port() {
        let lock = lock_with(&[("redis", 51000)]);
        let mut deps = vec![dep_with_port("redis", 6380)];
        let r = resolve_ports(
            &mut deps,
            PortSource::Lock(Some(&lock)),
            &pm("nix"),
            PortMode::Assign,
        )
        .unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Explicit(6380)));
        assert_eq!(port_of(&deps[0]), Some(6380));
    }

    #[test]
    fn resolve_service_ports_skips_non_service_deps() {
        let mut deps = vec![Dependency::simple("node")];
        let r = resolve_ports(
            &mut deps,
            PortSource::Lock(None),
            &pm("nix"),
            PortMode::Assign,
        )
        .unwrap();
        assert_eq!(r[0], None);
        assert!(port_of(&deps[0]).is_none());
    }

    #[test]
    fn resolve_service_ports_assigns_distinct_ports() {
        let mut deps = vec![Dependency::simple("redis"), Dependency::simple("mysql")];
        resolve_ports(
            &mut deps,
            PortSource::Lock(None),
            &pm("nix"),
            PortMode::Assign,
        )
        .unwrap();
        assert_ne!(port_of(&deps[0]), port_of(&deps[1]));
    }

    #[test]
    fn resolve_uses_default_and_ignores_lock_when_backend_cannot_apply() {
        let lock = lock_with(&[("redis", 51000)]);
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(
            &mut deps,
            PortSource::Lock(Some(&lock)),
            &pm("brew"),
            PortMode::Assign,
        )
        .unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Default(6379)));
        assert_eq!(port_of(&deps[0]), Some(6379));
    }

    #[test]
    fn resolve_read_only_leaves_unlocked_ports_unassigned() {
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(
            &mut deps,
            PortSource::Lock(None),
            &pm("nix"),
            PortMode::ReadOnly,
        )
        .unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Unassigned));
        assert!(
            port_of(&deps[0]).is_none(),
            "read-only must not invent a port"
        );
    }

    #[test]
    fn resolve_read_only_uses_locked_port() {
        let lock = lock_with(&[("redis", 51000)]);
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(
            &mut deps,
            PortSource::Lock(Some(&lock)),
            &pm("nix"),
            PortMode::ReadOnly,
        )
        .unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Locked(51000)));
    }

    #[test]
    fn resolve_reads_lock_by_canonical_name() {
        let lock = lock_with(&[("postgresql", 51000)]);
        let mut deps = vec![Dependency::simple("postgres")];
        let r = resolve_ports(
            &mut deps,
            PortSource::Lock(Some(&lock)),
            &pm("nix"),
            PortMode::ReadOnly,
        )
        .unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Locked(51000)));
    }

    #[test]
    fn resolve_rejects_out_of_range_explicit_port() {
        for bad in [0, 99999] {
            let mut deps = vec![dep_with_port("mysql", bad)];
            let err = resolve_ports(
                &mut deps,
                PortSource::Lock(None),
                &pm("nix"),
                PortMode::Assign,
            )
            .unwrap_err();
            assert!(err.to_string().contains("out of range"), "{err}");
        }
    }

    // ── check_port_conflicts ──────────────────────────────────────────────────

    fn check(
        names_ports: Vec<Dependency>,
        lock: Option<&LockFile>,
        pm_name: &'static str,
    ) -> Result<()> {
        let mut deps = names_ports;
        resolve_and_check(
            &mut deps,
            PortSource::Lock(lock),
            &pm(pm_name),
            PortMode::ReadOnly,
        )
        .map(|_| ())
    }

    #[test]
    fn no_conflict_for_unassigned_mysql_and_mariadb_under_nix() {
        let deps = vec![Dependency::simple("mysql"), Dependency::simple("mariadb")];
        assert!(check(deps, None, "nix").is_ok());
    }

    #[test]
    fn default_port_conflict_where_ports_cannot_be_applied() {
        let deps = vec![
            Dependency::simple("elasticsearch"),
            Dependency::simple("opensearch"),
        ];
        let err = check(deps, None, "brew").unwrap_err();
        assert_eq!(
            err.to_string(),
            "port conflict: 'elasticsearch' and 'opensearch' both use port 9200"
        );
    }

    #[test]
    fn locked_port_collision_conflicts() {
        let lock = lock_with(&[("redis", 51000), ("memcached", 51000)]);
        let deps = vec![Dependency::simple("redis"), Dependency::simple("memcached")];
        let err = check(deps, Some(&lock), "nix").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("'redis'") && msg.contains("'memcached'") && msg.contains("51000"));
    }

    #[test]
    fn explicit_port_conflict() {
        let deps = vec![
            dep_with_port("redis", 7000),
            dep_with_port("memcached", 7000),
        ];
        let err = check(deps, None, "nix").unwrap_err();
        assert!(err.to_string().contains("7000"));
    }

    #[test]
    fn explicit_port_wins_over_default() {
        let deps = vec![dep_with_port("mysql", 3307), Dependency::simple("mariadb")];
        assert!(check(deps, None, "brew").is_ok());
    }

    #[test]
    fn conflicts_ignore_non_service_deps() {
        let deps = vec![dep_with_port("node", 3306), dep_with_port("mysql", 3306)];
        assert!(check(deps, None, "nix").is_ok());
    }

    // ── unapplied_port_warning ────────────────────────────────────────────────

    #[test]
    fn warns_for_explicit_port_brew_cannot_apply() {
        let dep = dep_with_port("redis", 6380);
        let msg = unapplied_port_warning(&dep, &pm("brew")).expect("must warn");
        assert!(msg.contains("redis") && msg.contains("6380") && msg.contains("brew"));
    }

    #[test]
    fn no_warning_when_port_applicable_or_default() {
        assert!(unapplied_port_warning(&dep_with_port("redis", 6380), &pm("nix")).is_none());
        assert!(unapplied_port_warning(&dep_with_port("redis", 6379), &pm("brew")).is_none());
        assert!(unapplied_port_warning(&Dependency::simple("redis"), &pm("brew")).is_none());
    }

    #[test]
    fn warns_for_postgres_on_winget() {
        let dep = dep_with_port("postgresql", 5433);
        let msg = unapplied_port_warning(&dep, &pm("winget")).expect("must warn");
        assert!(msg.contains("postgresql") && msg.contains("winget"));
    }

    // ── worktree port source ──────────────────────────────────────────────────

    fn worktree_ports(entries: &[(&str, u16)]) -> WorktreePorts {
        WorktreePorts {
            ports: entries.iter().map(|(n, p)| (n.to_string(), *p)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn worktree_up_assigns_a_new_port_instead_of_the_locks() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        let recorded = RecordedPorts::with_lock(&feat, Some(lock_with(&[("redis", 51000)])));
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(&mut deps, recorded.source(), &pm("nix"), PortMode::Assign).unwrap();
        assert!(
            matches!(r[0], Some(ResolvedPort::Assigned(p)) if p != 51000),
            "{r:?}"
        );
    }

    #[test]
    fn worktree_source_reserves_every_lock_port() {
        let wt = worktree_ports(&[]);
        let lock = lock_with(&[("redis", 51000), ("memcached", 51001)]);
        let main = lock_with(&[("redis", 53000)]);
        let source = PortSource::Worktree {
            ports: &wt,
            lock: Some(&lock),
            main_lock: Some(&main),
        };
        assert_eq!(source.reserved(), HashSet::from([51000, 51001, 53000]));
        // Outside a worktree the lock's ports are this checkout's own: nothing to avoid.
        assert!(PortSource::Lock(Some(&lock)).reserved().is_empty());
    }

    #[test]
    fn worktree_reserves_the_main_checkouts_lock_ports() {
        let tmp = crate::test_support::tmp_dir();
        let (main, feat) = crate::test_support::fake_linked_worktree(&tmp);
        // The main checkout's `devy up` recorded a port this worktree's lock doesn't have.
        lock_with(&[("redis", 53000)])
            .write(&main.join(crate::lock::PATH))
            .unwrap();
        let recorded = RecordedPorts::with_lock(&feat, Some(lock_with(&[("redis", 51000)])));
        assert_eq!(recorded.source().reserved(), HashSet::from([51000, 53000]));

        // A missing or unreadable main lock reserves nothing extra.
        std::fs::remove_file(main.join(crate::lock::PATH)).unwrap();
        let recorded = RecordedPorts::with_lock(&feat, Some(lock_with(&[("redis", 51000)])));
        assert_eq!(recorded.source().reserved(), HashSet::from([51000]));
        std::fs::write(main.join(crate::lock::PATH), "not: [valid").unwrap();
        let recorded = RecordedPorts::with_lock(&feat, None);
        assert!(recorded.source().reserved().is_empty());
    }

    #[test]
    fn assign_port_retries_past_reserved_ports() {
        let reserved = HashSet::from([51000, 51001]);
        let mut offers = [51000, 51001, 51000, 52000].into_iter();
        let mut calls = 0;
        let p = assign_port(&reserved, || {
            calls += 1;
            Ok(offers.next().unwrap())
        })
        .unwrap();
        assert_eq!(p, 52000);
        assert_eq!(calls, 4);
    }

    #[test]
    fn assign_port_gives_up_after_bounded_attempts() {
        let reserved = HashSet::from([51000]);
        let mut calls = 0;
        let err = assign_port(&reserved, || {
            calls += 1;
            Ok(51000)
        })
        .unwrap_err();
        assert_eq!(calls, ASSIGN_ATTEMPTS);
        assert!(err.to_string().contains("devy.lock"), "{err}");
    }

    #[test]
    fn recorded_ports_expose_the_detected_worktree() {
        let tmp = crate::test_support::tmp_dir();
        let (main, feat) = crate::test_support::fake_linked_worktree(&tmp);
        let recorded = RecordedPorts::with_lock(&feat, None);
        assert_eq!(recorded.worktree(), crate::worktree::detect(&feat).as_ref());
        assert!(recorded.worktree().is_some());
        assert!(RecordedPorts::with_lock(&main, None).worktree().is_none());
    }

    #[test]
    fn conflicts_are_detected_on_worktree_ports_not_lock_ports() {
        let tmp = crate::test_support::tmp_dir();
        let (main, feat) = crate::test_support::fake_linked_worktree(&tmp);
        worktree_ports(&[("redis", 52000), ("memcached", 52000)])
            .write_if_changed(&feat)
            .unwrap();
        let lock = lock_with(&[("redis", 51000), ("memcached", 51001)]);
        let deps = || vec![Dependency::simple("redis"), Dependency::simple("memcached")];

        let in_feat = RecordedPorts::with_lock(&feat, Some(lock.clone()));
        let err = resolve_and_check(
            &mut deps(),
            in_feat.source(),
            &pm("nix"),
            PortMode::ReadOnly,
        )
        .unwrap_err();
        assert!(err.to_string().contains("52000"), "{err}");

        let in_main = RecordedPorts::with_lock(&main, Some(lock));
        resolve_and_check(
            &mut deps(),
            in_main.source(),
            &pm("nix"),
            PortMode::ReadOnly,
        )
        .unwrap();
    }

    #[test]
    fn recorded_ports_in_a_worktree_ignore_the_locks_assigned_port() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        let recorded = RecordedPorts::with_lock(&feat, Some(lock_with(&[("redis", 51000)])));
        assert!(recorded.in_worktree());
        assert_eq!(
            recorded.lock().unwrap().get("redis").unwrap().assigned_port,
            Some(51000)
        );
        let mut deps = vec![Dependency::simple("redis")];
        let r =
            resolve_ports(&mut deps, recorded.source(), &pm("nix"), PortMode::ReadOnly).unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Unassigned));
    }

    #[test]
    fn worktree_reuses_its_own_recorded_port() {
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        worktree_ports(&[("redis", 52000)])
            .write_if_changed(&feat)
            .unwrap();
        let recorded = RecordedPorts::with_lock(&feat, Some(lock_with(&[("redis", 51000)])));
        for mode in [PortMode::Assign, PortMode::ReadOnly] {
            let mut deps = vec![Dependency::simple("redis")];
            let r = resolve_ports(&mut deps, recorded.source(), &pm("nix"), mode).unwrap();
            assert_eq!(r[0], Some(ResolvedPort::Locked(52000)));
            assert_eq!(port_of(&deps[0]), Some(52000));
        }
    }

    #[test]
    fn update_keeps_worktree_ports() {
        // `devy up --update` drops the lock for version pinning only; the port source is
        // still the worktree file.
        let tmp = crate::test_support::tmp_dir();
        let (_, feat) = crate::test_support::fake_linked_worktree(&tmp);
        worktree_ports(&[("redis", 52000)])
            .write_if_changed(&feat)
            .unwrap();
        let recorded = RecordedPorts::with_lock(&feat, None);
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(&mut deps, recorded.source(), &pm("nix"), PortMode::Assign).unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Locked(52000)));
    }

    #[test]
    fn worktree_explicit_port_still_wins() {
        let wt = worktree_ports(&[("redis", 52000)]);
        let mut deps = vec![dep_with_port("redis", 6380)];
        let r = resolve_ports(
            &mut deps,
            PortSource::Worktree {
                ports: &wt,
                lock: None,
                main_lock: None,
            },
            &pm("nix"),
            PortMode::Assign,
        )
        .unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Explicit(6380)));
    }

    #[test]
    fn privileged_worktree_port_counts_as_unrecorded() {
        let wt = worktree_ports(&[("redis", 80)]);
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(
            &mut deps,
            PortSource::Worktree {
                ports: &wt,
                lock: None,
                main_lock: None,
            },
            &pm("nix"),
            PortMode::ReadOnly,
        )
        .unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Unassigned));
    }

    #[test]
    fn outside_a_worktree_the_worktree_file_is_never_read() {
        let dir = crate::test_support::tmp_dir();
        std::fs::create_dir(dir.join(".git")).unwrap();
        std::fs::create_dir(dir.join(".devy")).unwrap();
        std::fs::write(dir.join(".devy/worktree.yml"), "not: [valid").unwrap();
        let warnings = crate::output::with_warn_messages(|| {
            let recorded = RecordedPorts::with_lock(&dir, Some(lock_with(&[("redis", 51000)])));
            assert!(!recorded.in_worktree());
            let mut deps = vec![Dependency::simple("redis")];
            let r = resolve_ports(&mut deps, recorded.source(), &pm("nix"), PortMode::ReadOnly)
                .unwrap();
            assert_eq!(r[0], Some(ResolvedPort::Locked(51000)));
        });
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn unassigned_error_names_the_port_source() {
        let wt = worktree_ports(&[]);
        assert_eq!(
            PortSource::Lock(None).unassigned_error("redis").to_string(),
            "'redis' has no port in devy.lock yet — run `devy up` first"
        );
        assert_eq!(
            PortSource::Worktree {
                ports: &wt,
                lock: None,
                main_lock: None,
            }
            .unassigned_error("redis")
            .to_string(),
            "'redis' has no port in this worktree yet — run `devy up` first"
        );
    }

    #[test]
    fn worktree_ports_for_records_applicable_ports_by_canonical_name() {
        let mut deps = vec![
            Dependency::simple("postgres"),
            dep_with_port("redis", 6380),
            Dependency::simple("node"),
        ];
        let resolved = resolve_ports(
            &mut deps,
            PortSource::Lock(None),
            &pm("nix"),
            PortMode::Assign,
        )
        .unwrap();
        let wt = worktree_ports_for(&deps, &resolved, &pm("nix"));
        assert_eq!(wt.get("redis"), None, "explicit ports are not recorded");
        assert!(wt.get("postgresql").is_some());
        assert_eq!(wt.ports.len(), 1, "{wt:?}");
        // Brew can't apply ports, so nothing is recorded.
        assert!(
            worktree_ports_for(&deps, &resolved, &pm("brew"))
                .ports
                .is_empty()
        );
    }

    // ── shared_fixed_port_warning ─────────────────────────────────────────────

    #[test]
    fn fixed_port_warning_for_explicit_nix_port() {
        let msg = shared_fixed_port_warning(&dep_with_port("redis", 6380), &pm("nix"))
            .expect("must warn");
        assert_eq!(
            msg,
            "'redis' has a fixed port 6380 in devy.yml, so it can't run in this worktree and the main checkout at the same time"
        );
    }

    #[test]
    fn no_fixed_port_warning_for_brew_or_unset_ports() {
        assert!(shared_fixed_port_warning(&dep_with_port("redis", 6380), &pm("brew")).is_none());
        assert!(shared_fixed_port_warning(&Dependency::simple("redis"), &pm("nix")).is_none());
        assert!(shared_fixed_port_warning(&dep_with_port("node", 3000), &pm("nix")).is_none());
    }
}
