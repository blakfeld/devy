//! Service port resolution shared by `up`, `check`, `status`, `start`, `stop` and `restart`.
//!
//! Precedence: explicit port in devy.yml → `assigned_port` in devy.lock (when the backend
//! can apply a port) → a fresh OS-assigned port (`up` only, when the backend can apply a
//! port) → the module's default port.

use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::config::{Dependency, ExtraValue};
use crate::lock::LockFile;
use crate::modules;
use crate::package_manager::PackageManager;

/// Whether port resolution may pick new random ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortMode {
    /// `devy up`: unassigned ports get a free port from the OS.
    Assign,
    /// Every other command: never assigns, never writes devy.lock.
    ReadOnly,
}

/// Where a service's effective port came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedPort {
    Explicit(u16),
    Locked(u16),
    Assigned(u16),
    Default(u16),
    /// The backend can apply a port, but none is in the lock yet and the mode is read-only.
    Unassigned,
}

impl ResolvedPort {
    pub(crate) fn port(self) -> Option<u16> {
        match self {
            Self::Explicit(p) | Self::Locked(p) | Self::Assigned(p) | Self::Default(p) => Some(p),
            Self::Unassigned => None,
        }
    }
}

/// Resolves the port of every service dep and writes it into `dep.extra[port_key]` so
/// health checks, config writers and env vars all see the same value.
///
/// Returns one entry per dep, `None` for deps without a configurable port.
pub(crate) fn resolve_ports(
    deps: &mut [Dependency],
    lock: Option<&LockFile>,
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
        let r = resolve_one(dep, key, lock, pm, mode)?;
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
    lock: Option<&LockFile>,
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
    if module.port_applicable(pm) {
        let canonical = modules::canonical_name(&dep.name);
        if let Some(p) = lock
            .and_then(|l| l.get(canonical))
            .and_then(|d| d.assigned_port)
        {
            return Ok(ResolvedPort::Locked(p));
        }
        return match mode {
            PortMode::Assign => {
                let p = modules::helpers::find_available_port()
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
    lock: Option<&LockFile>,
    pm: &dyn PackageManager,
    mode: PortMode,
) -> Result<Vec<Option<ResolvedPort>>> {
    let resolved = resolve_ports(deps, lock, pm, mode)?;
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
    if Some(port) == module.default_port() || module.port_applicable(pm) {
        return None;
    }
    Some(format!(
        "devy cannot make {} listen on port {port} with {} — configure the service to listen on {port} manually",
        dep.name,
        pm.name()
    ))
}

/// Loads devy.lock next to devy.yml for read-only port resolution.
pub(crate) fn load_lock(project_root: &std::path::Path) -> Result<Option<LockFile>> {
    LockFile::load(&project_root.join(crate::lock::PATH)).context("Failed to read devy.lock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockedDep;
    use crate::package_manager::MockPackageManager;
    use std::collections::BTreeMap;

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
    fn resolve_service_ports_injects_random_port_when_no_lock() {
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(&mut deps, None, &pm("nix"), PortMode::Assign).unwrap();
        assert!(matches!(r[0], Some(ResolvedPort::Assigned(p)) if p > 0));
        assert!(port_of(&deps[0]).is_some(), "port must be injected");
    }

    #[test]
    fn resolve_service_ports_reuses_locked_port() {
        let lock = lock_with(&[("redis", 16379)]);
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(&mut deps, Some(&lock), &pm("nix"), PortMode::Assign).unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Locked(16379)));
        assert_eq!(port_of(&deps[0]), Some(16379));
    }

    #[test]
    fn resolve_service_ports_respects_user_configured_port() {
        let lock = lock_with(&[("redis", 51000)]);
        let mut deps = vec![dep_with_port("redis", 6380)];
        let r = resolve_ports(&mut deps, Some(&lock), &pm("nix"), PortMode::Assign).unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Explicit(6380)));
        assert_eq!(port_of(&deps[0]), Some(6380));
    }

    #[test]
    fn resolve_service_ports_skips_non_service_deps() {
        let mut deps = vec![Dependency::simple("node")];
        let r = resolve_ports(&mut deps, None, &pm("nix"), PortMode::Assign).unwrap();
        assert_eq!(r[0], None);
        assert!(port_of(&deps[0]).is_none());
    }

    #[test]
    fn resolve_service_ports_assigns_distinct_ports() {
        let mut deps = vec![Dependency::simple("redis"), Dependency::simple("mysql")];
        resolve_ports(&mut deps, None, &pm("nix"), PortMode::Assign).unwrap();
        assert_ne!(port_of(&deps[0]), port_of(&deps[1]));
    }

    #[test]
    fn resolve_uses_default_and_ignores_lock_when_backend_cannot_apply() {
        let lock = lock_with(&[("redis", 51000)]);
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(&mut deps, Some(&lock), &pm("brew"), PortMode::Assign).unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Default(6379)));
        assert_eq!(port_of(&deps[0]), Some(6379));
    }

    #[test]
    fn resolve_read_only_leaves_unlocked_ports_unassigned() {
        let mut deps = vec![Dependency::simple("redis")];
        let r = resolve_ports(&mut deps, None, &pm("nix"), PortMode::ReadOnly).unwrap();
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
        let r = resolve_ports(&mut deps, Some(&lock), &pm("nix"), PortMode::ReadOnly).unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Locked(51000)));
    }

    #[test]
    fn resolve_reads_lock_by_canonical_name() {
        let lock = lock_with(&[("postgresql", 51000)]);
        let mut deps = vec![Dependency::simple("postgres")];
        let r = resolve_ports(&mut deps, Some(&lock), &pm("nix"), PortMode::ReadOnly).unwrap();
        assert_eq!(r[0], Some(ResolvedPort::Locked(51000)));
    }

    #[test]
    fn resolve_rejects_out_of_range_explicit_port() {
        for bad in [0, 99999] {
            let mut deps = vec![dep_with_port("mysql", bad)];
            let err = resolve_ports(&mut deps, None, &pm("nix"), PortMode::Assign).unwrap_err();
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
        resolve_and_check(&mut deps, lock, &pm(pm_name), PortMode::ReadOnly).map(|_| ())
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
}
