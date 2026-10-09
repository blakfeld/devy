//! Filesystem safety primitives: atomic symlink-refusing writes and private per-user directories.
//!
//! A repository devy runs in is untrusted input: it can commit symlinks where devy writes
//! files, directories where devy expects its own state, and executables on the PATH an
//! activated environment builds. Everything here exists so those can't redirect devy's
//! writes, executables or sockets.

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(not(test))]
use std::sync::RwLock;

// ── Atomic writes ─────────────────────────────────────────────────────────────

fn symlink_error(path: &Path) -> anyhow::Error {
    anyhow!(
        "refusing to write {}: it is a symbolic link",
        path.display()
    )
}

/// Fails with the refusing-to-write error when `path` is a symlink. A missing path is fine.
pub fn refuse_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(symlink_error(path)),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to inspect {}", path.display())),
    }
}

/// An unpredictable 64-bit hex string: `RandomState` is seeded from the OS, and the
/// counter keeps two calls in one process apart.
fn random_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(elapsed.as_nanos());
    }
    format!("{:016x}", hasher.finish())
}

/// An unpredictable 128-bit lowercase hex string (32 digits).
pub fn random_hex128() -> String {
    format!("{}{}", random_suffix(), random_suffix())
}

/// Opens a brand-new file at `path` (mode 0600 on Unix) without following a symlink there.
pub fn create_new_nofollow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Creates `<dir>/.<name>.<random>.tmp` exclusively, retrying on a name collision (for
/// example a symlink a repository planted at a name devy might pick).
fn create_temp(dir: &Path, name: &OsStr) -> Result<(PathBuf, File)> {
    for _ in 0..32 {
        let mut tmp_name = OsString::from(".");
        tmp_name.push(name);
        tmp_name.push(format!(".{}.tmp", random_suffix()));
        let tmp = dir.join(tmp_name);
        match create_new_nofollow(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to create {}", tmp.display()));
            }
        }
    }
    bail!(
        "Failed to create a temporary file in {}: every name tried already exists",
        dir.display()
    )
}

fn set_mode(file: &File, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (file, mode);
        Ok(())
    }
}

/// Writes `bytes` to `path` without ever writing through a symlink: the destination is
/// checked, the content goes to a new temporary file in the same directory (created
/// exclusively and with `O_NOFOLLOW`), which is synced, given `mode` (Unix only) and
/// renamed over `path`. A symlink at `path` fails with
/// `refusing to write <path>: it is a symbolic link` and its target is left untouched.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    refuse_symlink(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("Failed to write {}: no file name", path.display()))?;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let (tmp, mut file) = create_temp(dir, name)?;
    let result = (|| -> Result<()> {
        let err = || format!("Failed to write {}", tmp.display());
        file.write_all(bytes).with_context(err)?;
        set_mode(&file, mode).with_context(err)?;
        file.sync_all().with_context(err)?;
        drop(file);
        // Re-checked for Windows, where the rename has no O_NOFOLLOW counterpart. On Unix
        // `rename` replaces a symlink planted meanwhile rather than following it.
        refuse_symlink(path)?;
        fs::rename(&tmp, path).with_context(|| format!("Failed to write {}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Opens (creating when missing) a lock-guard file without following a symlink at
/// `path`. The file is never replaced, so concurrent devy processes lock the same inode.
pub fn open_lock_file(path: &Path) -> Result<File> {
    refuse_symlink(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|e| {
        if is_nofollow_error(&e) {
            symlink_error(path)
        } else {
            anyhow::Error::new(e).context(format!("Failed to open {}", path.display()))
        }
    })?;
    refuse_symlink(path)?;
    Ok(file)
}

/// Opens `path` for reading without following a symlink at it (`O_NOFOLLOW`; on Windows
/// a reparse point is opened itself) and without blocking on a FIFO (`O_NONBLOCK`, which
/// regular files ignore).
pub fn open_read_nofollow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

/// Reads `path` when it is a regular file (not a symlink, FIFO or device) of at most
/// `cap` bytes. The file opened must be the regular file checked, so a swap between the
/// check and the open is refused. A missing file keeps its `NotFound` kind; anything
/// else devy refuses is `InvalidData`.
pub fn read_regular_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    read_capped(open_regular_nofollow(path)?, path, cap)
}

/// [`read_regular_capped`] for a file that may be a non-redirecting reparse point on
/// Windows: a OneDrive/Cloud Files placeholder, a deduplicated file or a WOF-compressed
/// one. Opened with `FILE_FLAG_OPEN_REPARSE_POINT`, such a file can yield its reparse
/// stub instead of its content, so [`open_regular_following_placeholders`] opens it
/// normally. Elsewhere it is exactly [`read_regular_capped`].
pub fn read_regular_capped_following_placeholders(
    path: &Path,
    cap: u64,
) -> std::io::Result<Vec<u8>> {
    read_capped(open_regular_following_placeholders(path)?, path, cap)
}

/// Reads at most `cap` bytes of `file` (opened from `path`), refusing a longer file
/// with `InvalidData`.
fn read_capped(file: File, path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("{} is larger than {cap} bytes", path.display()),
        ));
    }
    Ok(bytes)
}

/// Opens `path` for reading when it is a regular file: not a symlink (or, on Windows, a
/// junction or other link), FIFO or device. The file opened must be the regular file
/// checked, so a swap between the check and the open is refused. A missing file keeps
/// its `NotFound` kind; anything else devy refuses is `InvalidData`.
pub fn open_regular_nofollow(path: &Path) -> std::io::Result<File> {
    let invalid = |what: &str| {
        std::io::Error::new(ErrorKind::InvalidData, format!("{} {what}", path.display()))
    };
    let checked = fs::symlink_metadata(path)?;
    if checked.file_type().is_symlink() {
        return Err(invalid("is a symlink; devy does not follow it"));
    }
    if !checked.file_type().is_file() {
        return Err(invalid("is not a regular file"));
    }
    let file = open_read_nofollow(path)?;
    let opened = file.metadata()?;
    if !opened.file_type().is_file() || !same_inode(&checked, &opened) {
        return Err(invalid("changed while it was being read"));
    }
    Ok(file)
}

/// [`open_regular_nofollow`], except that on Windows the file is opened normally rather
/// than with `FILE_FLAG_OPEN_REPARSE_POINT`, so a non-redirecting reparse point (cloud
/// placeholder, dedup, WOF) yields its content. A redirecting link (symlink, junction,
/// mount point: `is_symlink`) is still refused by the `symlink_metadata` check, and the
/// handle opened must be the file checked ([`same_file_windows`]), so a swap between
/// the check and the open is refused once noticed. Unlike `open_regular_nofollow`, a
/// link swapped in during that window is followed by the open itself before being
/// refused. Elsewhere this is exactly [`open_regular_nofollow`] (`O_NOFOLLOW`).
pub fn open_regular_following_placeholders(path: &Path) -> std::io::Result<File> {
    #[cfg(not(windows))]
    {
        open_regular_nofollow(path)
    }
    #[cfg(windows)]
    {
        let invalid = |what: &str| {
            std::io::Error::new(ErrorKind::InvalidData, format!("{} {what}", path.display()))
        };
        let checked = fs::symlink_metadata(path)?;
        if checked.file_type().is_symlink() {
            return Err(invalid("is a symlink; devy does not follow it"));
        }
        if !checked.file_type().is_file() {
            return Err(invalid("is not a regular file"));
        }
        let file = File::open(path)?;
        let opened = file.metadata()?;
        if !opened.file_type().is_file() || !same_file_windows(&checked, &opened) {
            return Err(invalid("changed while it was being read"));
        }
        Ok(file)
    }
}

