//! Linked git worktree detection, read straight from the `.git` layout on disk.
//!
//! devy never runs `git` here: every command resolves ports through this module, so it
//! must be cheap, and it must work when `git` itself comes from the project's nix
//! profile and isn't on PATH yet (design D4 of `add-worktree-environments`).
//!
//! Layout this reads:
//! - A main checkout has a `.git` directory, which is also the common directory.
//! - A linked worktree has a `.git` file reading `gitdir: <common>/worktrees/<id>`
//!   (absolute, or relative to the file's directory since git 2.48). That directory holds
//!   `commondir` (the path to `<common>`, usually `../..`), `HEAD`, and `gitdir` (the path
//!   back to the worktree's `.git` file).
//! - A submodule also has a `.git` file, but it points into `.git/modules/<name>`, which
//!   has no `commondir`, so it is not a linked worktree.
//!
//! The `.git` file and everything it leads to come from the checkout, which may be
//! untrusted. A layout only counts as a linked worktree when git's back-link
//! (`<common>/worktrees/<id>/gitdir`) names this checkout's `.git` file, and on Windows
//! no path read from it may reach a network share or device namespace unless the
//! project itself is on that share.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Upper bound on how much of any git metadata file is read. `.git`, `commondir` and
/// `gitdir` are one line; `config` is the only one that can grow, and a
/// repository-supplied file must not make every devy command read megabytes.
const MAX_GIT_FILE_BYTES: u64 = 1024 * 1024;

/// A project inside a linked git worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedWorktree {
    /// The repository's common git directory (`<main>/.git`, or the bare repository).
    pub common_dir: PathBuf,
    /// The worktree's top level: the directory holding its `.git` file.
    pub top_level: PathBuf,
    /// The main checkout, or `None` when the common directory is a bare repository.
    pub main_checkout: Option<PathBuf>,
    /// The main checkout joined with the project root's path relative to `top_level`
    /// (the same monorepo subdirectory in the main checkout), or `None` without a main
    /// checkout.
    pub main_project_root: Option<PathBuf>,
}

/// The nearest `.git` at or above a directory, resolved to its git directories.
struct GitLocation {
    /// The directory holding `.git`.
    top_level: PathBuf,
    /// The checkout's own git directory (`.git`, or the `gitdir:` target of a `.git` file).
    git_dir: PathBuf,
    /// Whether `.git` is a file rather than a directory.
    via_file: bool,
}

/// Detects whether `project_root` is inside a linked git worktree. Returns `None` for a
/// main checkout, a submodule, a directory outside any repository, or a `.git` file that
/// doesn't point at an existing `<common>/worktrees/<id>` directory whose `gitdir` points
/// back at it.
pub fn detect(project_root: &Path) -> Option<LinkedWorktree> {
    let project_root = canonical(project_root);
    let location = locate(&project_root)?;
    if !location.via_file {
        return None;
    }
    let common_dir = commondir_of(&location.git_dir, &project_root)?;
    // Only `<common>/worktrees/<id>` is a linked worktree's git directory; anything else
    // with a `commondir` file is a layout devy doesn't understand.
    if location.git_dir.parent() != Some(common_dir.join("worktrees").as_path())
        || location.git_dir.file_name().is_none()
    {
        return None;
    }
    // git's back-link must name this checkout's `.git` file. Without it, a `.git` file
    // could borrow any repository's worktree directory (and its main checkout's ports).
    let back_link = read_path_file(
        &location.git_dir.join("gitdir"),
        &location.git_dir,
        &project_root,
    )?;
    let dot_git = location.top_level.join(".git");
    if back_link.canonicalize().ok()? != dot_git.canonicalize().ok()? {
        return None;
    }
    let main_checkout = main_checkout_of(&common_dir);
    // The subdirectory's components in the main checkout come from that checkout, so on
    // Windows one of them could be a link to a share; devy then reads no main lock.
    let main_project_root = main_checkout
        .as_ref()
        .map(
            |main| match project_root.strip_prefix(&location.top_level) {
                Ok(rel) if !rel.as_os_str().is_empty() => main.join(rel),
                _ => main.clone(),
            },
        )
        .filter(|main_root| !has_link_component(main_root));
    Some(LinkedWorktree {
        common_dir,
        top_level: location.top_level,
        main_checkout,
        main_project_root,
    })
}

/// Finds the nearest `.git` at or above `start` (already canonical) and resolves it.
/// Stops at the first `.git` found, even when it can't be resolved, so an unreadable
/// `.git` file never makes devy adopt an outer repository.
fn locate(start: &Path) -> Option<GitLocation> {
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        // A `.git` that can't be examined stops the search rather than letting an outer
        // repository be adopted. On Windows a `.git` symlink or junction could lead the
        // checks below anywhere (see [`follow`]); devy doesn't treat it as a worktree.
        let is_link = match fs::symlink_metadata(&dot_git) {
            Ok(meta) => is_link_meta(&meta),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        if cfg!(windows) && is_link {
            return None;
        }
        // It exists, so a dangling symlink stops the search too.
        let Ok(meta) = fs::metadata(&dot_git) else {
            return None;
        };
        if meta.is_dir() {
            return Some(GitLocation {
                top_level: dir.to_path_buf(),
                git_dir: dot_git,
                via_file: false,
            });
        }
        if !meta.is_file() {
            return None;
        }
        let content = read_small(&dot_git)?;
        let target = content.lines().next()?.strip_prefix("gitdir:")?.trim();
        if target.is_empty() {
            return None;
        }
        let git_dir = follow(target, dir, start)?;
        if !git_dir.is_dir() {
            return None;
        }
        return Some(GitLocation {
            top_level: dir.to_path_buf(),
            git_dir: canonical(&git_dir),
            via_file: true,
        });
    }
    None
}

/// Resolves `<git_dir>/commondir` (relative paths against `git_dir`). `None` when the
/// file is missing or names no existing directory.
fn commondir_of(git_dir: &Path, project_root: &Path) -> Option<PathBuf> {
    let common = read_path_file(&git_dir.join("commondir"), git_dir, project_root)?;
    common.is_dir().then(|| canonical(&common))
}

/// The main checkout of the repository at `common_dir` (canonical): its parent, unless
/// the repository is bare. A `config` that exists but can't be read (too large, a
/// symlink) gives `None`: devy can't tell whether the parent is a checkout.
fn main_checkout_of(common_dir: &Path) -> Option<PathBuf> {
    let config = match read_git_file(&common_dir.join("config")) {
        Ok(config) => config,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return None,
    };
    if is_bare(&config) {
        return None;
    }
    common_dir.parent().map(Path::to_path_buf)
}

