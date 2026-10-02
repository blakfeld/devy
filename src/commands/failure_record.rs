//! `.devy/last-up-failure.json`: what the most recent failed `devy up` was doing when it
//! failed. Written by `up`, read by `doctor`. Local only — never sent anywhere by `up`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::up::UpProgress;

pub const PATH: &str = ".devy/last-up-failure.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRecord {
    /// The error chain exactly as printed after `error:`.
    pub error_chain: String,
    pub step: Option<String>,
    pub dependency: Option<String>,
    pub platform: String,
    pub backend: Option<String>,
    pub devy_version: String,
    /// UTC, RFC 3339.
    pub timestamp: String,
}

impl FailureRecord {
    pub fn new(err: &anyhow::Error, progress: &UpProgress) -> Self {
        Self {
            error_chain: format!("{err:#}"),
            step: progress.step.map(String::from),
            dependency: progress.dependency.clone(),
            platform: platform(),
            backend: progress.backend.clone(),
            devy_version: env!("CARGO_PKG_VERSION").to_string(),
            timestamp: utc_timestamp(std::time::SystemTime::now()),
        }
    }
}

/// `<os>-<arch>`, e.g. `macos-aarch64`.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub fn path(project_root: &Path) -> PathBuf {
    project_root.join(PATH)
}

/// Replaces any existing record atomically. On Unix the file is owner-only (0600).
pub fn write(project_root: &Path, record: &FailureRecord) -> Result<()> {
    let target = path(project_root);
    let dir = target.parent().expect("PATH has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let body = serde_json::to_string_pretty(record)?;
    let tmp = dir.join(format!(".last-up-failure.{}.tmp", std::process::id()));
    write_private(&tmp, body.as_bytes()).with_context(|| format!("Failed to write {PATH}"))?;
    std::fs::rename(&tmp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// Deletes the record; a missing record is not an error.
pub fn remove(project_root: &Path) -> Result<()> {
    match std::fs::remove_file(path(project_root)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("Failed to remove {PATH}"))
        }
        _ => Ok(()),
    }
}

/// The record, or `None` when there is none.
pub fn load(project_root: &Path) -> Result<Option<FailureRecord>> {
    let text = match std::fs::read_to_string(path(project_root)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {PATH}")),
    };
    serde_json::from_str(&text)
        .map(Some)
        .with_context(|| format!("Failed to parse {PATH}"))
}

/// `time` as `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_timestamp(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Proleptic Gregorian date for days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn record(chain: &str) -> FailureRecord {
        FailureRecord::new(
            &anyhow::anyhow!("inner").context(chain.to_string()),
            &UpProgress {
                step: Some("install"),
                dependency: Some("postgres".into()),
                backend: Some("nix".into()),
            },
        )
    }

    #[test]
    fn timestamp_is_rfc3339_utc() {
        assert_eq!(utc_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = UNIX_EPOCH + Duration::from_secs(1_791_000_000);
        assert_eq!(utc_timestamp(t), "2026-10-03T04:00:00Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(utc_timestamp(leap), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn new_captures_chain_step_and_dependency() {
        let r = record("Failed to install postgres");
        assert_eq!(r.error_chain, "Failed to install postgres: inner");
        assert_eq!(r.step.as_deref(), Some("install"));
        assert_eq!(r.dependency.as_deref(), Some("postgres"));
        assert_eq!(r.backend.as_deref(), Some("nix"));
        assert_eq!(r.devy_version, env!("CARGO_PKG_VERSION"));
        assert!(r.platform.contains('-'));
    }

    #[test]
    fn write_creates_devy_dir_and_round_trips() {
        let dir = crate::test_support::tmp_dir();
        let r = record("boom");
        write(&dir, &r).unwrap();
        assert_eq!(load(&dir).unwrap(), Some(r));
    }

    #[test]
    fn write_replaces_previous_record() {
        let dir = crate::test_support::tmp_dir();
        write(&dir, &record("first")).unwrap();
        write(&dir, &record("second")).unwrap();
        assert!(
            load(&dir)
                .unwrap()
                .unwrap()
                .error_chain
                .starts_with("second")
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.join(".devy"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "no temp files left: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn write_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::tmp_dir();
        write(&dir, &record("x")).unwrap();
        let mode = std::fs::metadata(path(&dir)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn remove_deletes_and_tolerates_missing() {
        let dir = crate::test_support::tmp_dir();
        remove(&dir).unwrap();
        write(&dir, &record("x")).unwrap();
        remove(&dir).unwrap();
        assert!(!path(&dir).exists());
        assert_eq!(load(&dir).unwrap(), None);
    }
}