/// Whether `a` (from `symlink_metadata`) and `b` (from the opened handle) describe the
/// same file. Stable std exposes no file index or volume serial number
/// (`windows_by_handle` is unstable), so this compares the creation time, last write
/// time and size, which a cloud file's hydration and a deduplicated or compressed
/// file's reparse data leave unchanged. The attributes are not compared, because
/// hydration can change them (`RECALL_ON_DATA_ACCESS`, `OFFLINE`, pinning).
#[cfg(windows)]
fn same_file_windows(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    a.creation_time() == b.creation_time()
        && a.last_write_time() == b.last_write_time()
        && a.file_size() == b.file_size()
}

#[cfg(unix)]
fn same_inode(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_inode(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

/// `O_NOFOLLOW` on a symlink fails with `ELOOP`.
fn is_nofollow_error(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::ELOOP)
    }
    #[cfg(not(unix))]
    {
        let _ = e;
        false
    }
}

/// Creates `dir` and any missing directories between `root` and it, refusing when any
/// component below `root` is a symlink or not a directory. `dir` must be inside `root`.
pub fn ensure_dir_in(root: &Path, dir: &Path) -> Result<()> {
    let rel = dir.strip_prefix(root).map_err(|_| {
        anyhow!(
            "refusing to create {}: it is outside {}",
            dir.display(),
            root.display()
        )
    })?;
    let mut current = root.to_path_buf();
    for component in rel.components() {
        match component {
            Component::Normal(part) => current.push(part),
            Component::CurDir => continue,
            _ => bail!(
                "refusing to create {}: unexpected path component",
                dir.display()
            ),
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) => require_real_dir(&current, &meta)?,
            Err(e) if e.kind() == ErrorKind::NotFound => match fs::create_dir(&current) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let meta = fs::symlink_metadata(&current)
                        .with_context(|| format!("Failed to inspect {}", current.display()))?;
                    require_real_dir(&current, &meta)?;
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("Failed to create {}", current.display()));
                }
            },
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn require_real_dir(path: &Path, meta: &fs::Metadata) -> Result<()> {
    if meta.file_type().is_symlink() {
        return Err(symlink_error(path));
    }
    if !meta.is_dir() {
        bail!(
            "refusing to write {}: it is not a directory",
            path.display()
        );
    }
    Ok(())
}

// ── Private per-user directories ──────────────────────────────────────────────

/// The real uid of the user running devy.
#[cfg(unix)]
pub fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no arguments, cannot fail and has no side effects.
    unsafe { libc::getuid() }
}