/// Whether git config text sets `core.bare` to true.
fn is_bare(config: &str) -> bool {
    let mut in_core = false;
    let mut bare = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // `[core]` only; `[core "sub"]` is a different section.
            in_core = line
                .strip_prefix('[')
                .and_then(|rest| rest.split(']').next())
                .is_some_and(|name| name.trim().eq_ignore_ascii_case("core"));
            continue;
        }
        if !in_core || line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((key, value)) => (key.trim(), Some(value.trim())),
            None => (line, None),
        };
        if key.eq_ignore_ascii_case("bare") {
            // A later assignment wins, as in git. A bare key means true.
            bare = value.is_none_or(|value| {
                let value = value
                    .split(['#', ';'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                ["true", "yes", "on", "1"]
                    .iter()
                    .any(|t| value.eq_ignore_ascii_case(t))
            });
        }
    }
    bare
}

/// Reads a one-line file holding a path, resolving a relative path against `base`
/// (canonical). `None` when the path is one devy won't follow from a checkout at
/// `project_root` (see [`follow`]).
fn read_path_file(file: &Path, base: &Path, project_root: &Path) -> Option<PathBuf> {
    let content = read_small(file)?;
    let line = content.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    follow(line, base, project_root)
}

/// `target` (read from git metadata) joined to `base`, when devy may touch it from a
/// checkout at `project_root`: its text passes [`may_follow`], and on Windows the joined
/// path, relative or absolute, passes through no symlink, junction or mount point
/// ([`has_link_component`]). Screening the text alone can't see where a link leads, and
/// following one could reach a network share.
fn follow(target: &str, base: &Path, project_root: &Path) -> Option<PathBuf> {
    if !may_follow(target, project_root) {
        return None;
    }
    let path = base.join(target);
    (!has_link_component(&path)).then_some(path)
}

/// On Windows, whether any component of `path` is a symlink, junction or mount point
/// ([`is_redirecting_link`]), any of which could lead to a network share; also true for
/// a path that isn't absolute (`\dir`, `C:dir`), whose starting point devy can't check.
/// Always false elsewhere, where looking a path up can't reach a share. Call it before any filesystem
/// access to a path derived from git metadata.
fn has_link_component(path: &Path) -> bool {
    cfg!(windows)
        && walk_finds_link(path, |p| {
            trimmed_by_windows(p) || windows_device_name(p) || is_redirecting_link(p)
        })
}

/// Whether the last component of `path` ends in a space or a period, which Win32 trims
/// from the last component of a path but not from the others: stat-ing `C:\x\d ` on
/// its own reaches `C:\x\d`, while opening `C:\x\d \HEAD` goes through `d `, so the
/// walk would check a different directory than the one used. git never writes such
/// names, so they count as links.
fn trimmed_by_windows(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_str().is_none_or(|n| n.ends_with([' ', '.'])))
}

/// Whether the last component of `path` is a DOS device name (`CON`, `NUL`, `COM1`,
/// `lpt1.txt`, `CONIN$`, …), which Win32 can open as `\\.\<name>` wherever it appears
/// last. Every prefix of the path is stat-ed as a last component, so these count as
/// links; git never writes such names.
fn windows_device_name(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    let Some(name) = name.to_str() else {
        return true;
    };
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_lowercase();
    match stem.as_str() {
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$" => true,
        // `com0`/`lpt0` aren't devices, but refusing them too costs nothing.
        s => s
            .strip_prefix("com")
            .or_else(|| s.strip_prefix("lpt"))
            .is_some_and(|rest| {
                let mut c = rest.chars();
                matches!(
                    (c.next(), c.next()),
                    (Some('0'..='9' | '¹' | '²' | '³'), None)
                )
            }),
    }
}

