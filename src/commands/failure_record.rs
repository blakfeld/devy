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

    /// [`FailureRecord::new`] for the project at `project_root`. When its `devy.yml` is
    /// a symlink or not a regular file, the error (whose parse-error text can quote the
    /// link target, which `doctor` would send to claude) is replaced by
    /// [`CONFIG_NOT_REGULAR`].
    pub fn for_project(project_root: &Path, err: &anyhow::Error, progress: &UpProgress) -> Self {
        let mut record = Self::new(err, progress);
        let regular = std::fs::symlink_metadata(project_root.join("devy.yml"))
            .is_ok_and(|meta| meta.file_type().is_file());
        if !regular {
            record.error_chain = CONFIG_NOT_REGULAR.to_string();
        }
        record
    }
}

/// The recorded error when `devy.yml` is not a regular file.
pub const CONFIG_NOT_REGULAR: &str =
    "devy.yml is a symlink or not a regular file; the error was not recorded";

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
    crate::fs_safe::ensure_devy_dir(project_root)?;
    let body = serde_json::to_string_pretty(record)?;
    crate::fs_safe::write_atomic(&target, body.as_bytes(), 0o600)
        .with_context(|| format!("Failed to write {PATH}"))
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

/// The record, or `None` when there is none. It goes into `doctor`'s AI context, so it
/// is read only as a regular, non-symlink file inside the project
/// ([`crate::ai::context_file`]); anything else counts as no record.
pub fn load(project_root: &Path) -> Result<Option<FailureRecord>> {
    let Some(text) = crate::ai::context_file(project_root, PATH) else {
        if std::fs::symlink_metadata(path(project_root)).is_ok() {
            anyhow::bail!("{PATH} is not a regular file in the project; ignoring it");
        }
        return Ok(None);
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

    #[test]
    fn regular_config_keeps_the_error_chain() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("devy.yml"), "name: x\n").unwrap();
        let err = anyhow::anyhow!("exit 1").context("Failed to install postgres");
        let record = FailureRecord::for_project(&dir, &err, &UpProgress::default());
        assert_eq!(record.error_chain, "Failed to install postgres: exit 1");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_error_is_not_recorded() {
        let dir = crate::test_support::tmp_dir();
        let outside = crate::test_support::tmp_dir();
        std::fs::write(outside.join("secret"), "SENTINEL_SECRET: [\n").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), dir.join("devy.yml")).unwrap();
        let err = anyhow::anyhow!("did not find expected node content near SENTINEL_SECRET")
            .context("Failed to parse devy.yml");
        let record = FailureRecord::for_project(&dir, &err, &UpProgress::default());
        assert_eq!(record.error_chain, CONFIG_NOT_REGULAR);
        // A directory (or anything else that is not a regular file) is treated the same.
        let dir2 = crate::test_support::tmp_dir();
        std::fs::create_dir(dir2.join("devy.yml")).unwrap();
        let record = FailureRecord::for_project(&dir2, &err, &UpProgress::default());
        assert_eq!(record.error_chain, CONFIG_NOT_REGULAR);
    }

    #[cfg(unix)]
    #[test]
    fn load_refuses_a_symlinked_record_without_reading_it() {
        let dir = crate::test_support::tmp_dir();
        let outside = crate::test_support::tmp_dir();
        std::fs::write(outside.join("secret"), "SENTINEL_SECRET\n").unwrap();
        std::fs::create_dir(dir.join(".devy")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), path(&dir)).unwrap();
        let err = format!("{:#}", load(&dir).unwrap_err());
        assert!(err.contains("not a regular file"), "{err}");
        assert!(!err.contains("SENTINEL"), "{err}");
    }

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
        let mut leftovers: Vec<_> = std::fs::read_dir(dir.join(".devy"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        leftovers.sort();
        assert_eq!(
            leftovers,
            [".gitignore", "last-up-failure.json"],
            "no temp files left"
        );
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