/// The per-user base for devy's private directories: `$XDG_RUNTIME_DIR` when set,
/// otherwise `$TMPDIR`, otherwise the system temporary directory.
pub fn user_base() -> PathBuf {
    ["XDG_RUNTIME_DIR", "TMPDIR"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        // A variable inherited from another session (`su`, containers) may name a
        // directory that doesn't exist here.
        .find(|p| p.is_absolute() && p.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

fn user_dir_name() -> String {
    #[cfg(unix)]
    {
        format!("devy-{}", current_uid())
    }
    #[cfg(not(unix))]
    {
        "devy".to_string()
    }
}

/// Where `user_dir` lives, without creating or checking it. Before reading anything
/// there, check it with [`existing_private_dir`] (read-only) or use [`user_dir`].
#[cfg(any(test, target_os = "macos", target_os = "linux"))]
pub fn user_dir_path() -> PathBuf {
    user_base().join(user_dir_name())
}

/// devy's private per-user directory (`<user_base>/devy-<uid>`), created 0700 when
/// missing and owner- and mode-checked when it exists.
pub fn user_dir() -> Result<PathBuf> {
    private_dir(&user_base(), &user_dir_name(), false)
}

/// Checks that `dir`, when it exists, is a real directory owned by the current user
/// with mode 0700 (as [`private_dir`] requires of an existing directory); a missing
/// `dir` passes. Fails with an ownership or mode error otherwise, or when it can't be
/// examined. Never creates anything.
#[cfg(any(test, target_os = "macos", target_os = "linux"))]
pub fn existing_private_dir(dir: &Path) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        _ => check_private(dir, expected_owner()),
    }
}

/// Creates `<base>/<prefix>` (or `<base>/<prefix>-<random>` when `random`) with mode
/// 0700 and exclusive creation. An existing deterministic directory is accepted only
/// when it is a real directory owned by the current user with mode 0700; otherwise this
/// fails with an ownership or mode error. `base` itself must already exist.
pub fn private_dir(base: &Path, prefix: &str, random: bool) -> Result<PathBuf> {
    private_dir_as(base, prefix, random, expected_owner())
}

#[cfg(all(test, unix))]
thread_local! {
    static FAKE_OWNER: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with `private_dir` expecting directories to be owned by `uid`, so tests can
/// present a directory the current user created as another user's.
#[cfg(all(test, unix))]
pub fn with_fake_owner<T>(uid: u32, f: impl FnOnce() -> T) -> T {
    FAKE_OWNER.with(|c| c.set(Some(uid)));
    let result = f();
    FAKE_OWNER.with(|c| c.set(None));
    result
}

/// Whether `uid` is the current user (as [`with_fake_owner`] presents it in tests) or
/// root: the only owners devy trusts for files it reads from shared places.
#[cfg(unix)]
pub fn is_user_or_root(uid: u32) -> bool {
    uid == expected_owner() || uid == 0
}

/// The uid `private_dir` requires as owner: the current user's.
fn expected_owner() -> u32 {
    #[cfg(all(test, unix))]
    if let Some(uid) = FAKE_OWNER.with(|c| c.get()) {
        return uid;
    }
    #[cfg(unix)]
    {
        current_uid()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// `private_dir` checking ownership against `uid` instead of the current user.
fn private_dir_as(base: &Path, prefix: &str, random: bool, uid: u32) -> Result<PathBuf> {
    for _ in 0..32 {
        let dir = if random {
            base.join(format!("{prefix}-{}", random_suffix()))
        } else {
            base.join(prefix)
        };
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        match builder.create(&dir) {
            Ok(()) => {
                check_private(&dir, uid)?;
                return Ok(dir);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists && random => continue,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                check_private(&dir, uid)?;
                return Ok(dir);
            }
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to create {}", dir.display()));
            }
        }
    }
    bail!(
        "Failed to create a private directory in {}: every name tried already exists",
        base.display()
    )
}

/// Requires `dir` to be a real directory owned by `uid` with mode 0700 (Unix); on other
/// platforms only that it is a real directory.
fn check_private(dir: &Path, uid: u32) -> Result<()> {
    let meta = fs::symlink_metadata(dir)
        .with_context(|| format!("Failed to inspect {}", dir.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        bail!(
            "{} is not a directory owned by the current user — remove it so devy can create its own",
            dir.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != uid {
            bail!(
                "{} is not owned by the current user — remove it so devy can create its own",
                dir.display()
            );
        }
        let mode = meta.mode() & 0o777;
        if mode != 0o700 {
            bail!(
                "{} has mode {mode:03o}, expected 700 — remove it so devy can create its own",
                dir.display()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = uid;
    Ok(())
}

/// A private directory that is removed when dropped.
pub struct PrivateTempDir(PathBuf);

impl PrivateTempDir {
    /// A new random-named 0700 directory inside `user_dir()`.
    pub fn new(prefix: &str) -> Result<Self> {
        Self::new_in(&user_dir()?, prefix)
    }

    /// A new random-named 0700 directory inside `base`, which must already exist.
    pub fn new_in(base: &Path, prefix: &str) -> Result<Self> {
        Ok(Self(private_dir(base, prefix, true)?))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ── Devy-managed directories ──────────────────────────────────────────────────

/// The project-relative directories devy writes into or executes from.
pub const MANAGED_DIRS: [&str; 2] = [".devy", ".shadowenv.d"];

/// `.devy/nix-profile`, relative to the project root.
pub const NIX_PROFILE: &str = ".devy/nix-profile";

/// devy's per-checkout state directory, relative to the project root.
pub const DEVY_DIR: &str = ".devy";

/// What `.devy/.gitignore` holds when devy creates it: ignore everything in `.devy/`.
const DEVY_GITIGNORE: &[u8] = b"*\n";

/// Creates `<root>/.devy` when missing (refusing a symlinked or non-directory component,
/// as [`ensure_dir_in`] does) and makes sure `.devy/.gitignore` exists, so git ignores
/// devy's state without the user editing their own ignore rules. A missing
/// `.gitignore` is created with `*`; an existing one, or anything else at that name
/// (including a symlink), is never modified. Every creation of `.devy/` goes through
/// here. Returns the `.devy` path.
pub fn ensure_devy_dir(root: &Path) -> Result<PathBuf> {
    let devy = root.join(DEVY_DIR);
    ensure_dir_in(root, &devy)?;
    let ignore = devy.join(".gitignore");
    match create_new_nofollow(&ignore) {
        Ok(mut file) => {
            let written = file
                .write_all(DEVY_GITIGNORE)
                .and_then(|()| set_mode(&file, 0o644));
            if let Err(e) = written {
                drop(file);
                let _ = fs::remove_file(&ignore);
                return Err(e).with_context(|| format!("Failed to write {}", ignore.display()));
            }
        }
        // Already there (or a symlink, which `create_new` refuses without following).
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) if is_nofollow_error(&e) => {}
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to create {}", ignore.display()));
        }
    }
    Ok(devy)
}

/// Creates `dir`, which must be inside `<root>/.devy`, after [`ensure_devy_dir`], with
/// the same symlink refusals as [`ensure_dir_in`].
pub fn ensure_devy_subdir(root: &Path, dir: &Path) -> Result<()> {
    let devy = ensure_devy_dir(root)?;
    if !dir.starts_with(&devy) {
        bail!(
            "refusing to create {}: it is outside {}",
            dir.display(),
            devy.display()
        );
    }
    ensure_dir_in(root, dir)
}

fn refusal(rel: &Path, what: &str) -> anyhow::Error {
    anyhow!(
        "{} is {what}; devy will not use it — remove it from the repository",
        rel.display()
    )
}

/// Verifies that `.devy/`, `.shadowenv.d/` and each of `venvs` (project-relative) is
/// absent or a real directory owned by the current user, that git tracks nothing under
/// them (in the project's repository, or in the nested repository, such as a
/// submodule, that holds them), that none of them is itself a repository, and that
/// `.devy/nix-profile`, when present, links into `/nix/store`.
///
/// On a refusal, shadowenv's trust for the project goes first, whichever command asked:
/// a `.shadowenv.d` that is committed, a symlink or foreign-owned is not devy's to hand
/// to shadowenv, and a signature an earlier `devy up` wrote would keep loading it.
pub fn check_managed_paths(root: &Path, venvs: &[PathBuf]) -> Result<()> {
    check_managed_paths_only(root, venvs).inspect_err(|_| {
        crate::env_manager::shadowenv::untrust(root);
    })
}

/// [`check_managed_paths`] without removing shadowenv's trust.
fn check_managed_paths_only(root: &Path, venvs: &[PathBuf]) -> Result<()> {
    let mut rels: Vec<PathBuf> = MANAGED_DIRS.iter().map(PathBuf::from).collect();
    rels.extend(venvs.iter().cloned());
    let mut top_level = Vec::new();
    for rel in &rels {
        check_managed_dir(root, rel, is_owned)?;
        match nested_repository(root, rel) {
            None => top_level.push(rel.clone()),
            Some((repo, inner)) if inner.as_os_str().is_empty() => {
                return Err(refusal(&repo, "a git repository of its own"));
            }
            // The project's index does not list a submodule's files (and git refuses a
            // pathspec inside one), so ask the nested repository itself.
            Some((repo, inner)) => match git_ls_files(root, &root.join(&repo), &[inner])? {
                Some(Some(_)) => return Err(refusal(rel, "tracked by git")),
                Some(None) => {}
                // A `.git` that git does not take for a repository: ask the project's own.
                None => top_level.push(rel.clone()),
            },
        }
    }
    if let Some(tracked) = git_tracked(root, &top_level)? {
        return Err(refusal(&tracked, "tracked by git"));
    }
    verified_nix_profile(root)?;
    Ok(())
}

/// The innermost directory from `rel` up to (not including) `root` that holds a `.git`
/// entry (a submodule or nested checkout), and `rel` relative to it. `rel` itself counts
/// (a repository at `.venv`), with an empty remainder.
fn nested_repository(root: &Path, rel: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut shown = PathBuf::new();
    let mut found = None;
    for component in rel.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        shown.push(part);
        if fs::symlink_metadata(root.join(&shown).join(".git")).is_ok() {
            found = Some(shown.clone());
        }
    }
    let repo = found?;
    let inner = shown
        .strip_prefix(&repo)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Some((repo, inner))
}

#[cfg(unix)]
fn is_owned(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.uid() == current_uid()
}

#[cfg(not(unix))]
fn is_owned(_meta: &fs::Metadata) -> bool {
    true
}

/// Checks every component of `rel` below `root`: each must be absent or a real
/// directory that `owned` accepts.
fn check_managed_dir(root: &Path, rel: &Path, owned: impl Fn(&fs::Metadata) -> bool) -> Result<()> {
    let mut current = root.to_path_buf();
    let mut shown = PathBuf::new();
    for component in rel.components() {
        let part = match component {
            Component::Normal(part) => part,
            Component::CurDir => continue,
            _ => bail!("{} is not a path inside the project", rel.display()),
        };
        current.push(part);
        shown.push(part);
        let meta = match fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to inspect {}", current.display()));
            }
        };
        if meta.file_type().is_symlink() {
            return Err(refusal(&shown, "a symbolic link"));
        }
        if !meta.is_dir() {
            return Err(refusal(&shown, "not a directory"));
        }
        if !owned(&meta) {
            // Most likely left by an earlier `sudo devy up`, not committed.
            return Err(anyhow!(
                "{} is not owned by the current user; devy will not use it — remove it or change its owner",
                shown.display()
            ));
        }
    }
    Ok(())
}

/// The first of `rels` under which git tracks a file, when the project is in a git
/// repository and git can be found outside the project. `None` when nothing is tracked
/// or git is unavailable (the symlink and owner checks still apply). Fails when git is
/// found but cannot list the index (a corrupt index, a repository git refuses as
/// unsafe, …): the check cannot be made, so it does not pass.
pub(crate) fn git_tracked(root: &Path, rels: &[PathBuf]) -> Result<Option<PathBuf>> {
    Ok(git_ls_files(root, root, rels)?.flatten())
}

/// [`git_tracked`] for `rels` relative to `repo` (the project root, or a nested
/// repository inside `project`), with `None` when git was not asked or does not see a
/// repository there (no `.git`, git unavailable, or `fatal: not a git repository`) and
/// `Some(None)` when it listed nothing. git is always found outside `project`, never
/// just outside `repo`.
fn git_ls_files(project: &Path, repo: &Path, rels: &[PathBuf]) -> Result<Option<Option<PathBuf>>> {
    let root = repo;
    if !root.ancestors().any(|dir| dir.join(".git").exists()) {
        return Ok(None);
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let Some(git) = which_outside_project_in("git", &path_var, Some(project)) else {
        return Ok(None);
    };
    // git itself (and anything it might spawn) sees only PATH entries outside the project.
    let canonical_project = project.canonicalize().ok();
    let safe_path = std::env::join_paths(std::env::split_paths(&path_var).filter(|entry| {
        entry.is_absolute() && !is_inside(entry, project, canonical_project.as_deref())
    }))
    .unwrap_or_default();
    // Case-insensitive pathspecs: on macOS and Windows `.Venv` *is* `.venv`, and plain
    // pathspecs would miss it even with core.ignorecase.
    let pathspecs = rels.iter().map(|rel| {
        format!(
            ":(icase,literal){}",
            rel.to_string_lossy().replace('\\', "/")
        )
    });
    let mut command = Command::new(&git);
    // No `GIT_*` variable from the (possibly project-influenced) environment reaches
    // git: `GIT_CONFIG_KEY_<n>` acts like `-c`, `GIT_DIR`/`GIT_INDEX_FILE` choose another
    // index, `GIT_EXEC_PATH` other programs, `GIT_TRACE*` writes to a chosen file. The
    // ones devy needs are set below.
    for (name, _) in std::env::vars_os() {
        // Case-insensitively: Windows environment names are.
        let lossy = name.to_string_lossy();
        if lossy.len() >= 4 && lossy.is_char_boundary(4) && lossy[..4].eq_ignore_ascii_case("GIT_")
        {
            command.env_remove(&name);
        }
    }
    let output = command
        // An untrusted repository's own config must not run anything during the check:
        // an empty core.fsmonitor disables the hook on every git version (old versions
        // treat any other value, including `false`, as a command to run).
        .args([
            "-c",
            "core.fsmonitor=",
            "-c",
            "safe.bareRepository=explicit",
            "ls-files",
            "-z",
            "--",
        ])
        .args(pathspecs)
        .current_dir(root)
        .env("PATH", safe_path)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Untranslated messages, so "not a git repository" below is recognised.
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_LITERAL_PATHSPECS")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_DISCOVERY_ACROSS_FILESYSTEM")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("Failed to run {}", git.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let fatal = stderr
            .lines()
            .find(|l| l.starts_with("fatal:"))
            .unwrap_or_else(|| stderr.lines().next().unwrap_or(""))
            .trim();
        // A `.git` git does not recognise (an empty directory, a stray file) is no
        // repository: git tracks nothing there, as when there is no `.git` at all.
        if fatal.starts_with("fatal: not a git repository") {
            return Ok(None);
        }
        // git names the repository it refused; safe.directory must match that path, which
        // is the repository's top level, not necessarily the project root.
        let refused = stderr
            .split("repository at '")
            .nth(1)
            .and_then(|rest| rest.split('\'').next())
            .map(|p| crate::output::clean_line(p).into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let reason = crate::output::clean_line(fatal)
            .chars()
            .take(200)
            .collect::<String>();
        let advice = if fatal.contains("dubious ownership") || fatal.contains("unsafe repository") {
            format!(
                "if you trust this checkout, run git config --global --add safe.directory {refused}"
            )
        } else {
            "fix the repository (or use a newer git) and run devy again".to_string()
        };
        bail!(
            "could not check which files git tracks in {} (git ls-files failed{}); devy will not use .devy, .shadowenv.d or the virtualenv until it can — {advice}",
            root.display(),
            if reason.is_empty() {
                String::new()
            } else {
                format!(": {reason}")
            }
        );
    }
    Ok(Some(first_tracked(&output.stdout, rels)))
}

/// Maps `git ls-files -z` output to the first of `rels` that contains a listed file,
/// comparing path components case-insensitively (matching the icase pathspecs).
fn first_tracked(stdout: &[u8], rels: &[PathBuf]) -> Option<PathBuf> {
    let parts = |s: &str| -> Vec<String> {
        s.split(['/', '\\'])
            .filter(|p| !p.is_empty() && *p != ".")
            .map(str::to_lowercase)
            .collect()
    };
    stdout
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .find_map(|entry| {
            let file = parts(&String::from_utf8_lossy(entry));
            rels.iter()
                .find(|rel| {
                    let rel = parts(&rel.to_string_lossy());
                    !rel.is_empty() && file.starts_with(&rel)
                })
                .cloned()
        })
}

/// The `/nix/store` path `.devy/nix-profile` resolves to, or `None` when there is no
/// profile yet. Anything else at that path (a directory, a link elsewhere) is refused.
pub fn verified_nix_profile(root: &Path) -> Result<Option<PathBuf>> {
    verified_nix_profile_in(root, Path::new("/nix/store"))
}

fn verified_nix_profile_in(root: &Path, store: &Path) -> Result<Option<PathBuf>> {
    let link = root.join(NIX_PROFILE);
    let refuse = || {
        anyhow!(
            "{NIX_PROFILE} does not link into {}; devy will not use it — remove it from the repository",
            store.display()
        )
    };
    match fs::symlink_metadata(&link) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("Failed to inspect {}", link.display())),
        Ok(meta) if !meta.file_type().is_symlink() => return Err(refuse()),
        Ok(_) => {}
    }
    fs::read_link(&link).map_err(|_| refuse())?;
    let target = link.canonicalize().map_err(|_| refuse())?;
    let store = store.canonicalize().unwrap_or_else(|_| store.to_path_buf());
    if target.starts_with(&store) && target != store {
        Ok(Some(target))
    } else {
        Err(refuse())
    }
}

// ── Executable resolution ─────────────────────────────────────────────────────

#[cfg(not(test))]
static PROJECT_ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);

// Per thread under test: the harness runs tests on parallel threads, and a test that
// locates a `devy.yml` in its temp directory must not change which PATH entries every
// other test's executable lookups skip. A consequence: a unit test that records a root
// and then resolves executables on a thread it spawns sees no recorded root there (the
// lookup falls back to the current directory); no test does that today.
#[cfg(test)]
thread_local! {
    static PROJECT_ROOT: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Records the project root whose PATH entries executable lookups ignore. Set when
/// `devy.yml` is located; before that the current directory stands in for it.
pub fn set_project_root(root: &Path) {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    #[cfg(not(test))]
    if let Ok(mut slot) = PROJECT_ROOT.write() {
        *slot = Some(root);
    }
    #[cfg(test)]
    PROJECT_ROOT.with(|slot| *slot.borrow_mut() = Some(root));
}

fn recorded_project_root() -> Option<PathBuf> {
    #[cfg(not(test))]
    {
        PROJECT_ROOT.read().ok().and_then(|slot| slot.clone())
    }
    #[cfg(test)]
    {
        PROJECT_ROOT.with(|slot| slot.borrow().clone())
    }
}

fn project_root() -> Option<PathBuf> {
    recorded_project_root().or_else(|| std::env::current_dir().ok())
}

/// Finds `name` on PATH, skipping relative entries and every entry inside the project
/// root, so a binary a repository commits (or an activated project environment puts
/// first on PATH) is never run in place of the system one.
pub fn which_outside_project(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    which_outside_project_in(name, &path, project_root().as_deref())
}

/// Whether `path` is relative or inside the project root (see `which_outside_project`):
/// a place devy must not take an executable from.
pub fn is_project_local(path: &Path) -> bool {
    if !path.is_absolute() {
        return true;
    }
    project_root().is_some_and(|root| {
        let canonical = root.canonicalize().ok();
        is_inside(path, &root, canonical.as_deref())
    })
}

/// `which_outside_project`, then the project's nix profile `bin` once it passes the
/// `/nix/store` check: the only project-local place devy runs a tool from (e.g. a
/// `shadowenv` installed into the profile).
pub fn which_with_project_profile(name: &str) -> Option<PathBuf> {
    which_outside_project(name).or_else(|| {
        let root = project_root()?;
        let profile = verified_nix_profile(&root).ok()??;
        executable_in(&profile.join("bin"), name)
    })
}

/// `which_outside_project` with an explicit PATH value and project root.
pub fn which_outside_project_in(
    name: &str,
    path_var: &OsStr,
    root: Option<&Path>,
) -> Option<PathBuf> {
    path_entries_outside(path_var, root).find_map(|entry| executable_in(&entry, name))
}

/// The absolute PATH entries of `path_var` that are not inside `root`.
fn path_entries_outside<'a>(
    path_var: &'a OsStr,
    root: Option<&Path>,
) -> impl Iterator<Item = PathBuf> + 'a {
    let root = root.map(|r| (r.to_path_buf(), r.canonicalize().ok()));
    std::env::split_paths(path_var)
        .filter(|entry| entry.is_absolute())
        .filter(move |entry| match &root {
            Some((raw, canonical)) => !is_inside(entry, raw, canonical.as_deref()),
            None => true,
        })
}

/// PATH without relative entries or entries inside the project root: the PATH to hand a
/// program devy runs on the user's behalf (such as a downloaded installer), so that the
/// commands it runs by name are never taken from the repository.
pub fn path_outside_project() -> OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    path_outside_project_in(&path, project_root().as_deref())
}

/// Whether `dir` is an absolute path outside the project root.
pub fn dir_outside_project(dir: &OsStr) -> bool {
    let dir = Path::new(dir);
    dir.is_absolute()
        && path_entries_outside(dir.as_os_str(), project_root().as_deref())
            .next()
            .is_some_and(|entry| entry == dir)
}

/// `path_outside_project` with an explicit PATH value and project root.
pub fn path_outside_project_in(path_var: &OsStr, root: Option<&Path>) -> OsString {
    std::env::join_paths(path_entries_outside(path_var, root)).unwrap_or_default()
}

fn is_inside(entry: &Path, root: &Path, canonical_root: Option<&Path>) -> bool {
    let canonical_entry = canonicalize_existing_prefix(entry);
    let candidates = [Some(entry), canonical_entry.as_deref()];
    let roots = [Some(root), canonical_root];
    candidates
        .iter()
        .flatten()
        .any(|e| roots.iter().flatten().any(|r| e.starts_with(r)))
}

/// `path` canonicalized through its deepest existing ancestor, with the components
/// that don't exist yet appended unchanged. So a path that doesn't exist still compares
/// in the same form as a canonical root: through `/var` → `/private/var` on macOS, or
/// an 8.3 short name and the `\\?\` prefix on Windows. `None` if no ancestor resolves.
fn canonicalize_existing_prefix(path: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut base = path;
    loop {
        if let Ok(canonical) = base.canonicalize() {
            return Some(rest.iter().rev().fold(canonical, |p, c| p.join(c)));
        }
        rest.push(base.file_name()?.to_os_string());
        base = base.parent()?;
    }
}

fn executable_in(dir: &Path, name: &str) -> Option<PathBuf> {
    which::which_in(name, Some(dir), dir).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tmp_dir;

    #[test]
    fn missing_path_under_a_canonical_root_is_inside() {
        // The root is recorded canonical (`/private/var/…` on macOS, `\\?\C:\…` with long
        // names on Windows) while a path under it may be written another way and not
        // exist yet; it must still count as inside.
        let root = tmp_dir();
        let canonical = root.canonicalize().unwrap();
        let missing = root.join("homebrew").join("bin").join("brew");
        assert!(is_inside(&missing, &canonical, Some(&canonical)));
        let outside = std::env::temp_dir().join("devy-elsewhere").join("brew");
        assert!(!is_inside(&outside, &canonical, Some(&canonical)));
    }

    #[test]
    fn path_outside_project_drops_relative_and_project_entries() {
        let root = tmp_dir();
        let inside = root.join("bin");
        let outside = std::env::temp_dir();
        let path = std::env::join_paths([
            inside.clone(),
            PathBuf::from("relative/bin"),
            outside.clone(),
        ])
        .unwrap();
        let cleaned = path_outside_project_in(&path, Some(&root));
        assert_eq!(
            std::env::split_paths(&cleaned).collect::<Vec<_>>(),
            vec![outside]
        );
    }

    #[cfg(unix)]
    fn symlink(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    // ── write_atomic ──────────────────────────────────────────────────────────

    #[test]
    fn write_atomic_writes_and_replaces_without_leftovers() {
        let dir = tmp_dir();
        let path = dir.join("devy.lock");
        write_atomic(&path, b"one", 0o644).unwrap();
        write_atomic(&path, b"two", 0o644).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = fs::read_dir(&*dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from("devy.lock")]);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_sets_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir();
        let path = dir.join("unit");
        write_atomic(&path, b"x", 0o600).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_refuses_symlinked_destination() {
        let dir = tmp_dir();
        let outside = tmp_dir();
        let victim = outside.join("zshrc");
        fs::write(&victim, "precious").unwrap();
        let path = dir.join(".devy_bun_stamp");
        symlink(&victim, &path);
        let err = write_atomic(&path, b"123", 0o644).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "refusing to write {}: it is a symbolic link",
                path.display()
            )
        );
        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_ignores_planted_temp_name_symlinks() {
        let dir = tmp_dir();
        let outside = tmp_dir();
        let victim = outside.join("victim");
        fs::write(&victim, "precious").unwrap();
        for n in 0..200 {
            symlink(&victim, &dir.join(format!("devy.lock.{n}.tmp")));
            symlink(&victim, &dir.join(format!(".devy.lock.{n}.tmp")));
        }
        symlink(
            &victim,
            &dir.join(format!("devy.lock.{}.tmp", std::process::id())),
        );
        write_atomic(&dir.join("devy.lock"), b"lock", 0o644).unwrap();
        assert_eq!(fs::read_to_string(dir.join("devy.lock")).unwrap(), "lock");
        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
    }

    #[cfg(unix)]
    #[test]
    fn create_temp_does_not_follow_a_symlink_at_the_chosen_name() {
        let dir = tmp_dir();
        let outside = tmp_dir();
        let victim = outside.join("victim");
        let link = dir.join("link");
        symlink(&victim, &link);
        assert!(create_new_nofollow(&link).is_err());
        assert!(!victim.exists(), "the link target must not be created");
    }

    // ── ensure_devy_dir ───────────────────────────────────────────────────────

    #[test]
    fn ensure_devy_dir_creates_the_gitignore_once() {
        let root = tmp_dir();
        let devy = ensure_devy_dir(&root).unwrap();
        assert_eq!(devy, root.join(".devy"));
        let ignore = devy.join(".gitignore");
        assert_eq!(fs::read_to_string(&ignore).unwrap(), "*\n");
        // A second call leaves the file as it is, even after the user edits it.
        fs::write(&ignore, "*\n!keep\n").unwrap();
        ensure_devy_dir(&root).unwrap();
        assert_eq!(fs::read_to_string(&ignore).unwrap(), "*\n!keep\n");
    }

    #[test]
    fn ensure_devy_dir_keeps_existing_content() {
        let root = tmp_dir();
        fs::create_dir(root.join(".devy")).unwrap();
        fs::write(root.join(".devy/.gitignore"), "custom\n").unwrap();
        ensure_devy_dir(&root).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".devy/.gitignore")).unwrap(),
            "custom\n"
        );
    }

    #[test]
    fn ensure_devy_dir_adds_the_gitignore_to_an_existing_dir() {
        let root = tmp_dir();
        fs::create_dir_all(root.join(".devy/data")).unwrap();
        ensure_devy_dir(&root).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".devy/.gitignore")).unwrap(),
            "*\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_devy_dir_does_not_follow_a_gitignore_symlink() {
        let root = tmp_dir();
        let outside = tmp_dir();
        let victim = outside.join("victim");
        fs::create_dir(root.join(".devy")).unwrap();
        symlink(&victim, &root.join(".devy/.gitignore"));
        ensure_devy_dir(&root).unwrap();
        assert!(!victim.exists(), "the link target must not be created");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_devy_dir_refuses_a_symlinked_devy_dir() {
        let root = tmp_dir();
        let outside = tmp_dir();
        symlink(&outside, &root.join(".devy"));
        let err = ensure_devy_dir(&root).unwrap_err().to_string();
        assert!(err.contains("it is a symbolic link"), "{err}");
        assert!(!outside.join(".gitignore").exists());
    }

    #[test]
    fn ensure_devy_subdir_creates_the_dir_and_the_gitignore() {
        let root = tmp_dir();
        let data = root.join(".devy/data/redis");
        ensure_devy_subdir(&root, &data).unwrap();
        assert!(data.is_dir());
        assert!(root.join(".devy/.gitignore").is_file());
        let err = ensure_devy_subdir(&root, &root.join("elsewhere"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("is outside"), "{err}");
    }

    // ── open_lock_file / ensure_dir_in ────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn open_lock_file_refuses_symlink() {
        let dir = tmp_dir();
        let outside = tmp_dir();
        let victim = outside.join("victim");
        let path = dir.join(".devy-lock");
        symlink(&victim, &path);
        let err = open_lock_file(&path).unwrap_err().to_string();
        assert!(err.contains("it is a symbolic link"), "{err}");
        assert!(!victim.exists());
    }

    #[test]
    fn open_lock_file_creates_and_reopens_the_same_file() {
        let dir = tmp_dir();
        let path = dir.join(".devy-lock");
        open_lock_file(&path).unwrap();
        open_lock_file(&path).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn ensure_dir_in_creates_nested_dirs() {
        let root = tmp_dir();
        let dir = root.join(".devy").join("data").join("redis");
        ensure_dir_in(&root, &dir).unwrap();
        assert!(dir.is_dir());
        ensure_dir_in(&root, &dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dir_in_refuses_symlinked_component() {
        let root = tmp_dir();
        let outside = tmp_dir();
        fs::create_dir(root.join(".devy")).unwrap();
        symlink(&outside, &root.join(".devy").join("data"));
        let err = ensure_dir_in(&root, &root.join(".devy/data/redis"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("it is a symbolic link"), "{err}");
        assert!(!outside.join("redis").exists());
    }

    #[test]
    fn ensure_dir_in_rejects_a_file_component() {
        let root = tmp_dir();
        fs::write(root.join(".devy"), "x").unwrap();
        let err = ensure_dir_in(&root, &root.join(".devy/data"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("it is not a directory"), "{err}");
    }

    #[test]
    fn ensure_dir_in_rejects_paths_outside_root() {
        let root = tmp_dir();
        let other = tmp_dir();
        assert!(ensure_dir_in(&root, &other).is_err());
    }

    // ── private_dir ───────────────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn private_dir_creates_0700_and_reuses_it() {
        use std::os::unix::fs::PermissionsExt;
        let base = tmp_dir();
        let dir = private_dir(&base, "sock", false).unwrap();
        assert_eq!(dir, base.join("sock"));
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(private_dir(&base, "sock", false).unwrap(), dir);
    }

    #[test]
    fn private_dir_random_names_differ() {
        let base = tmp_dir();
        let a = private_dir(&base, "ai", true).unwrap();
        let b = private_dir(&base, "ai", true).unwrap();
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_rejects_foreign_owner() {
        let base = tmp_dir();
        private_dir(&base, "sock", false).unwrap();
        let err = private_dir_as(&base, "sock", false, current_uid() + 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not owned by the current user"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_rejects_wrong_mode() {
        use std::os::unix::fs::PermissionsExt;
        let base = tmp_dir();
        let dir = base.join("sock");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        let err = private_dir(&base, "sock", false).unwrap_err().to_string();
        assert!(err.contains("has mode 777"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_rejects_symlink() {
        let base = tmp_dir();
        let outside = tmp_dir();
        symlink(&outside, &base.join("sock"));
        let err = private_dir(&base, "sock", false).unwrap_err().to_string();
        assert!(err.contains("not a directory owned"), "{err}");
    }

    /// The read-only check never creates the directory, and refuses one that
    /// `private_dir` would refuse.
    #[test]
    fn existing_private_dir_checks_without_creating() {
        let base = tmp_dir();
        let dir = base.join("logs");
        existing_private_dir(&dir).unwrap();
        assert!(!dir.exists(), "nothing is created");
        private_dir(&base, "logs", false).unwrap();
        existing_private_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let err = with_fake_owner(current_uid() + 1, || existing_private_dir(&dir))
                .unwrap_err()
                .to_string();
            assert!(err.contains("not owned by the current user"), "{err}");
            for mode in [0o777, 0o770, 0o755] {
                fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
                let err = existing_private_dir(&dir).unwrap_err().to_string();
                assert!(err.contains(&format!("has mode {mode:03o}")), "{err}");
            }
            fs::remove_dir(&dir).unwrap();
            let outside = tmp_dir();
            symlink(&outside, &dir);
            let err = existing_private_dir(&dir).unwrap_err().to_string();
            assert!(err.contains("not a directory owned"), "{err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn open_regular_nofollow_refuses_links_and_special_files() {
        let dir = tmp_dir();
        let real = dir.join("real.log");
        fs::write(&real, "x\n").unwrap();
        assert!(open_regular_nofollow(&real).is_ok());
        symlink(&real, &dir.join("link.log"));
        let err = open_regular_nofollow(&dir.join("link.log")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("is a symlink"), "{err}");
        let err = open_regular_nofollow(&dir).unwrap_err();
        assert!(err.to_string().contains("is not a regular file"), "{err}");
        let missing = open_regular_nofollow(&dir.join("missing.log")).unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn read_following_placeholders_reads_regular_files_only() {
        let dir = tmp_dir();
        let real = dir.join("config");
        fs::write(&real, "[core]\n").unwrap();
        assert_eq!(
            read_regular_capped_following_placeholders(&real, 64).unwrap(),
            b"[core]\n"
        );
        let err = read_regular_capped_following_placeholders(&real, 3).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("larger than 3 bytes"), "{err}");
        let err = read_regular_capped_following_placeholders(&dir, 64).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("is not a regular file"), "{err}");
        let missing = read_regular_capped_following_placeholders(&dir.join("missing"), 64);
        assert_eq!(missing.unwrap_err().kind(), ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn read_following_placeholders_refuses_symlinks() {
        let dir = tmp_dir();
        let real = dir.join("real");
        fs::write(&real, "x\n").unwrap();
        symlink(&real, &dir.join("link"));
        let err = read_regular_capped_following_placeholders(&dir.join("link"), 64).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("is a symlink"), "{err}");
    }

    #[cfg(windows)]
    #[test]
    fn same_file_windows_matches_one_file_and_not_another() {
        let dir = tmp_dir();
        let a = dir.join("a");
        let b = dir.join("b");
        fs::write(&a, "same\n").unwrap();
        let opened = File::open(&a).unwrap().metadata().unwrap();
        assert!(same_file_windows(
            &fs::symlink_metadata(&a).unwrap(),
            &opened
        ));
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&b, "same\n").unwrap();
        assert!(!same_file_windows(
            &fs::symlink_metadata(&b).unwrap(),
            &opened
        ));
    }

    #[test]
    fn private_temp_dir_is_removed_on_drop() {
        let path = {
            let dir = PrivateTempDir::new("devy-test").unwrap();
            assert!(dir.path().is_dir());
            dir.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    // ── managed paths ─────────────────────────────────────────────────────────

    #[test]
    fn managed_paths_absent_or_real_dirs_pass() {
        let root = tmp_dir();
        check_managed_paths(&root, &[PathBuf::from(".venv")]).unwrap();
        fs::create_dir_all(root.join(".devy/data")).unwrap();
        fs::create_dir_all(root.join(".venv/bin")).unwrap();
        check_managed_paths(&root, &[PathBuf::from(".venv")]).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn managed_symlinked_dir_is_refused() {
        let root = tmp_dir();
        let outside = tmp_dir();
        // The directory it points to (another project's, say) keeps its signature: the
        // untrust that follows a refusal never follows the link.
        fs::write(outside.join(".trust-a46f63ff"), "sig").unwrap();
        symlink(&outside, &root.join(".shadowenv.d"));
        let err = check_managed_paths(&root, &[]).unwrap_err().to_string();
        assert_eq!(
            err,
            ".shadowenv.d is a symbolic link; devy will not use it — remove it from the repository"
        );
        assert_eq!(fs::read(outside.join(".trust-a46f63ff")).unwrap(), b"sig");
    }

    /// Every caller (`up`, `exec`, service start) gets the untrust, not just `up`.
    #[cfg(unix)]
    #[test]
    fn refused_managed_path_removes_shadowenv_trust() {
        let root = tmp_dir();
        fs::create_dir(root.join(".shadowenv.d")).unwrap();
        let sig = root.join(".shadowenv.d/.trust-a46f63ff");
        fs::write(&sig, "sig").unwrap();
        check_managed_paths(&root, &[]).unwrap();
        assert!(sig.exists(), "a passing check keeps it");
        let outside = tmp_dir();
        symlink(&outside, &root.join(".devy"));
        assert!(check_managed_paths(&root, &[]).is_err());
        assert!(!sig.exists());
    }

    #[test]
    fn managed_dir_with_foreign_owner_is_refused() {
        let root = tmp_dir();
        fs::create_dir(root.join(".devy")).unwrap();
        let err = check_managed_dir(&root, Path::new(".devy"), |_| false)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(".devy is not owned by the current user"),
            "{err}"
        );
    }

    #[test]
    fn managed_file_instead_of_dir_is_refused() {
        let root = tmp_dir();
        fs::write(root.join(".devy"), "x").unwrap();
        let err = check_managed_paths(&root, &[]).unwrap_err().to_string();
        assert!(err.contains(".devy is not a directory"), "{err}");
    }

    #[test]
    fn first_tracked_maps_files_to_their_managed_dir() {
        let rels = [PathBuf::from(".devy"), PathBuf::from(".venv")];
        assert_eq!(
            first_tracked(b".venv/bin/git\0", &rels),
            Some(PathBuf::from(".venv"))
        );
        assert_eq!(first_tracked(b"", &rels), None);
        assert_eq!(first_tracked(b".devyx/file\0", &rels), None);
        // Case-insensitive filesystems: `.Venv` is `.venv`.
        assert_eq!(
            first_tracked(b".Venv/bin/pip\0", &rels),
            Some(PathBuf::from(".venv"))
        );
        let nested = [PathBuf::from("envs/py")];
        assert_eq!(
            first_tracked(b"Envs/Py/bin/pip\0", &nested),
            Some(PathBuf::from("envs/py"))
        );
    }

    /// A committed `.Devy` (different case) is still caught on case-insensitive
    /// filesystems, where it is the `.devy` devy would use.
    #[test]
    fn git_tracked_matches_case_insensitively() {
        let root = tmp_dir();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&*root)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !git(&["init", "-q"]) {
            return; // git unavailable
        }
        fs::create_dir(root.join(".Devy")).unwrap();
        fs::write(root.join(".Devy/state"), "x").unwrap();
        assert!(git(&["add", "--", ".Devy/state"]));
        assert_eq!(
            git_tracked(&root, &[PathBuf::from(".devy")]).unwrap(),
            Some(PathBuf::from(".devy"))
        );
    }

    /// A repository git cannot read is refused, not skipped; a `.git` git does not
    /// recognise as a repository at all (an empty directory) is like none.
    #[test]
    fn git_tracked_fails_closed_when_git_cannot_list_the_index() {
        let root = tmp_dir();
        fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(git_tracked(&root, &[PathBuf::from(".devy")]).unwrap(), None);
        fs::remove_dir(root.join(".git")).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&*root)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !git(&["init", "-q"]) {
            return; // git unavailable
        }
        fs::write(root.join(".git/index"), "not an index").unwrap();
        let err = check_managed_paths(&root, &[]).unwrap_err().to_string();
        assert!(
            err.contains("could not check which files git tracks"),
            "{err}"
        );
    }

    #[test]
    fn managed_path_in_a_nested_repository_is_checked_there() {
        let root = tmp_dir();
        // A managed directory that is itself a repository (`.git` is a file in a
        // submodule) is refused.
        fs::create_dir(root.join(".venv")).unwrap();
        fs::write(root.join(".venv/.git"), "gitdir: ../.git/modules/venv\n").unwrap();
        let err = check_managed_paths(&root, &[PathBuf::from(".venv")])
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with(".venv is a git repository of its own"),
            "{err}"
        );
        check_managed_paths(&root, &[]).unwrap();

        // A venv inside a submodule: allowed when the submodule tracks nothing there,
        // refused when it does.
        let git = |dir: &Path, args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        let svc = root.join("svc");
        fs::create_dir(&svc).unwrap();
        if !git(&svc, &["init", "-q"]) {
            return; // git unavailable
        }
        let venv = [PathBuf::from("svc/.venv")];
        check_managed_paths(&root, &venv).unwrap();
        fs::create_dir_all(svc.join(".venv/bin")).unwrap();
        fs::write(svc.join(".venv/bin/sudo"), "x").unwrap();
        assert!(git(&svc, &["add", "-f", "--", ".venv/bin/sudo"]));
        let err = check_managed_paths(&root, &venv).unwrap_err().to_string();
        assert!(err.starts_with("svc/.venv is tracked by git"), "{err}");
    }

    #[test]
    fn managed_dir_with_leading_curdir_is_checked() {
        let root = tmp_dir();
        fs::write(root.join(".venv"), "x").unwrap();
        let err = check_managed_paths(&root, &[PathBuf::from("./.venv")])
            .unwrap_err()
            .to_string();
        assert!(err.contains(".venv is not a directory"), "{err}");
    }

    #[test]
    fn git_tracked_is_none_without_a_repository() {
        let root = tmp_dir();
        fs::create_dir(root.join(".venv")).unwrap();
        // tmp dirs live outside any repository, so git is never consulted.
        if root.ancestors().any(|d| d.join(".git").exists()) {
            return;
        }
        assert_eq!(git_tracked(&root, &[PathBuf::from(".venv")]).unwrap(), None);
    }

    #[test]
    fn nix_profile_directory_is_refused() {
        let root = tmp_dir();
        fs::create_dir_all(root.join(".devy/nix-profile/bin")).unwrap();
        let err = verified_nix_profile(&root).unwrap_err().to_string();
        assert!(
            err.contains(".devy/nix-profile does not link into"),
            "{err}"
        );
    }

    #[test]
    fn nix_profile_absent_is_none() {
        let root = tmp_dir();
        assert_eq!(verified_nix_profile(&root).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn nix_profile_link_must_resolve_into_the_store() {
        let root = tmp_dir();
        let store = tmp_dir();
        let elsewhere = tmp_dir();
        fs::create_dir(root.join(".devy")).unwrap();
        let profile = store.join("abc-profile");
        fs::create_dir(&profile).unwrap();
        symlink(&profile, &root.join(NIX_PROFILE));
        assert_eq!(
            verified_nix_profile_in(&root, &store).unwrap(),
            Some(profile.canonicalize().unwrap())
        );
        fs::remove_file(root.join(NIX_PROFILE)).unwrap();
        symlink(&elsewhere, &root.join(NIX_PROFILE));
        assert!(verified_nix_profile_in(&root, &store).is_err());
    }

    // ── which_outside_project ─────────────────────────────────────────────────

    #[cfg(unix)]
    fn fake_exe(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn path_of(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    /// Scenario "Planted sudo": a project `bin/` first on PATH never supplies `sudo`.
    #[cfg(unix)]
    #[test]
    fn planted_sudo_in_project_bin_is_skipped() {
        let root = tmp_dir();
        let system = tmp_dir();
        fake_exe(&root.join("bin"), "sudo");
        let real = fake_exe(&system, "sudo");
        let path = path_of(&[&root.join("bin"), &system]);
        assert_eq!(
            which_outside_project_in("sudo", &path, Some(&root)),
            Some(real)
        );
        let only_project = path_of(&[&root.join("bin")]);
        assert_eq!(
            which_outside_project_in("sudo", &only_project, Some(&root)),
            None
        );
    }

    /// Scenario "Project-local nix ignored": the project profile's `nix` loses to the system one.
    #[cfg(unix)]
    #[test]
    fn project_local_nix_is_ignored() {
        let root = tmp_dir();
        let system = tmp_dir();
        let profile_bin = root.join(".devy/nix-profile/bin");
        fake_exe(&profile_bin, "nix");
        let real = fake_exe(&system, "nix");
        let path = path_of(&[&profile_bin, &system]);
        assert_eq!(
            which_outside_project_in("nix", &path, Some(&root)),
            Some(real)
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_path_entries_are_skipped() {
        let root = tmp_dir();
        fake_exe(&root.join("bin"), "tool");
        let path = OsString::from("bin:.");
        assert_eq!(which_outside_project_in("tool", &path, None), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_path_entry_into_project_is_skipped() {
        let root = tmp_dir();
        let links = tmp_dir();
        fake_exe(&root.join("bin"), "tool");
        symlink(&root.join("bin"), &links.join("bin"));
        let path = path_of(&[&links.join("bin")]);
        assert_eq!(which_outside_project_in("tool", &path, Some(&root)), None);
    }
}