/// Whether `is_link` holds for any component of `path`, checked from the root down. A
/// drive or share prefix and the root (`C:\`, `\\?\C:\`, `\\server\share\`) are not
/// checked; any other prefix (`\\?\` without a drive, `\\.\`), whose components Rust
/// doesn't split, counts as a link. `..` steps back lexically, which is sound because
/// every component already walked was checked not to be a link. A missing component is
/// not a link, and the walk goes on past it so a later `..` can't skip a check. A path
/// that isn't absolute counts as crossing a link.
fn walk_finds_link(path: &Path, is_link: impl Fn(&Path) -> bool) -> bool {
    use std::path::{Component, Prefix};
    if !path.is_absolute() {
        return true;
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => match prefix.kind() {
                // Rust reads `//?/C:` (or `\\?/C:`) as a share on host `?`; Win32 treats
                // it as a device path, so it is refused (`.` too, defensively).
                Prefix::UNC(server, _) if matches!(server.as_encoded_bytes(), b"?" | b".") => {
                    return true;
                }
                Prefix::Disk(_)
                | Prefix::VerbatimDisk(_)
                | Prefix::UNC(..)
                | Prefix::VerbatimUNC(..) => current.push(component.as_os_str()),
                Prefix::Verbatim(_) | Prefix::DeviceNS(_) => return true,
            },
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // Git never writes `..` above the root, and where Win32 puts the root
                // depends on the kind of path.
                if !current.pop() {
                    return true;
                }
            }
            Component::Normal(part) => {
                // Under a verbatim prefix `/` isn't a separator, so `\\?\C:/x/../l` is one
                // "name" here that hides its own components; Windows names never contain
                // `/`. A `:` names an NTFS stream (`dir::$INDEX_ALLOCATION` is `dir`), which
                // git never writes. Refuse both rather than walk them.
                if cfg!(windows)
                    && part
                        .as_encoded_bytes()
                        .iter()
                        .any(|b| *b == b'/' || *b == b':')
                {
                    return true;
                }
                current.push(part);
                if is_link(&current) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether `path` itself (not what it points to) is a link that redirects the path: a
/// symlink, or on Windows a junction or mount point (see [`is_link_meta`]). False when
/// it doesn't exist (or a component above it isn't a directory); true when it can't be
/// examined, since an unreadable entry could still be traversed.
fn is_redirecting_link(path: &Path) -> bool {
    use std::io::ErrorKind;
    match fs::symlink_metadata(path) {
        Ok(meta) => is_link_meta(&meta),
        Err(e) => !matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory),
    }
}

/// [`is_redirecting_link`] for metadata already read with `symlink_metadata`.
///
/// On Windows, std reads the entry's reparse tag (`FileAttributeTagInfo`, or
/// `dwReserved0` from `FindFirstFileExW`) and reports `is_symlink` only for
/// name-surrogate tags: `IO_REPARSE_TAG_SYMLINK` (`0xA000000C`),
/// `IO_REPARSE_TAG_MOUNT_POINT` (`0xA0000003`, junctions and volume mount points), and
/// the few other tags that likewise stand for another named entity. Those are the
/// reparse points that can send a lookup somewhere else, such as a network share. Other
/// reparse points (OneDrive / Cloud Files placeholders, dedup, WOF compression) leave
/// the path where it is, so they are not links, and a repository in such a folder is
/// still detected as a worktree. DFS link folders (`IO_REPARSE_TAG_DFS`) are not name
/// surrogates either; they only redirect on a DFS namespace server's own disk, and an
/// SMB client resolves them before devy sees the path, so a project on a DFS share is
/// trusted as a whole like any share (see [`may_follow`]). If std can't read the tag,
/// `symlink_metadata` fails and the caller fails closed.
fn is_link_meta(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Whether a path read from git metadata in the checkout at `project_root` may be
/// touched at all. On Windows, merely stat-ing a UNC path makes the system connect to
/// that host (leaking the user's NTLM credentials to it), and device-namespace paths
/// reach raw devices, so neither is followed unless the project itself is on that same
/// share. Other platforms have no such paths.
fn may_follow(target: &str, project_root: &Path) -> bool {
    !cfg!(windows) || windows_target_allowed(target, &project_root.to_string_lossy())
}

/// Where a Windows path leads, judged from its text alone.
#[derive(Debug, PartialEq, Eq)]
enum WindowsOrigin {
    /// A drive path, `\\?\C:\…`, or a relative path.
    Local,
    /// `\\host\share\…` or `\\?\UNC\host\share\…`: host and share, lowercased.
    Share(String, String),
    /// `\\.\…`, a `\\?\` path that isn't a drive or UNC, an NT `\??\` path, or a
    /// malformed UNC path.
    Device,
}

/// Classifies `path` by its prefix as Windows reads it (either slash, any case).
fn windows_origin(path: &str) -> WindowsOrigin {
    let raw = path.to_ascii_lowercase();
    let path = raw.replace('/', "\\");
    // `\??\` is the NT object namespace, which Win32 passes through unchanged. Only the
    // backslash spellings are verbatim: `//?/` or `\\?/` are device paths, whose `..`
    // Win32 resolves above the drive (`//?/C:/../UNC/host/…` reaches a share).
    let verbatim = ["\\\\?\\", "\\??\\"]
        .iter()
        .find_map(|prefix| raw.strip_prefix(prefix));
    if let Some(rest) = verbatim {
        // In a verbatim path `/` is not a separator, so it can't be classified here.
        if rest.contains('/') {
            return WindowsOrigin::Device;
        }
        if let Some(unc) = rest.strip_prefix("unc\\") {
            return unc_share(unc);
        }
        let bytes = rest.as_bytes();
        let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        return if drive {
            WindowsOrigin::Local
        } else {
            WindowsOrigin::Device
        };
    }
    if ["\\\\?", "\\??", "\\\\."]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return WindowsOrigin::Device;
    }
    match path.strip_prefix("\\\\") {
        Some(unc) => unc_share(unc),
        None => WindowsOrigin::Local,
    }
}

/// `host\share[\…]` as a share, or `Device` when either part is missing.
fn unc_share(rest: &str) -> WindowsOrigin {
    let mut parts = rest.split('\\');
    match (parts.next(), parts.next()) {
        (Some(host), Some(share)) if !host.is_empty() && !share.is_empty() => {
            WindowsOrigin::Share(host.to_string(), share.to_string())
        }
        _ => WindowsOrigin::Device,
    }
}

/// Whether `target`, read from the checkout at `project_root`, may be followed on
/// Windows: a local path, or a path on the share the project itself is on.
fn windows_target_allowed(target: &str, project_root: &str) -> bool {
    match windows_origin(target) {
        WindowsOrigin::Local => true,
        WindowsOrigin::Device => false,
        share @ WindowsOrigin::Share(..) => windows_origin(project_root) == share,
    }
}

/// Reads a file of git metadata when it is a regular file of at most
/// `MAX_GIT_FILE_BYTES`. git never writes these as symlinks, so a symlink (which could
/// lead anywhere, including a network share on Windows), FIFO or other special file is
/// refused without being followed, as is a path through a link on Windows
/// ([`has_link_component`]).
fn read_small(path: &Path) -> Option<String> {
    read_git_file(path).ok()
}

/// [`read_small`], keeping the error: `NotFound` when the file is missing, another kind
/// when it exists but is refused.
fn read_git_file(path: &Path) -> std::io::Result<String> {
    if has_link_component(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path passes through a link",
        ));
    }
    // Not `read_regular_capped`: on Windows that opens a reparse point itself, which for
    // a repository in OneDrive or a deduplicated or WOF-compressed folder can return the
    // reparse stub instead of the file's content. Redirecting links are still refused.
    let bytes =
        crate::fs_safe::read_regular_capped_following_placeholders(path, MAX_GIT_FILE_BYTES)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `path` canonicalized when it exists, so `/var` and `/private/var` (macOS temp dirs)
/// and `..` segments compare equal; unchanged otherwise.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

// ── .devy/worktree.yml ────────────────────────────────────────────────────────

/// `.devy/worktree.yml`, relative to the project root: the ports `devy up` recorded for
/// this linked worktree, kept out of the committed `devy.lock` (design D5).
pub const PORTS_PATH: &str = ".devy/worktree.yml";

const PORTS_HEADER: &str = "# Generated by devy for this worktree only — do not edit manually\n";

/// The content of `.devy/worktree.yml`: canonical dependency name → port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreePorts {
    pub version: u32,
    #[serde(default)]
    pub ports: BTreeMap<String, u16>,
}

impl Default for WorktreePorts {
    fn default() -> Self {
        Self {
            version: 1,
            ports: BTreeMap::new(),
        }
    }
}

impl WorktreePorts {
    /// Loads `<project_root>/.devy/worktree.yml`. A missing file has no ports. An
    /// unreadable or unparseable one also counts as empty, with the warning
    /// `ignoring unreadable .devy/worktree.yml: <reason>`. Never writes anything.
    pub fn load(project_root: &Path) -> Self {
        match Self::read(project_root) {
            Ok(ports) => ports.unwrap_or_default(),
            Err(reason) => {
                crate::output::warn(&format!("ignoring unreadable {PORTS_PATH}: {reason}"));
                Self::default()
            }
        }
    }

    /// `None` when the file doesn't exist. The file is devy's own, so a symlinked
    /// `.devy/`, or a symlink or any other non-regular file at the path, is refused
    /// rather than followed. Reasons are fixed phrases: none of the file's text (which a
    /// symlink could have pointed at anything) ever reaches a message.
    fn read(project_root: &Path) -> Result<Option<Self>, &'static str> {
        match fs::symlink_metadata(project_root.join(crate::fs_safe::DEVY_DIR)) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(".devy is a symbolic link"),
            Ok(meta) if !meta.is_dir() => return Err(".devy is not a directory"),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(".devy could not be inspected"),
        }
        let bytes = match crate::fs_safe::read_regular_capped(
            &project_root.join(PORTS_PATH),
            crate::yaml_safe::MAX_YAML_BYTES as u64,
        ) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Err("it is not a regular file of at most 1 MiB");
            }
            Err(_) => return Err("it could not be read"),
        };
        let text = String::from_utf8(bytes).map_err(|_| "it is not UTF-8")?;
        let parsed: Self = crate::yaml_safe::from_str_strict(&text, PORTS_PATH)
            .map_err(|_| "it is not a valid worktree port file")?;
        if parsed.version != 1 {
            return Err("unsupported format version");
        }
        if parsed.ports.values().any(|port| *port == 0) {
            return Err("it records port 0");
        }
        Ok(Some(parsed))
    }

    /// The recorded port for canonical dependency `name`.
    pub fn get(&self, name: &str) -> Option<u16> {
        self.ports.get(name).copied()
    }

    /// The file's content as devy writes it.
    fn render(&self) -> anyhow::Result<String> {
        let body = serde_norway::to_string(self)?;
        Ok(format!("{PORTS_HEADER}{body}"))
    }

    /// Writes `<project_root>/.devy/worktree.yml` atomically (temp file and rename, never
    /// through a symlink), creating `.devy/` and its `.gitignore` when needed. Leaves the
    /// file untouched when its content wouldn't change, and doesn't create it just to
    /// record no ports (brew, apt, WinGet). Returns whether it wrote.
    pub fn write_if_changed(&self, project_root: &Path) -> anyhow::Result<bool> {
        use anyhow::Context;
        let content = self.render()?;
        let path = project_root.join(PORTS_PATH);
        if self.ports.is_empty()
            && fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(false);
        }
        let unchanged = crate::fs_safe::read_regular_capped(&path, content.len() as u64)
            .is_ok_and(|old| old == content.as_bytes());
        if unchanged {
            return Ok(false);
        }
        crate::fs_safe::ensure_devy_dir(project_root)?;
        crate::fs_safe::write_atomic(&path, content.as_bytes(), 0o644)
            .with_context(|| format!("Failed to write {PORTS_PATH}"))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tmp_dir;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// Creates a main checkout at `<root>/app`.
    fn main_checkout(root: &Path) -> PathBuf {
        let app = root.join("app");
        write(&app.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(
            &app.join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        );
        app
    }

    /// Adds a linked worktree `<root>/<id>` of the repository at `common`, laid out as
    /// `git worktree add` does, with an absolute `gitdir:` line and back-link.
    fn add_worktree(common: &Path, root: &Path, id: &str) -> PathBuf {
        let checkout = root.join(id);
        let admin = common.join("worktrees").join(id);
        write(&admin.join("commondir"), "../..\n");
        write(&admin.join("HEAD"), "ref: refs/heads/feat\n");
        write(
            &admin.join("gitdir"),
            &format!("{}\n", checkout.join(".git").display()),
        );
        write(
            &checkout.join(".git"),
            &format!("gitdir: {}\n", admin.display()),
        );
        checkout
    }

    #[test]
    fn main_checkout_is_not_a_worktree() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        assert_eq!(detect(&app), None);
    }

    #[test]
    fn directory_outside_a_repository_is_not_a_worktree() {
        let tmp = tmp_dir();
        assert_eq!(detect(&tmp), None);
    }

    #[test]
    fn linked_worktree_finds_its_main_checkout() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let feat = add_worktree(&app.join(".git"), &tmp, "app-feat");

        let wt = detect(&feat).expect("linked worktree");
        assert_eq!(wt.common_dir, canonical(&app.join(".git")));
        assert_eq!(wt.top_level, canonical(&feat));
        assert_eq!(wt.main_checkout, Some(canonical(&app)));
        assert_eq!(wt.main_project_root, Some(canonical(&app)));
    }

    #[test]
    fn relative_gitdir_resolves_against_the_git_file() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let feat = add_worktree(&app.join(".git"), &tmp, "app-feat");
        write(
            &feat.join(".git"),
            "gitdir: ../app/.git/worktrees/app-feat\n",
        );

        let wt = detect(&feat).expect("linked worktree");
        assert_eq!(wt.main_checkout, Some(canonical(&app)));
    }

    #[test]
    fn relative_back_link_resolves_against_the_worktree_git_dir() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let common = app.join(".git");
        let feat = add_worktree(&common, &tmp, "app-feat");
        // git 2.48+ with worktree.useRelativePaths writes this relative to the admin dir.
        write(
            &common.join("worktrees/app-feat/gitdir"),
            "../../../../app-feat/.git\n",
        );
        assert_eq!(detect(&feat).unwrap().top_level, canonical(&feat));
    }

    #[test]
    fn missing_or_mismatched_back_link_is_not_a_worktree() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let common = app.join(".git");
        let feat = add_worktree(&common, &tmp, "app-feat");
        let back_link = common.join("worktrees/app-feat/gitdir");

        // Another checkout's `.git` file, which exists.
        let other = add_worktree(&common, &tmp, "app-other");
        write(&back_link, &format!("{}\n", other.join(".git").display()));
        assert_eq!(detect(&feat), None);

        // A path that doesn't exist.
        write(
            &back_link,
            &format!("{}\n", tmp.join("gone/.git").display()),
        );
        assert_eq!(detect(&feat), None);

        // The checkout directory rather than its `.git` file.
        write(&back_link, &format!("{}\n", feat.display()));
        assert_eq!(detect(&feat), None);

        // Empty, then missing.
        write(&back_link, "\n");
        assert_eq!(detect(&feat), None);
        fs::remove_file(&back_link).unwrap();
        assert_eq!(detect(&feat), None);

        // Restored, it is a worktree again.
        write(&back_link, &format!("{}\n", feat.join(".git").display()));
        assert!(detect(&feat).is_some());
    }

    #[test]
    fn borrowed_worktree_dir_of_another_repository_is_not_a_worktree() {
        // A checkout whose `.git` file points at a real worktree of some other repository
        // must not pass for that worktree (or inherit its main checkout).
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        add_worktree(&app.join(".git"), &tmp, "app-feat");
        let intruder = tmp.join("intruder");
        write(
            &intruder.join(".git"),
            &format!(
                "gitdir: {}\n",
                app.join(".git/worktrees/app-feat").display()
            ),
        );
        assert_eq!(detect(&intruder), None);
    }

    #[test]
    fn commondir_is_read_rather_than_assumed() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let feat = add_worktree(&app.join(".git"), &tmp, "app-feat");
        let common = canonical(&app.join(".git"));
        write(
            &app.join(".git/worktrees/app-feat/commondir"),
            &format!("{}\n", common.display()),
        );
        assert_eq!(detect(&feat).unwrap().common_dir, common);

        // A commondir that doesn't lead back to the worktree's parent isn't a worktree.
        let elsewhere = tmp.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        write(
            &app.join(".git/worktrees/app-feat/commondir"),
            &format!("{}\n", elsewhere.display()),
        );
        assert_eq!(detect(&feat), None);
    }

    /// A `.git` that exists but leads nowhere stops the search; the outer repository is
    /// not adopted.
    #[cfg(unix)]
    #[test]
    fn dangling_dot_git_symlink_stops_the_search() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let feat = add_worktree(&app.join(".git"), &tmp, "app-feat");
        let inner = feat.join("inner");
        fs::create_dir(&inner).unwrap();
        assert!(detect(&inner).is_some());
        std::os::unix::fs::symlink(tmp.join("gone"), inner.join(".git")).unwrap();
        assert_eq!(detect(&inner), None);
    }

    #[test]
    fn missing_gitdir_target_is_not_a_worktree() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let feat = add_worktree(&app.join(".git"), &tmp, "app-feat");
        fs::remove_dir_all(app.join(".git/worktrees")).unwrap();
        assert_eq!(detect(&feat), None);
    }

    #[test]
    fn submodule_git_file_is_not_a_worktree() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        write(&app.join(".git/modules/lib/HEAD"), "ref: refs/heads/main\n");
        let lib = app.join("lib");
        write(&lib.join(".git"), "gitdir: ../.git/modules/lib\n");

        assert_eq!(detect(&lib), None);
    }

    #[test]
    fn worktree_of_a_bare_repository_has_no_main_checkout() {
        let tmp = tmp_dir();
        let bare = tmp.join("app.git");
        write(&bare.join("HEAD"), "ref: refs/heads/main\n");
        write(
            &bare.join("config"),
            "[core]\n\trepositoryformatversion = 0\n\tBare = true\n",
        );
        let feat = add_worktree(&bare, &tmp, "app-feat");

        let wt = detect(&feat).expect("linked worktree");
        assert_eq!(wt.common_dir, canonical(&bare));
        assert_eq!(wt.main_checkout, None);
        assert_eq!(wt.main_project_root, None);
    }

    #[test]
    fn subdirectory_project_maps_to_the_same_subdirectory_in_the_main_checkout() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let feat = add_worktree(&app.join(".git"), &tmp, "app-feat");
        let api = feat.join("services/api");
        fs::create_dir_all(&api).unwrap();

        let wt = detect(&api).expect("linked worktree");
        assert_eq!(wt.top_level, canonical(&feat));
        assert_eq!(
            wt.main_project_root,
            Some(canonical(&app).join("services").join("api"))
        );
    }

    #[test]
    fn bare_config_parsing() {
        let tmp = tmp_dir();
        let cases = [
            ("[core]\n\tbare = true\n", true),
            ("[core]\n\tbare\n", true),
            ("[CORE]\n\tbare = yes ; comment\n", true),
            ("[core]\n\tbare = false\n", false),
            ("[core]\n\tbare = true\n\tbare = false\n", false),
            ("[core \"x\"]\n\tbare = true\n", false),
            ("[remote \"origin\"]\n\tbare = true\n", false),
            ("", false),
        ];
        for (config, expected) in cases {
            assert_eq!(is_bare(config), expected, "config: {config:?}");
        }
        // A missing config is a non-bare repository; one that exists but can't be read
        // leaves the main checkout unknown.
        let common = tmp.join(".git");
        fs::create_dir(&common).unwrap();
        assert_eq!(main_checkout_of(&common), Some(tmp.to_path_buf()));
        let big = "#".repeat(MAX_GIT_FILE_BYTES as usize + 1);
        write(&common.join("config"), &big);
        assert_eq!(main_checkout_of(&common), None);
        write(&common.join("config"), "[core]\n\tbare = true\n");
        assert_eq!(main_checkout_of(&common), None);
        write(&common.join("config"), "[core]\n\tbare = false\n");
        assert_eq!(main_checkout_of(&common), Some(tmp.to_path_buf()));
    }

    // ── Windows path screening ────────────────────────────────────────────────

    #[test]
    fn windows_origin_classifies_prefixes() {
        use WindowsOrigin::{Device, Local, Share};
        let share = || Share("host".into(), "share".into());
        let cases = [
            (r"C:\src\app\.git\worktrees\a", Local),
            (r"..\app\.git\worktrees\a", Local),
            ("../app/.git/worktrees/a", Local),
            (r"\src\app", Local),
            (r"\\?\C:\src\app", Local),
            (r"\??\C:\src\app", Local),
            (r"\\host\share\app\.git", share()),
            ("//HOST/Share/app/.git", share()),
            (r"\\?\UNC\host\share\app", share()),
            // Only the backslash spellings are verbatim; the others are device paths.
            (r"//?/unc/HOST/share/app", Device),
            ("//?/C:/../UNC/host/share/x", Device),
            (r"\\?/C:/../UNC/host/share/x", Device),
            ("/??/C:/x", Device),
            (r"\\?\C:/x/../lnk", Device),
            (r"\??\UNC\host\share\app", share()),
            (r"\\.\C:\src", Device),
            (r"\\.\pipe\x", Device),
            ("//./PhysicalDrive0", Device),
            (r"\\?\GLOBALROOT\Device\HarddiskVolume1", Device),
            (r"\\?\Volume{0000}\x", Device),
            (r"\\.", Device),
            (r"\\?", Device),
            (r"\\host", Device),
            (r"\\host\", Device),
            (r"\\\share", Device),
        ];
        for (path, expected) in cases {
            assert_eq!(windows_origin(path), expected, "{path}");
        }
    }

    #[test]
    fn windows_targets_on_shares_and_devices_are_refused() {
        let local_root = r"\\?\C:\src\app-feat";
        assert!(windows_target_allowed(
            r"C:\src\app\.git\worktrees\a",
            local_root
        ));
        assert!(windows_target_allowed(r"..\app\.git", local_root));
        assert!(!windows_target_allowed(r"\\evil\share\x", local_root));
        assert!(!windows_target_allowed("//evil/share/x", local_root));
        assert!(!windows_target_allowed(r"\\?\UNC\evil\share\x", local_root));
        assert!(!windows_target_allowed(r"\\.\pipe\x", local_root));

        // A project on a share may follow paths on that same share, in either form.
        let share_root = r"\\?\UNC\fileserver\code\app-feat";
        assert!(windows_target_allowed(
            r"\\FileServer\Code\app\.git\worktrees\a",
            share_root
        ));
        assert!(windows_target_allowed(r"C:\src\app\.git", share_root));
        assert!(!windows_target_allowed(
            r"\\fileserver\other\app\.git",
            share_root
        ));
        assert!(!windows_target_allowed(r"\\evil\code\app\.git", share_root));
        assert!(!windows_target_allowed(r"\\.\pipe\x", share_root));
    }

    #[test]
    fn walk_finds_link_checks_every_component_of_the_path() {
        // Absolute on every platform: a drive prefix on Windows, `/` elsewhere.
        let abs =
            |path: &str| PathBuf::from(format!("{}{path}", if cfg!(windows) { "C:" } else { "" }));
        let links = |link: PathBuf| move |p: &Path| p == link;
        let joined = abs("/base/wt").join("../app/.git/worktrees/a");
        assert!(!walk_finds_link(&joined, links(abs("/elsewhere"))));
        // Components above the base are checked too.
        assert!(walk_finds_link(&joined, links(abs("/base"))));
        assert!(walk_finds_link(&joined, links(abs("/base/wt"))));
        assert!(walk_finds_link(&joined, links(abs("/base/app"))));
        assert!(walk_finds_link(
            &joined,
            links(abs("/base/app/.git/worktrees/a"))
        ));
        // An absolute target through a link is caught like a relative one.
        let absolute = abs("/checkout/lnk/worktrees/x");
        assert!(walk_finds_link(&absolute, links(abs("/checkout/lnk"))));
        assert!(!walk_finds_link(&absolute, links(abs("/checkout/other"))));
        // `.` and `..` are not looked up; `..` steps back to the parent.
        let dots = abs("/base/wt/./a/../b");
        assert!(!walk_finds_link(&dots, links(abs("/base/wt/a/.."))));
        assert!(walk_finds_link(&dots, links(abs("/base/wt/b"))));
        // A later `..` can't skip a component after a missing one.
        let past_missing = abs("/base/missing/../lnk/x");
        assert!(walk_finds_link(&past_missing, links(abs("/base/lnk"))));
        assert!(!walk_finds_link(&past_missing, links(abs("/base/other"))));
        // Not absolute: its start can't be checked, so it is refused.
        assert!(walk_finds_link(Path::new("base/app"), |_| false));
        #[cfg(windows)]
        {
            // Root- or drive-relative.
            assert!(walk_finds_link(Path::new(r"\base\app"), |_| false));
            assert!(walk_finds_link(Path::new(r"C:app"), |_| false));
        }
    }

    /// The prefix and root of a canonical path are never handed to the predicate, so
    /// `\\?\C:\` (or `/`) isn't mistaken for a link.
    #[test]
    fn walk_finds_link_skips_the_prefix_and_root() {
        let path = if cfg!(windows) {
            r"\\?\C:\work\app\.git"
        } else {
            "/work/app/.git"
        };
        let seen = std::cell::RefCell::new(Vec::new());
        assert!(!walk_finds_link(Path::new(path), |p: &Path| {
            seen.borrow_mut().push(p.to_path_buf());
            false
        }));
        let root = Path::new(path).ancestors().last().unwrap().to_path_buf();
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(!seen.contains(&root), "{seen:?}");
        assert_eq!(seen.last().map(PathBuf::as_path), Some(Path::new(path)));
    }

    #[test]
    fn names_windows_trims_count_as_links() {
        assert!(trimmed_by_windows(Path::new("/base/d ")));
        assert!(trimmed_by_windows(Path::new("/base/d.")));
        assert!(!trimmed_by_windows(Path::new("/base/d")));
        assert!(!trimmed_by_windows(Path::new("/base/.git")));
        assert!(!trimmed_by_windows(Path::new("/")));
    }

    #[test]
    fn dos_device_names_count_as_links() {
        for name in [
            "COM1", "nul", "nul.txt", "CON ", "Aux.x.y", "lpt9", "conin$", "CONOUT$", "com¹",
        ] {
            assert!(windows_device_name(&Path::new("/x").join(name)), "{name}");
        }
        for name in [
            "COM",
            "com10",
            "console",
            "nullable",
            ".git",
            "worktrees",
            "lpt",
        ] {
            assert!(!windows_device_name(&Path::new("/x").join(name)), "{name}");
        }
        assert!(!windows_device_name(Path::new("/")));
    }

    /// A prefix that isn't a drive or share is never walked into: Rust doesn't split
    /// its components, so nothing below it would be checked.
    #[cfg(windows)]
    #[test]
    fn walk_finds_link_refuses_other_prefixes() {
        assert!(walk_finds_link(Path::new(r"\\?\C:/x/../lnk"), |_| false));
        assert!(walk_finds_link(Path::new(r"\\.\C:\x"), |_| false));
        // Device paths: Rust reads `//?/C:` as a share on host `?`, `//./C:` as DeviceNS.
        assert!(walk_finds_link(
            Path::new("//?/C:/../UNC/host/share/x"),
            |_| false
        ));
        assert!(walk_finds_link(Path::new("//./C:/x"), |_| false));
        // `..` above the root, and NTFS stream syntax.
        assert!(walk_finds_link(Path::new(r"C:\..\x"), |_| false));
        assert!(walk_finds_link(
            Path::new(r"C:\x\lnk::$INDEX_ALLOCATION\y"),
            |_| false
        ));
        assert!(!walk_finds_link(Path::new(r"\\?\C:\x"), |_| false));
        assert!(!walk_finds_link(Path::new(r"\\host\share\x"), |_| false));
        assert!(!walk_finds_link(Path::new(r"\\?\UNC\host\share\x"), |_| {
            false
        }));
    }

    /// An entry that can't be examined could still be traversed, so it counts as a link;
    /// only a missing one doesn't.
    #[cfg(unix)]
    #[test]
    fn unexaminable_entry_counts_as_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tmp_dir();
        let locked = tmp.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let probe = fs::symlink_metadata(locked.join("x"));
        let denied = matches!(&probe, Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied);
        let result = is_redirecting_link(&locked.join("x"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        // Running as root, the lookup isn't denied and there is nothing to check.
        if denied {
            assert!(result);
        }
        assert!(!is_redirecting_link(&tmp.join("missing")));
        assert!(!is_redirecting_link(&tmp.join("missing/x")));
    }

    #[cfg(unix)]
    #[test]
    fn walk_finds_link_finds_a_real_symlink() {
        let tmp = canonical(&tmp_dir());
        let real = tmp.join("real");
        fs::create_dir_all(real.join("worktrees/a")).unwrap();
        std::os::unix::fs::symlink(&real, tmp.join("link")).unwrap();
        assert!(walk_finds_link(
            &tmp.join("link/worktrees/a"),
            is_redirecting_link
        ));
        assert!(!walk_finds_link(
            &tmp.join("real/worktrees/a"),
            is_redirecting_link
        ));
        // Components that don't exist are not links.
        assert!(!walk_finds_link(
            &tmp.join("missing/x"),
            is_redirecting_link
        ));
    }

    /// git never writes `commondir`, `gitdir` or `config` as symlinks; one is refused
    /// rather than followed (it could lead anywhere, including a share on Windows).
    #[cfg(unix)]
    #[test]
    fn symlinked_git_metadata_files_are_not_followed() {
        use std::os::unix::fs::symlink;
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let common = app.join(".git");
        let feat = add_worktree(&common, &tmp, "app-feat");
        let admin = common.join("worktrees/app-feat");
        assert!(detect(&feat).is_some());

        for name in ["commondir", "gitdir"] {
            let real = tmp.join(format!("real-{name}"));
            fs::rename(admin.join(name), &real).unwrap();
            symlink(&real, admin.join(name)).unwrap();
            assert_eq!(detect(&feat), None, "{name} symlink");
            fs::remove_file(admin.join(name)).unwrap();
            fs::rename(&real, admin.join(name)).unwrap();
            assert!(detect(&feat).is_some(), "{name} restored");
        }

        // A symlinked `config` is not read, so whether the repository is bare (and so
        // where its main checkout is) is unknown.
        let real_config = tmp.join("real-config");
        fs::rename(common.join("config"), &real_config).unwrap();
        symlink(&real_config, common.join("config")).unwrap();
        let wt = detect(&feat).expect("linked worktree");
        assert_eq!(wt.main_checkout, None);
        assert_eq!(wt.main_project_root, None);
        // The same file in place is read.
        fs::remove_file(common.join("config")).unwrap();
        fs::rename(&real_config, common.join("config")).unwrap();
        let wt = detect(&feat).expect("linked worktree");
        assert_eq!(wt.main_checkout, Some(canonical(&app)));
    }

    /// Creating a symlink on Windows needs Developer Mode or admin rights; without them
    /// the test has nothing to check.
    #[cfg(windows)]
    #[test]
    fn windows_links_in_git_paths_are_not_followed() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let common = app.join(".git");
        let feat = add_worktree(&common, &tmp, "app-feat");
        if symlink_dir(&app, tmp.join("app-link")).is_err() {
            return;
        }
        // A relative `gitdir:` through a directory symlink.
        write(
            &feat.join(".git"),
            "gitdir: ../app-link/.git/worktrees/app-feat\n",
        );
        assert_eq!(detect(&feat), None);
        // An absolute `gitdir:` through the same link.
        write(
            &feat.join(".git"),
            &format!(
                "gitdir: {}\n",
                tmp.join(r"app-link\.git\worktrees\app-feat").display()
            ),
        );
        assert_eq!(detect(&feat), None);
        // The same layout without the link is a worktree.
        write(
            &feat.join(".git"),
            "gitdir: ../app/.git/worktrees/app-feat\n",
        );
        assert!(detect(&feat).is_some());
        // A relative back-link through a link to the checkout. (Joined to the verbatim
        // canonical git dir, `..` is resolved lexically, so the link must be the last
        // directory named.)
        symlink_dir(&feat, tmp.join("feat-link")).unwrap();
        write(
            &common.join("worktrees/app-feat/gitdir"),
            "../../../../feat-link/.git\n",
        );
        assert_eq!(detect(&feat), None);
        // An absolute back-link through it.
        write(
            &common.join("worktrees/app-feat/gitdir"),
            &format!("{}\n", tmp.join(r"feat-link\.git").display()),
        );
        assert_eq!(detect(&feat), None);
        // An absolute `commondir` through the link.
        write(
            &common.join("worktrees/app-feat/gitdir"),
            &format!("{}\n", feat.join(".git").display()),
        );
        write(
            &common.join("worktrees/app-feat/commondir"),
            &format!("{}\n", tmp.join(r"app-link\.git").display()),
        );
        assert_eq!(detect(&feat), None);
        write(&common.join("worktrees/app-feat/commondir"), "../..\n");
        assert!(detect(&feat).is_some());
        // A `.git` that is itself a symlink.
        let real = tmp.join("real-dot-git");
        fs::rename(feat.join(".git"), &real).unwrap();
        symlink_file(&real, feat.join(".git")).unwrap();
        assert_eq!(detect(&feat), None);
    }

    /// The project's subdirectory in the main checkout comes from that checkout; a link
    /// there drops the main project root rather than reading through it.
    #[cfg(windows)]
    #[test]
    fn windows_link_in_main_project_root_is_not_followed() {
        use std::os::windows::fs::symlink_dir;
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let feat = add_worktree(&app.join(".git"), &tmp, "app-feat");
        fs::create_dir_all(feat.join("api")).unwrap();
        fs::create_dir_all(tmp.join("elsewhere")).unwrap();
        if symlink_dir(tmp.join("elsewhere"), app.join("api")).is_err() {
            return;
        }
        let wt = detect(&feat.join("api")).expect("linked worktree");
        assert_eq!(wt.main_checkout, Some(canonical(&app)));
        assert_eq!(wt.main_project_root, None);
        // A real directory there is used.
        fs::remove_dir(app.join("api")).unwrap();
        fs::create_dir(app.join("api")).unwrap();
        let wt = detect(&feat.join("api")).expect("linked worktree");
        assert_eq!(wt.main_project_root, Some(canonical(&app).join("api")));
    }

    /// A junction (`IO_REPARSE_TAG_MOUNT_POINT`) redirects a path like a symlink does, so
    /// it counts as a link; plain directories and files don't. Junctions need no special
    /// rights, but if `mklink` isn't available the test has nothing to check.
    #[cfg(windows)]
    #[test]
    fn windows_junction_counts_as_a_link() {
        let tmp = tmp_dir();
        let app = main_checkout(&tmp);
        let feat = add_worktree(&app.join(".git"), &tmp, "app-feat");
        let junction = tmp.join("app-junction");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&app)
            .output()
            .is_ok_and(|out| out.status.success());
        if !made {
            return;
        }
        assert!(is_redirecting_link(&junction));
        assert!(!is_redirecting_link(&app));
        assert!(!is_redirecting_link(&app.join(".git/HEAD")));
        // A `gitdir:` through the junction is refused; the same path without it isn't.
        write(
            &feat.join(".git"),
            "gitdir: ../app-junction/.git/worktrees/app-feat\n",
        );
        assert_eq!(detect(&feat), None);
        write(
            &feat.join(".git"),
            "gitdir: ../app/.git/worktrees/app-feat\n",
        );
        assert!(detect(&feat).is_some());
    }

    // ── WorktreePorts ─────────────────────────────────────────────────────────

    fn ports(entries: &[(&str, u16)]) -> WorktreePorts {
        WorktreePorts {
            ports: entries.iter().map(|(n, p)| (n.to_string(), *p)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn worktree_ports_round_trip() {
        let root = tmp_dir();
        let recorded = ports(&[("redis", 52000), ("postgresql", 52001)]);
        assert!(recorded.write_if_changed(&root).unwrap());
        let text = fs::read_to_string(root.join(PORTS_PATH)).unwrap();
        assert!(text.contains("version: 1"), "{text}");
        assert!(text.contains("redis: 52000"), "{text}");
        assert_eq!(WorktreePorts::load(&root), recorded);
        assert_eq!(WorktreePorts::load(&root).get("redis"), Some(52000));
        // `.devy/` was created through the helper, with its `.gitignore`.
        assert_eq!(
            fs::read_to_string(root.join(".devy/.gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn missing_worktree_ports_file_is_empty_without_a_warning() {
        let root = tmp_dir();
        let warnings = crate::output::with_warn_messages(|| {
            assert_eq!(WorktreePorts::load(&root), WorktreePorts::default());
        });
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(
            !root.join(".devy").exists(),
            "load must not create anything"
        );
    }

    #[test]
    fn corrupt_worktree_ports_file_is_empty_with_a_warning() {
        for content in [
            "ports: [not, a, map\n",
            "version: 2\nports: {}\n",
            "version: 1\nports:\n  redis: 0\n",
            "version: 1\nports:\n  redis: 99999\n",
            "version: 1\nunknown: true\n",
        ] {
            let root = tmp_dir();
            write(&root.join(PORTS_PATH), content);
            let warnings = crate::output::with_warn_messages(|| {
                assert_eq!(WorktreePorts::load(&root), WorktreePorts::default());
            });
            assert_eq!(warnings.len(), 1, "{content}: {warnings:?}");
            assert!(
                warnings[0].starts_with("ignoring unreadable .devy/worktree.yml: "),
                "{warnings:?}"
            );
            // Loading never rewrites the file.
            assert_eq!(fs::read_to_string(root.join(PORTS_PATH)).unwrap(), content);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_worktree_ports_file_is_not_followed() {
        let root = tmp_dir();
        let outside = tmp_dir();
        fs::write(
            outside.join("ports.yml"),
            "version: 1\nports:\n  redis: 52000\n",
        )
        .unwrap();
        fs::create_dir(root.join(".devy")).unwrap();
        std::os::unix::fs::symlink(outside.join("ports.yml"), root.join(PORTS_PATH)).unwrap();
        let warnings = crate::output::with_warn_messages(|| {
            assert_eq!(WorktreePorts::load(&root), WorktreePorts::default());
        });
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let err = ports(&[("redis", 1)]).write_if_changed(&root).unwrap_err();
        assert!(format!("{err:#}").contains("symbolic link"), "{err:#}");
        assert_eq!(
            fs::read_to_string(outside.join("ports.yml")).unwrap(),
            "version: 1\nports:\n  redis: 52000\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_devy_dir_is_not_read_and_file_text_never_reaches_the_warning() {
        let root = tmp_dir();
        let outside = tmp_dir();
        fs::write(outside.join("worktree.yml"), "SENTINEL_SECRET: hunter2\n").unwrap();
        std::os::unix::fs::symlink(&*outside, root.join(".devy")).unwrap();
        let warnings = crate::output::with_warn_messages(|| {
            assert_eq!(WorktreePorts::load(&root), WorktreePorts::default());
        });
        assert_eq!(
            warnings,
            ["ignoring unreadable .devy/worktree.yml: .devy is a symbolic link"]
        );

        // Even through a real `.devy`, a parse failure never quotes the file.
        let root = tmp_dir();
        write(&root.join(PORTS_PATH), "SENTINEL_SECRET: hunter2\n");
        let warnings = crate::output::with_warn_messages(|| {
            WorktreePorts::load(&root);
        });
        assert_eq!(warnings.len(), 1);
        assert!(!warnings[0].contains("SENTINEL"), "{warnings:?}");
    }

    #[test]
    fn unchanged_worktree_ports_are_not_rewritten() {
        let root = tmp_dir();
        let recorded = ports(&[("redis", 52000)]);
        assert!(recorded.write_if_changed(&root).unwrap());
        let path = root.join(PORTS_PATH);
        let before = fs::read(&path).unwrap();
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&path).unwrap());
        assert!(!recorded.write_if_changed(&root).unwrap());
        assert_eq!(fs::read(&path).unwrap(), before);
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&fs::metadata(&path).unwrap()),
            inode,
            "an unchanged file must not be replaced"
        );
        assert!(ports(&[("redis", 52001)]).write_if_changed(&root).unwrap());
        assert_eq!(WorktreePorts::load(&root).get("redis"), Some(52001));
    }
}
