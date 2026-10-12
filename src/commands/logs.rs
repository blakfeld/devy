//! `devy logs`: prints and follows service logs from wherever each service's backend
//! sends them — files tailed in-process, or `journalctl` / `<container_cli> logs`.
//! Read-only: nothing is started, stopped or written.

use anyhow::{Context, Result, anyhow, bail};
use colored::Colorize;
use std::io::{BufRead, BufReader, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::config::{Dependency, DevyConfig};
use crate::modules;
use crate::output::{clean, clean_line, clean_log};
use crate::package_manager::{self, LogCommand, LogCommandKind, LogSource};
use crate::service_runner::docker::{
    ContainerRuntime, format_timeout, read_pipe, recv_pipe, wait_within,
};
use crate::service_runner::{Runners, ServiceRunner};

use super::service;

pub const DEFAULT_LINES: u32 = 100;
/// How often followed files are checked for new output.
const POLL: Duration = Duration::from_millis(250);
/// Read size when scanning a file backwards for its last lines.
const BLOCK: u64 = 8 * 1024;

/// Set by the Ctrl-C handler; ends follow mode with exit status 0.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub name: Option<String>,
    pub lines: u32,
    pub follow: bool,
    /// Ask Claude to explain the logs (requires `name`; never with `follow`).
    pub explain: bool,
    /// With `explain`, print the request instead of sending it.
    pub show_context: bool,
}

#[cfg_attr(test, mutants::skip)] // binds the real devy.yml, backends, stdout and Ctrl-C
pub fn run(opts: Options) -> Result<()> {
    let (config, project_root) = DevyConfig::load_with_root()?;
    let pm = package_manager::detect(&config, &project_root)?;
    let runners = Runners::new(
        pm.as_ref(),
        ContainerRuntime::system(config.container_cli),
        &config,
        &project_root,
        None,
        false,
    );
    if opts.explain {
        return super::ask::explain_impl(
            &config,
            &project_root,
            &runners,
            &opts,
            &super::ask::Ai::system(),
            &mut std::io::stdout(),
        );
    }
    if opts.follow {
        ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst))
            .context("Failed to install the Ctrl-C handler")?;
    }
    let stdout = std::io::stdout();
    // Log text keeps its colors only on a terminal; every other escape is always removed.
    let tty = stdout.is_terminal();
    let result = logs_impl(
        &config,
        &project_root,
        &runners,
        &opts,
        &mut stdout.lock(),
        &INTERRUPTED,
        tty,
    );
    // A closed pipe (e.g. `| head`) just ends the output early.
    match result {
        Err(e) if is_broken_pipe(&e) => Ok(()),
        other => other,
    }
}

fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
}

/// Shows the logs `opts` asks for on `out`. Follow mode runs until `stop` is set.
/// Log text is cleaned of terminal control sequences; `tty` keeps SGR colors.
pub(crate) fn logs_impl(
    config: &DevyConfig,
    project_root: &Path,
    runners: &Runners,
    opts: &Options,
    out: &mut dyn Write,
    stop: &AtomicBool,
    tty: bool,
) -> Result<()> {
    let (services, single) = match &opts.name {
        Some(name) => (vec![service::resolve_dep(config, name)?], true),
        None => {
            let services: Vec<Dependency> = config
                .normalized_dependencies()?
                .into_iter()
                .filter(|dep| modules::get(&dep.name).is_service())
                .collect();
            if services.is_empty() {
                writeln!(out, "No services defined.")?;
                return Ok(());
            }
            (services, false)
        }
    };

    // With one service, failing to find its log source is the command's error. With
    // several, the failure (e.g. a container that belongs to someone else, or a container
    // runtime that can't be reached) is kept and reported in place, below, so the other
    // services are still shown.
    let mut sources = Vec::new();
    for dep in &services {
        let source = match runners
            .runner_for(dep)
            .log_source(dep, opts.lines, opts.follow)
        {
            Ok(LogSource::Unsupported(msg)) => bail!("{msg}"),
            Ok(source) => Ok(source),
            Err(e) if !single => Err(e),
            Err(e) => return Err(e),
        };
        sources.push((dep, source));
    }

    if opts.follow {
        return follow(&sources, single, opts.lines, out, stop, tty);
    }
    // With several services, one whose logs can't be read (e.g. a log file devy refuses
    // to open) is reported in place and the others are still shown.
    let mut unreadable = Vec::new();
    for (dep, source) in &sources {
        if !single {
            writeln!(out, "\n{}", clean_line(&dep.name).as_ref().bold())?;
        }
        let source = match source {
            Ok(source) => source,
            Err(e) => {
                info(out, &format!("{e:#}"))?;
                unreadable.push(dep.name.as_str());
                continue;
            }
        };
        let tail = match collect(source, opts.lines, None) {
            Ok(tail) => tail,
            Err(e) if !single => {
                info(out, &format!("{e:#}"))?;
                unreadable.push(dep.name.as_str());
                continue;
            }
            Err(e) => return Err(e),
        };
        match tail {
            Tail::Text(text) => {
                let text = clean_log(&text, tty);
                out.write_all(text.as_bytes())?;
                if !text.ends_with('\n') {
                    writeln!(out)?;
                }
            }
            Tail::Empty => no_logs(out, &dep.name, source)?,
        }
        for path in extra_log_paths(dep, runners, project_root) {
            info(out, &format!("also see {}", path.display()))?;
        }
    }
    if !unreadable.is_empty() {
        bail!("Could not read the logs of {}", unreadable.join(", "));
    }
    Ok(())
}

/// `· <msg>`, as `output::info` prints it (cleaned). The lines of a multi-line message
/// (e.g. a CLI's stderr) after the first are indented under it, so none can pass for a
/// `<name> | ` log line or a service header.
pub(crate) fn info(out: &mut dyn Write, msg: &str) -> std::io::Result<()> {
    let msg = clean(msg);
    let mut lines = msg.trim_end().lines();
    writeln!(out, "  {} {}", "·".cyan(), lines.next().unwrap_or_default())?;
    for line in lines {
        writeln!(out, "    {line}")?;
    }
    Ok(())
}

/// `· No logs yet for <name>`, with the expected path when the source is one file.
fn no_logs(out: &mut dyn Write, name: &str, source: &LogSource) -> std::io::Result<()> {
    match source {
        LogSource::Files(paths) if paths.len() == 1 => info(
            out,
            &format!(
                "No logs yet for {name} (expected at {})",
                paths[0].display()
            ),
        ),
        _ => info(out, &format!("No logs yet for {name}")),
    }
}

/// The module's own log files that exist, for services nix runs.
fn extra_log_paths(dep: &Dependency, runners: &Runners, project_root: &Path) -> Vec<PathBuf> {
    if dep.docker || runners.package.pm().name() != "nix" {
        return vec![];
    }
    let data_dir = modules::nix_data_dir(project_root, modules::canonical_name(&dep.name));
    modules::get(&dep.name)
        .extra_log_paths(&data_dir)
        .into_iter()
        .filter(|p| p.exists())
        .collect()
}

// ── collecting the last lines ─────────────────────────────────────────────────

/// A source's recent output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tail {
    Text(String),
    /// The service has not logged anything yet.
    Empty,
}

/// The last `lines` lines of `dep`'s log for AI context, read through its runner as
/// `devy logs` does but filtered by [`for_ai`]. Every command run (the container owner
/// check, then the log command) shares the `timeout` and is killed when it runs out; an
/// unsupported backend is an error.
pub(crate) fn recent(
    runner: &dyn ServiceRunner,
    dep: &Dependency,
    project_root: &Path,
    lines: u32,
    timeout: Duration,
) -> Result<Tail> {
    let deadline = Instant::now() + timeout;
    let source = for_ai(runner.log_source_within(dep, lines, timeout)?, project_root)?;
    collect(
        &source,
        lines,
        Some(deadline.saturating_duration_since(Instant::now())),
    )
}

/// `source` without log files that may not go into AI context: a file inside the
/// project must be a regular, non-symlink file (see [`crate::ai::is_context_path`]), so a
/// committed `.devy/data/x/x.log -> ~/.ssh/id_ed25519` is never sent.
/// When every file is dropped, that is an error rather than "no logs yet".
pub(crate) fn for_ai(source: LogSource, project_root: &Path) -> Result<LogSource> {
    match source {
        LogSource::Files(paths) if !paths.is_empty() => {
            let kept: Vec<PathBuf> = paths
                .into_iter()
                // A missing file reads as "no logs yet" and holds nothing to leak.
                .filter(|p| {
                    std::fs::symlink_metadata(p).is_err()
                        || crate::ai::is_context_path(project_root, p)
                })
                .collect();
            if kept.is_empty() {
                bail!(
                    "the log files are not regular files in the project (symlinks are never sent to claude)"
                );
            }
            Ok(LogSource::Files(kept))
        }
        other => Ok(other),
    }
}

/// Reads the last `lines` lines of `source` (built without follow). Command sources are
/// killed after `timeout`, when one is given.
pub(crate) fn collect(source: &LogSource, lines: u32, timeout: Option<Duration>) -> Result<Tail> {
    match source {
        LogSource::Unsupported(msg) => bail!("{msg}"),
        LogSource::Files(paths) => {
            let mut sections: Vec<(&PathBuf, Vec<String>)> = Vec::new();
            for path in paths {
                let (tail, _) = tail_file(path, lines)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                if !tail.is_empty() {
                    sections.push((path, tail));
                }
            }
            if sections.is_empty() {
                return Ok(Tail::Empty);
            }
            let labeled = paths.len() > 1;
            let mut text = String::new();
            for (path, tail) in sections {
                if labeled {
                    text.push_str(&format!("{}\n", path.display().to_string().dimmed()));
                }
                for line in tail {
                    text.push_str(&line);
                    text.push('\n');
                }
            }
            Ok(Tail::Text(text))
        }
        LogSource::Command(cmd) => {
            let out = capture(cmd, timeout)?;
            classify(cmd, &out)
        }
    }
}

/// Opens the log file at `path` for reading: `None` when it (or its directory) is
/// missing. Only a regular file is read, opened without following a symlink, so a log
/// file swapped for a link to another file (or a FIFO) is refused rather than shown. On
/// Unix its directory must also be one only the user or root controls
/// ([`check_log_dir`]), checked at every open: a directory that didn't exist when the
/// source was built, or was removed and recreated while following, may have been made by
/// another user. The file opened must itself be owned by the user or root
/// ([`check_log_owner`]), which closes the gap between the directory check and the open
/// and covers a file planted in a shared (sticky or group-writable) directory.
fn open_log(path: &Path) -> std::io::Result<Option<std::fs::File>> {
    let opened = check_log_dir(path)
        .and_then(|()| crate::fs_safe::open_regular_nofollow(path))
        .and_then(|file| check_log_owner(path, &file).map(|()| file));
    match opened {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether `uid` is the current user or root, who alone may own a log devy reads.
#[cfg(unix)]
fn trusted_owner(uid: u32) -> bool {
    crate::fs_safe::is_user_or_root(uid)
}

/// Refuses an opened log file (checked on the open handle) that another user owns.
#[cfg(unix)]
fn check_log_owner(path: &Path, file: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if trusted_owner(file.metadata()?.uid()) {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "{} is owned by another user; devy will not read it",
            path.display()
        ),
    ))
}

#[cfg(not(unix))]
fn check_log_owner(_path: &Path, _file: &std::fs::File) -> std::io::Result<()> {
    Ok(())
}

/// Refuses a log file whose directory isn't a real directory owned by the user or root,
/// or is writable by everyone without the sticky bit: anyone else could plant or swap
/// the file there. `NotFound` when the directory is missing. Group-writable directories
/// owned by the user are accepted (a umask of 002 makes them by default).
#[cfg(unix)]
fn check_log_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return Ok(());
    };
    let meta = std::fs::symlink_metadata(dir)?;
    let world_writable = meta.mode() & 0o002 != 0 && meta.mode() & 0o1000 == 0;
    if !meta.is_dir() || !trusted_owner(meta.uid()) || world_writable {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is not a directory only you or root can write to; devy will not read logs from it",
                dir.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_log_dir(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// The last `n` lines of the file at `path` and its length, scanning backwards from the
/// end in `BLOCK`-sized reads. A missing file has no lines; anything but a regular file
/// is refused ([`open_log`]).
pub(crate) fn tail_file(path: &Path, n: u32) -> std::io::Result<(Vec<String>, u64)> {
    let Some(mut file) = open_log(path)? else {
        return Ok((vec![], 0));
    };
    let len = file.metadata()?.len();
    let mut pos = len;
    let mut data: Vec<u8> = Vec::new();
    loop {
        // Newlines that start a line within `data`; a final newline ends the last line.
        let breaks =
            data.iter().filter(|&&b| b == b'\n').count() - usize::from(data.last() == Some(&b'\n'));
        if pos == 0 || breaks >= n as usize {
            break;
        }
        let start = pos.saturating_sub(BLOCK);
        let mut block = vec![0u8; (pos - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut block)?;
        block.extend_from_slice(&data);
        data = block;
        pos = start;
    }
    let text = String::from_utf8_lossy(&data);
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    let skip = lines.len().saturating_sub(n as usize);
    Ok((lines[skip..].to_vec(), len))
}

/// What a log command printed, with a container's stdout and stderr merged in order.
#[derive(Debug, Clone, Default)]
pub(crate) struct CmdOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[cfg_attr(test, mutants::skip)] // spawns real processes
fn capture(cmd: &LogCommand, timeout: Option<Duration>) -> Result<CmdOutput> {
    let mut command = Command::new(crate::package_manager::require_system_tool(&cmd.program)?);
    command.args(&cmd.args).stdin(Stdio::null());
    let merged = cmd.kind == LogCommandKind::Container;
    let mut merged_reader = None;
    if merged {
        let (reader, writer) = std::io::pipe().context("Failed to create a pipe")?;
        command.stdout(writer.try_clone()?).stderr(writer);
        merged_reader = Some(reader);
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to run `{}`", cmd.program))?;
    // The command keeps its copies of the pipe's write end open until dropped.
    drop(command);

    let deadline = timeout.map(|t| Instant::now() + t);
    let (stdout, stderr) = match merged_reader {
        Some(reader) => (read_pipe(Box::new(reader)), None),
        None => (
            read_pipe(Box::new(child.stdout.take().expect("stdout is piped"))),
            Some(read_pipe(Box::new(
                child.stderr.take().expect("stderr is piped"),
            ))),
        ),
    };

    let timed_out = || {
        anyhow!(
            "`{}` timed out after {}",
            cmd.program,
            format_timeout(timeout.unwrap_or_default())
        )
    };
    let Some(status) = wait_within(&mut child, timeout)? else {
        return Err(timed_out());
    };
    // With a timeout, the pipes are read only until the deadline (plus a grace): a
    // process the CLI left behind holding them open must not hold devy past it.
    Ok(CmdOutput {
        success: status.success(),
        stdout: recv_pipe(&stdout, deadline).ok_or_else(timed_out)?,
        stderr: match stderr {
            Some(rx) => recv_pipe(&rx, deadline).ok_or_else(timed_out)?,
            None => String::new(),
        },
    })
}

/// Interprets a log command's output: its text, no logs yet, or an error.
pub(crate) fn classify(cmd: &LogCommand, out: &CmdOutput) -> Result<Tail> {
    let text = out.stdout.trim_end();
    let empty = text.is_empty() || text == "-- No entries --";
    let last_stderr = || {
        out.stderr
            .trim()
            .lines()
            .last()
            .unwrap_or("no output")
            .to_string()
    };
    match &cmd.kind {
        LogCommandKind::Container => {
            if !out.success {
                if text.to_ascii_lowercase().contains("no such container") {
                    return Ok(Tail::Empty);
                }
                bail!(
                    "`{} logs` failed: {}",
                    cmd.program,
                    text.lines().last().unwrap_or("no output")
                );
            }
        }
        LogCommandKind::UserJournal => {
            if !out.success {
                if journal_unreadable(&out.stderr) {
                    return Ok(Tail::Empty);
                }
                bail!("`journalctl --user` failed: {}", last_stderr());
            }
        }
        LogCommandKind::SystemJournal { unit } => {
            if (!out.success || empty) && journal_unreadable(&out.stderr) {
                bail!(
                    "the system journal for {unit} could not be read — run `sudo journalctl -u {unit}` \
                     or add your user to the systemd-journal group"
                );
            }
            if !out.success {
                bail!("`journalctl` failed: {}", last_stderr());
            }
        }
    }
    if empty {
        Ok(Tail::Empty)
    } else {
        Ok(Tail::Text(format!("{text}\n")))
    }
}

/// Whether journalctl's stderr says the journal couldn't be read by this user.
fn journal_unreadable(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    [
        "no journal files",
        "insufficient permissions",
        "permission denied",
        "not seeing messages from other users",
    ]
    .iter()
    .any(|p| s.contains(p))
}

// ── following ─────────────────────────────────────────────────────────────────

/// Watches one file for appended lines, restarting from the top when it shrinks.
pub(crate) struct FileFollower {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    /// Whether the last poll's open was refused.
    refused: bool,
}

impl FileFollower {
    pub fn new(path: PathBuf, offset: u64) -> Self {
        Self {
            path,
            offset,
            partial: Vec::new(),
            refused: false,
        }
    }

    /// Complete lines written since the last poll. At most `MAX_LINE` bytes of a line are
    /// kept, so a long line without a newline cannot grow memory without bound. The file
    /// is reopened each poll with the same checks as [`tail_file`] ([`open_log`]), so a
    /// log replaced by a symlink, or a directory recreated by another user, is refused
    /// (`InvalidData`). A refusal can be a passing race (a file rotated between the check
    /// and the open reads as "changed"), so it is returned only when the previous poll
    /// was refused too; a first one reads as no new lines.
    pub fn poll(&mut self) -> std::io::Result<Vec<String>> {
        let opened = match open_log(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData && !self.refused => {
                self.refused = true;
                return Ok(vec![]);
            }
            other => other,
        };
        self.refused = false;
        let Some(mut file) = opened? else {
            return Ok(vec![]);
        };
        let len = file.metadata()?.len();
        if len < self.offset {
            // Truncated or replaced (e.g. launchd rewrote it on restart).
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok(vec![]);
        }
        file.seek(SeekFrom::Start(self.offset))?;
        // At most `MAX_POLL` bytes per poll; the next poll continues from `offset`.
        let mut reader = BufReader::new((&mut file).take((len - self.offset).min(MAX_POLL)));
        let mut lines = Vec::new();
        loop {
            let buf = match reader.fill_buf() {
                Ok(buf) => buf,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if buf.is_empty() {
                break;
            }
            let (chunk, used) = match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (&buf[..i], i + 1),
                None => (buf, buf.len()),
            };
            let room = MAX_LINE.saturating_sub(self.partial.len());
            self.partial
                .extend_from_slice(&chunk[..chunk.len().min(room)]);
            let newline = used > chunk.len();
            reader.consume(used);
            self.offset += used as u64;
            if newline {
                let line = String::from_utf8_lossy(&self.partial);
                lines.push(line.strip_suffix('\r').unwrap_or(&line).to_string());
                self.partial.clear();
            }
        }
        Ok(lines)
    }
}

/// Prints the last lines of every source, then streams new lines until `stop` is set.
/// With several services each line is prefixed `<name> | `.
fn follow(
    sources: &[(&Dependency, Result<LogSource>)],
    single: bool,
    lines: u32,
    out: &mut dyn Write,
    stop: &AtomicBool,
    tty: bool,
) -> Result<()> {
    let prefix = |name: &str| {
        if single {
            String::new()
        } else {
            format!("{} | ", clean_line(name))
        }
    };

    // Whether a log file was refused, at startup or partway through following.
    let mut refused = false;
    // Services whose log source couldn't be found (only with several services).
    let mut unfollowed: Vec<&str> = Vec::new();
    // Files: print their tails now, then poll from where each tail ended.
    let mut followers: Vec<(String, FileFollower)> = Vec::new();
    for (dep, source) in sources {
        let source = match source {
            Ok(source) => source,
            Err(e) => {
                info(out, &format!("{}not following: {e:#}", prefix(&dep.name)))?;
                unfollowed.push(dep.name.as_str());
                continue;
            }
        };
        if let LogSource::Files(paths) = source {
            let mut any = false;
            for path in paths {
                let (tail, len) = match tail_file(path, lines) {
                    Ok(read) => read,
                    // With several services, a refused file is noted and not followed.
                    Err(e) if !single && e.kind() == std::io::ErrorKind::InvalidData => {
                        any = true;
                        refused = true;
                        info(out, &format!("{}not following: {e}", prefix(&dep.name)))?;
                        continue;
                    }
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("Failed to read {}", path.display()));
                    }
                };
                any |= !tail.is_empty();
                for line in tail {
                    writeln!(out, "{}{}", prefix(&dep.name), clean_log(&line, tty))?;
                }
                followers.push((prefix(&dep.name), FileFollower::new(path.clone(), len)));
            }
            if !any {
                no_logs(out, &dep.name, source)?;
            }
        }
    }
    out.flush()?;

    let mut children: Vec<(String, Child)> = Vec::new();
    let done = AtomicBool::new(false);
    let done = &done;
    let result = std::thread::scope(|scope| -> Result<()> {
        let (tx, rx) = mpsc::channel::<(String, Followed)>();

        for (prefix, mut follower) in followers {
            let tx = tx.clone();
            scope.spawn(move || {
                while !stop.load(Ordering::SeqCst) && !done.load(Ordering::SeqCst) {
                    let lines = match follower.poll() {
                        Ok(lines) => lines,
                        // Refused (see `FileFollower::poll`): say so once and stop.
                        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                            let _ = tx.send((prefix.clone(), Followed::Refused(e.to_string())));
                            return;
                        }
                        // Anything else may pass (e.g. the file mid-rotation).
                        Err(_) => vec![],
                    };
                    for line in lines {
                        if tx.send((prefix.clone(), Followed::Line(line))).is_err() {
                            return;
                        }
                    }
                    std::thread::sleep(POLL);
                }
            });
        }

        let streamed = || -> Result<()> {
            for (dep, source) in sources {
                let Ok(LogSource::Command(cmd)) = source else {
                    continue;
                };
                let mut child =
                    Command::new(crate::package_manager::require_system_tool(&cmd.program)?)
                        .args(&cmd.args)
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .with_context(|| format!("Failed to run `{}`", cmd.program))?;
                let pipes: [Box<dyn Read + Send>; 2] = [
                    Box::new(child.stdout.take().expect("stdout is piped")),
                    Box::new(child.stderr.take().expect("stderr is piped")),
                ];
                children.push((cmd.program.clone(), child));
                for pipe in pipes {
                    let tx = tx.clone();
                    let prefix = prefix(&dep.name);
                    scope.spawn(move || {
                        let mut reader = BufReader::new(pipe);
                        while let Some(line) = read_line_lossy(&mut reader) {
                            if tx.send((prefix.clone(), Followed::Line(line))).is_err() {
                                return;
                            }
                        }
                    });
                }
            }
            drop(tx);
            refused |= print_until_stopped(&rx, out, stop, tty)?;
            Ok(())
        };
        let result = streamed();
        // Ends the file pollers and the commands, so their threads finish.
        done.store(true, Ordering::SeqCst);
        for (_, child) in &mut children {
            let _ = child.kill();
        }
        result
    });
    for (_, child) in &mut children {
        let _ = child.wait();
    }
    result?;
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Every stream ended on its own: report a log command that failed.
    let failed: Vec<&str> = children
        .iter_mut()
        .filter_map(|(program, child)| match child.try_wait() {
            Ok(Some(status)) if !status.success() => Some(program.as_str()),
            _ => None,
        })
        .collect();
    match follow_error(&failed, &unfollowed, refused) {
        Some(msg) => Err(anyhow!("{msg}")),
        None => Ok(()),
    }
}

/// Why following ended badly, once every stream has ended on its own: each log command
/// that `failed` (once per program), the services never followed, and a log file devy
/// `refused`, joined into one message. `None` when there is nothing to report.
fn follow_error(failed: &[&str], unfollowed: &[&str], refused: bool) -> Option<String> {
    let mut reasons: Vec<String> = Vec::new();
    for program in failed {
        let reason = format!("`{program}` exited with an error");
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    if !unfollowed.is_empty() {
        reasons.push(format!(
            "Could not follow the logs of {}",
            unfollowed.join(", ")
        ));
    }
    if refused {
        reasons.push("Stopped following a log file devy refused to read".to_string());
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// What a follower thread reports.
enum Followed {
    /// A line of log output.
    Line(String),
    /// The file is no longer followed: devy refused to read it (the reason).
    Refused(String),
}

/// Longest line kept from a followed log command; the rest of a longer line is dropped.
const MAX_LINE: usize = 64 * 1024;

/// Most bytes one `FileFollower::poll` reads, bounding the lines it holds at once.
const MAX_POLL: u64 = 8 * 1024 * 1024;

/// Reads one line (without its `\n` / `\r\n`), decoding invalid UTF-8 lossily so a
/// binary byte does not end the stream, and keeping at most `MAX_LINE` bytes of it.
/// `None` at end of input or on a read error.
fn read_line_lossy(reader: &mut dyn BufRead) -> Option<String> {
    let mut line = Vec::new();
    let mut any = false;
    loop {
        let buf = match reader.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if buf.is_empty() {
            break;
        }
        any = true;
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], i + 1),
            None => (buf, buf.len()),
        };
        let room = MAX_LINE.saturating_sub(line.len());
        line.extend_from_slice(&chunk[..chunk.len().min(room)]);
        let found = done > chunk.len();
        reader.consume(done);
        if found {
            break;
        }
    }
    if !any {
        return None;
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Some(String::from_utf8_lossy(&line).into_owned())
}

/// Writes received lines until `stop` is set or every sender is gone; a refused file is
/// reported as a devy note, not as a log line. Returns whether any file was refused.
fn print_until_stopped(
    rx: &mpsc::Receiver<(String, Followed)>,
    out: &mut dyn Write,
    stop: &AtomicBool,
    tty: bool,
) -> Result<bool> {
    let mut refused = false;
    while !stop.load(Ordering::SeqCst) {
        match rx.recv_timeout(POLL) {
            Ok((prefix, Followed::Line(line))) => {
                writeln!(out, "{prefix}{}", clean_log(&line, tty))?;
                out.flush()?;
            }
            Ok((prefix, Followed::Refused(why))) => {
                refused = true;
                info(out, &format!("{prefix}not following: {why}"))?;
                out.flush()?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Dependency;
    use crate::modules::LaunchSpec;
    use crate::package_manager::PackageManager;
    use crate::service_runner::package_runners;
    use crate::test_support::{private_tmp_dir, tmp_dir};
    use std::collections::HashMap;

    /// A backend whose services log to `<dir>/<service name>.log`.
    struct FilesPm {
        name: &'static str,
        dir: PathBuf,
    }

    impl PackageManager for FilesPm {
        fn name(&self) -> &str {
            self.name
        }
        fn is_available(&self) -> bool {
            true
        }
        fn bootstrap(&self) -> Result<()> {
            Ok(())
        }
        fn is_package_installed(&self, _: &Dependency) -> Result<bool> {
            Ok(true)
        }
        fn install_package(&self, _: &Dependency) -> Result<()> {
            Ok(())
        }
        fn is_service_running(&self, _: &str) -> Result<bool> {
            Ok(true)
        }
        fn start_service(&self, _: &str, _: Option<&LaunchSpec>) -> Result<()> {
            Ok(())
        }
        fn stop_service(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn resolved_version(&self, _: &Dependency) -> Result<Option<String>> {
            Ok(None)
        }
        fn log_source(&self, name: &str, _: u32, _: bool) -> Result<LogSource> {
            Ok(LogSource::Files(vec![self.dir.join(format!("{name}.log"))]))
        }
    }

    fn numbered(range: std::ops::RangeInclusive<u32>) -> String {
        range.map(|i| format!("line {i}\n")).collect()
    }

    fn opts(name: Option<&str>, lines: u32, follow: bool) -> Options {
        Options {
            name: name.map(String::from),
            lines,
            follow,
            explain: false,
            show_context: false,
        }
    }

    /// Runs `logs_impl` for `deps` with services logging into `dir`.
    fn run_logs(dir: &Path, pm_name: &'static str, deps: &[&str], o: &Options) -> Result<String> {
        run_logs_on(dir, pm_name, deps, o, false)
    }

    /// `run_logs`, as if stdout were a terminal when `tty`.
    fn run_logs_on(
        dir: &Path,
        pm_name: &'static str,
        deps: &[&str],
        o: &Options,
        tty: bool,
    ) -> Result<String> {
        let pm = FilesPm {
            name: pm_name,
            dir: dir.to_path_buf(),
        };
        let config = crate::test_support::make_config(deps, HashMap::new());
        let mut out = Vec::new();
        logs_impl(
            &config,
            dir,
            &package_runners(&pm, dir),
            o,
            &mut out,
            &AtomicBool::new(false),
            tty,
        )?;
        Ok(String::from_utf8(out).unwrap())
    }

    // ── terminal sanitization ────────────────────────────────────────────────

    const HOSTILE_LOG: &str = "\x1b[31mred\x1b[0m \x1b]52;c;ZWNobyBoaQ==\x07\x1b]8;;https://x.example\x1b\\link\x1b]8;;\x1b\\\x1b[2J\x1b[1;1Hend\n";

    #[test]
    fn logs_keep_sgr_but_strip_other_escapes_on_a_tty() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("redis.log"), HOSTILE_LOG).unwrap();
        let o = opts(Some("redis"), 10, false);
        let out = run_logs_on(&dir, "nix", &["redis"], &o, true).unwrap();
        assert_eq!(out, "\x1b[31mred\x1b[0m linkend\x1b[0m\n");
    }

    #[cfg(unix)]
    #[test]
    fn for_ai_drops_symlinked_project_log_files() {
        let dir = crate::test_support::tmp_dir();
        let outside = crate::test_support::tmp_dir();
        std::fs::write(outside.join("id_ed25519"), "PRIVATE\n").unwrap();
        std::fs::write(outside.join("system.log"), "ok\n").unwrap();
        std::fs::write(dir.join("real.log"), "ok\n").unwrap();
        std::os::unix::fs::symlink(outside.join("id_ed25519"), dir.join("pg.log")).unwrap();
        let source = LogSource::Files(vec![
            dir.join("real.log"),
            dir.join("pg.log"),
            outside.join("system.log"),
        ]);
        assert_eq!(
            for_ai(source, &dir).unwrap(),
            LogSource::Files(vec![dir.join("real.log"), outside.join("system.log")])
        );
        // Dropping every file is an error, not "no logs yet".
        let only_link = LogSource::Files(vec![dir.join("pg.log")]);
        let err = format!("{:#}", for_ai(only_link, &dir).unwrap_err());
        assert!(err.contains("not regular files"), "{err}");
        // A missing file is kept: it means the service has not logged yet.
        let missing = LogSource::Files(vec![dir.join("missing.log")]);
        assert_eq!(for_ai(missing.clone(), &dir).unwrap(), missing);
    }

    #[test]
    fn read_line_lossy_survives_invalid_utf8_and_caps_length() {
        let mut long = vec![b'x'; MAX_LINE + 10];
        long.push(b'\n');
        let mut input: Vec<u8> = b"ok\r\nbad\xff\x9bbyte\n".to_vec();
        input.extend_from_slice(&long);
        input.extend_from_slice(b"last");
        let mut reader = BufReader::with_capacity(16, &input[..]);
        assert_eq!(read_line_lossy(&mut reader).as_deref(), Some("ok"));
        assert_eq!(
            read_line_lossy(&mut reader).as_deref(),
            Some("bad\u{fffd}\u{fffd}byte")
        );
        assert_eq!(read_line_lossy(&mut reader).unwrap().len(), MAX_LINE);
        assert_eq!(read_line_lossy(&mut reader).as_deref(), Some("last"));
        assert_eq!(read_line_lossy(&mut reader), None);
    }

    #[test]
    fn unterminated_escape_in_a_tail_hides_only_its_line() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("redis.log"), "one\x1b]52;junk\ntwo\nthree\x1b]").unwrap();
        let out = run_logs(&dir, "nix", &["redis"], &opts(Some("redis"), 10, false)).unwrap();
        assert_eq!(out, "one\ntwo\nthree\n");
    }

    #[test]
    fn logs_strip_every_escape_when_not_a_tty() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("redis.log"), HOSTILE_LOG).unwrap();
        let out = run_logs(&dir, "nix", &["redis"], &opts(Some("redis"), 10, false)).unwrap();
        assert_eq!(out, "red linkend\n");
    }

    #[test]
    fn followed_logs_are_cleaned() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("redis.log"), HOSTILE_LOG).unwrap();
        let pm = FilesPm {
            name: "nix",
            dir: dir.to_path_buf(),
        };
        let config = crate::test_support::make_config(&["redis"], HashMap::new());
        // Already stopped: prints the tail and returns.
        let stop = AtomicBool::new(true);
        let mut out = Vec::new();
        logs_impl(
            &config,
            &dir,
            &package_runners(&pm, &dir),
            &opts(Some("redis"), 10, true),
            &mut out,
            &stop,
            true,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b[31mred\x1b[0m linkend\x1b[0m\n"
        );
    }

    /// A `Write` whose bytes another thread can read while it is being written.
    #[cfg(unix)]
    #[derive(Clone, Default)]
    struct SharedOut(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    #[cfg(unix)]
    impl Write for SharedOut {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    impl SharedOut {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    /// A followed log swapped for a symlink is reported once as a devy note (not as a log
    /// line), following it stops, and the command fails.
    #[cfg(unix)]
    #[test]
    fn a_log_swapped_for_a_symlink_while_following_is_reported() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("redis.log");
        std::fs::write(&path, "first\n").unwrap();
        std::fs::write(dir.join("secret"), "secret\n").unwrap();
        let pm = FilesPm {
            name: "nix",
            dir: dir.to_path_buf(),
        };
        let config = crate::test_support::make_config(&["redis"], HashMap::new());
        let stop = AtomicBool::new(false);
        let out = SharedOut::default();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut writer = out.clone();
        let result = std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                logs_impl(
                    &config,
                    &dir,
                    &package_runners(&pm, &dir),
                    &opts(Some("redis"), 10, true),
                    &mut writer,
                    &stop,
                    false,
                )
            });
            // Swap only once the tail has been printed and following has begun.
            while !out.text().contains("first\n") && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(dir.join("secret"), &path).unwrap();
            // The only follower ends on its own; `stop` is a safety net.
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            stop.store(true, Ordering::SeqCst);
            handle.join().unwrap()
        });
        let text = out.text();
        assert!(text.starts_with("first\n"), "{text}");
        assert!(
            text.contains("not following:") && text.contains("is a symlink"),
            "{text}"
        );
        assert!(!text.contains("secret\n"), "{text}");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("refused to read"), "{err}");
    }

    /// A log file (or its directory) another user owns is refused; the open handle's
    /// owner is what counts, so a file swapped in after the directory check is caught too.
    #[cfg(unix)]
    #[test]
    fn log_files_owned_by_another_user_are_refused() {
        let other = crate::fs_safe::current_uid() + 1;
        if crate::fs_safe::current_uid() == 0 {
            return; // Root-owned files are always trusted.
        }
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        std::fs::write(&path, "line\n").unwrap();
        let file = crate::fs_safe::open_regular_nofollow(&path).unwrap();
        assert!(check_log_owner(&path, &file).is_ok());
        let err =
            crate::fs_safe::with_fake_owner(other, || check_log_owner(&path, &file)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("owned by another user"), "{err}");
        // Through `tail_file`, the directory check refuses it first.
        let err = crate::fs_safe::with_fake_owner(other, || tail_file(&path, 10)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("only you or root"), "{err}");
    }

    /// A refusal seen on only one poll (e.g. a rotation racing the open) doesn't end
    /// following; the follower picks the file up again.
    #[cfg(unix)]
    #[test]
    fn a_passing_refusal_does_not_stop_following() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        std::fs::write(&path, "old\n").unwrap();
        let mut f = FileFollower::new(path.clone(), 4);
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(dir.join("missing-target"), &path).unwrap();
        assert!(f.poll().unwrap().is_empty());
        std::fs::remove_file(&path).unwrap();
        // Recreated shorter than what was read, so it is read from the top.
        std::fs::write(&path, "n\n").unwrap();
        assert_eq!(f.poll().unwrap(), ["n"]);
        // Refused on two polls in a row: reported.
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(dir.join("missing-target"), &path).unwrap();
        assert!(f.poll().unwrap().is_empty());
        let err = f.poll().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// With several services, a refused log file is reported in place, the other
    /// services' logs are still shown, and the command fails at the end.
    #[cfg(unix)]
    #[test]
    fn one_refused_log_does_not_hide_the_other_services() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("nginx.log"), "nginx up\n").unwrap();
        std::fs::write(dir.join("secret"), "secret\n").unwrap();
        std::os::unix::fs::symlink(dir.join("secret"), dir.join("redis.log")).unwrap();
        let pm = FilesPm {
            name: "brew",
            dir: dir.to_path_buf(),
        };
        let config = crate::test_support::make_config(&["redis", "nginx"], HashMap::new());
        let mut out = Vec::new();
        let err = logs_impl(
            &config,
            &dir,
            &package_runners(&pm, &dir),
            &opts(None, 10, false),
            &mut out,
            &AtomicBool::new(false),
            false,
        )
        .unwrap_err();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("nginx up"), "{out}");
        assert!(out.contains("is a symlink"), "{out}");
        assert!(!out.contains("secret\n"), "{out}");
        assert_eq!(err.to_string(), "Could not read the logs of redis");
    }

    /// In follow mode, a log file refused when following starts fails the command the
    /// same way as one refused partway through: with every file refused, there is
    /// nothing to follow and `devy logs -f` exits with an error rather than 0.
    #[cfg(unix)]
    #[test]
    fn following_fails_when_every_log_is_refused_at_startup() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("secret"), "secret\n").unwrap();
        std::os::unix::fs::symlink(dir.join("secret"), dir.join("redis.log")).unwrap();
        std::os::unix::fs::symlink(dir.join("secret"), dir.join("nginx.log")).unwrap();
        let pm = FilesPm {
            name: "brew",
            dir: dir.to_path_buf(),
        };
        let config = crate::test_support::make_config(&["redis", "nginx"], HashMap::new());
        let mut out = Vec::new();
        let err = logs_impl(
            &config,
            &dir,
            &package_runners(&pm, &dir),
            &opts(None, 10, true),
            &mut out,
            &AtomicBool::new(false),
            false,
        )
        .unwrap_err();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("redis | not following:"), "{out}");
        assert!(out.contains("nginx | not following:"), "{out}");
        assert!(!out.contains("secret\n"), "{out}");
        assert!(err.to_string().contains("refused to read"), "{err}");
    }

    /// In follow mode, a log file refused at startup is noted while the other services
    /// are still followed; once the last follower stops on its own the command fails,
    /// as it does for a file refused partway through.
    #[cfg(unix)]
    #[test]
    fn following_continues_past_a_log_refused_at_startup_then_fails() {
        let dir = crate::test_support::tmp_dir();
        let nginx = dir.join("nginx.log");
        std::fs::write(&nginx, "nginx up\n").unwrap();
        std::fs::write(dir.join("secret"), "secret\n").unwrap();
        std::os::unix::fs::symlink(dir.join("secret"), dir.join("redis.log")).unwrap();
        let pm = FilesPm {
            name: "brew",
            dir: dir.to_path_buf(),
        };
        let config = crate::test_support::make_config(&["redis", "nginx"], HashMap::new());
        let stop = AtomicBool::new(false);
        let out = SharedOut::default();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut writer = out.clone();
        let result = std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                logs_impl(
                    &config,
                    &dir,
                    &package_runners(&pm, &dir),
                    &opts(None, 10, true),
                    &mut writer,
                    &stop,
                    false,
                )
            });
            while !out.text().contains("nginx | nginx up\n") && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::fs::OpenOptions::new()
                .append(true)
                .open(&nginx)
                .unwrap()
                .write_all(b"nginx later\n")
                .unwrap();
            while !out.text().contains("nginx | nginx later\n") && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            // End the remaining follower on its own (a refusal partway through).
            std::fs::remove_file(&nginx).unwrap();
            std::os::unix::fs::symlink(dir.join("secret"), &nginx).unwrap();
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            stop.store(true, Ordering::SeqCst);
            handle.join().unwrap()
        });
        let text = out.text();
        assert!(text.contains("redis | not following:"), "{text}");
        assert!(text.contains("nginx | nginx later\n"), "{text}");
        assert!(!text.contains("secret\n"), "{text}");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("refused to read"), "{err}");
    }

    /// A log file's directory must be one only the user (or root) can write to, checked
    /// at every open, so one created later by someone else is never read from.
    #[cfg(unix)]
    #[test]
    fn log_files_in_a_world_writable_directory_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let base = crate::test_support::tmp_dir();
        let dir = base.join("logs");
        let path = dir.join("a.log");
        // Missing directory: no lines yet, and nothing is created.
        let mut f = FileFollower::new(path.clone(), 0);
        assert!(f.poll().unwrap().is_empty());
        assert_eq!(tail_file(&path, 10).unwrap(), (vec![], 0));
        assert!(!dir.exists());
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(&path, "line\n").unwrap();
        for mode in [0o777, 0o773] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
            let err = tail_file(&path, 10).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert!(err.to_string().contains("only you or root"), "{err}");
            assert!(f.poll().unwrap().is_empty() && f.poll().is_err());
        }
        // Sticky (like /tmp) or not writable by others: read.
        for mode in [0o1777, 0o775, 0o700] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(tail_file(&path, 10).unwrap().0, ["line"]);
        }
        assert_eq!(f.poll().unwrap(), ["line"]);
    }

    // ── tail_file ─────────────────────────────────────────────────────────────

    #[test]
    fn tail_of_a_short_file_is_the_whole_file() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        std::fs::write(&path, numbered(1..=5)).unwrap();
        let (lines, len) = tail_file(&path, 100).unwrap();
        assert_eq!(lines, numbered(1..=5).lines().collect::<Vec<_>>());
        assert_eq!(len, numbered(1..=5).len() as u64);
    }

    #[test]
    fn tail_of_exactly_n_lines() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        std::fs::write(&path, numbered(1..=20)).unwrap();
        let (lines, _) = tail_file(&path, 20).unwrap();
        assert_eq!(lines.len(), 20);
        assert_eq!(lines[0], "line 1");
    }

    #[test]
    fn tail_without_a_trailing_newline() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        std::fs::write(&path, "one\ntwo\r\nthree").unwrap();
        assert_eq!(tail_file(&path, 2).unwrap().0, ["two", "three"]);
    }

    #[test]
    fn tail_spans_several_blocks() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        std::fs::write(&path, numbered(1..=5000)).unwrap();
        let (lines, _) = tail_file(&path, 2000).unwrap();
        assert_eq!(lines.len(), 2000);
        assert_eq!(lines[0], "line 3001");
        assert_eq!(lines[1999], "line 5000");
    }

    #[test]
    fn tail_of_a_missing_file_is_empty() {
        let dir = crate::test_support::tmp_dir();
        assert_eq!(tail_file(&dir.join("nope.log"), 10).unwrap(), (vec![], 0));
    }

    // ── FileFollower ──────────────────────────────────────────────────────────

    #[test]
    fn follower_sees_appended_lines() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        std::fs::write(&path, "old\n").unwrap();
        let (_, len) = tail_file(&path, 10).unwrap();
        let mut f = FileFollower::new(path.clone(), len);
        assert!(f.poll().unwrap().is_empty());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"new\npart").unwrap();
        assert_eq!(f.poll().unwrap(), ["new"]);
        file.write_all(b"ial\n").unwrap();
        assert_eq!(f.poll().unwrap(), ["partial"]);
    }

    #[test]
    fn follower_caps_long_lines_and_handles_many_short_ones() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        let mut data = vec![b'x'; MAX_LINE * 2];
        data.extend_from_slice(b"\r\nshort\r\n");
        data.extend_from_slice(&b"y\n".repeat(20_000));
        std::fs::write(&path, &data).unwrap();
        let mut f = FileFollower::new(path.clone(), 0);
        let lines = f.poll().unwrap();
        assert_eq!(lines.len(), 20_002);
        assert_eq!(lines[0].len(), MAX_LINE);
        assert_eq!(lines[1], "short");
        assert_eq!(lines[20_001], "y");
        assert!(f.poll().unwrap().is_empty());
    }

    #[test]
    fn follower_restarts_after_truncation() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        std::fs::write(&path, numbered(1..=3)).unwrap();
        let mut f = FileFollower::new(path.clone(), numbered(1..=3).len() as u64);
        std::fs::write(&path, "fresh\n").unwrap();
        assert_eq!(f.poll().unwrap(), ["fresh"]);
    }

    /// Log files are opened without following symlinks and must be regular files, both
    /// when tailed and on every follow poll.
    #[cfg(unix)]
    #[test]
    fn symlinked_or_special_log_files_are_refused() {
        let dir = crate::test_support::tmp_dir();
        let target = dir.join("secret");
        std::fs::write(&target, "secret\n").unwrap();
        let link = dir.join("a.log");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = tail_file(&link, 10).unwrap_err();
        assert!(err.to_string().contains("is a symlink"), "{err}");
        let err = tail_file(&dir, 10).unwrap_err();
        assert!(err.to_string().contains("is not a regular file"), "{err}");
        // A followed file later swapped for a symlink is refused (on a second poll).
        let path = dir.join("b.log");
        std::fs::write(&path, "old\n").unwrap();
        let mut f = FileFollower::new(path.clone(), 4);
        assert!(f.poll().unwrap().is_empty());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(f.poll().unwrap().is_empty() && f.poll().is_err());
    }

    #[test]
    fn follower_waits_for_a_missing_file() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        let mut f = FileFollower::new(path.clone(), 0);
        assert!(f.poll().unwrap().is_empty());
        std::fs::write(&path, "started\n").unwrap();
        assert_eq!(f.poll().unwrap(), ["started"]);
    }

    // ── classify ──────────────────────────────────────────────────────────────

    fn cmd(kind: LogCommandKind) -> LogCommand {
        LogCommand {
            program: "docker".into(),
            args: vec![],
            kind,
        }
    }

    fn output(success: bool, stdout: &str, stderr: &str) -> CmdOutput {
        CmdOutput {
            success,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn system() -> LogCommandKind {
        LogCommandKind::SystemJournal {
            unit: "redis-server".into(),
        }
    }

    fn kinds() -> [LogCommandKind; 3] {
        [
            LogCommandKind::UserJournal,
            system(),
            LogCommandKind::Container,
        ]
    }

    #[test]
    fn output_is_returned_as_text() {
        for kind in kinds() {
            assert_eq!(
                classify(&cmd(kind), &output(true, "a\nb\n\n", "")).unwrap(),
                Tail::Text("a\nb\n".into())
            );
        }
    }

    #[test]
    fn empty_output_means_no_logs_yet() {
        for kind in kinds() {
            for stdout in ["", "\n", "-- No entries --\n"] {
                assert_eq!(
                    classify(&cmd(kind.clone()), &output(true, stdout, "")).unwrap(),
                    Tail::Empty
                );
            }
        }
    }

    #[test]
    fn unreadable_system_journal_suggests_sudo_or_the_group() {
        let hint = "Hint: You are currently not seeing messages from other users and the system.\n";
        for out in [
            output(true, "-- No entries --\n", hint),
            output(
                false,
                "",
                "No journal files were opened due to insufficient permissions.\n",
            ),
        ] {
            let err = classify(&cmd(system()), &out).unwrap_err().to_string();
            assert!(err.contains("could not be read"), "{err}");
            assert!(err.contains("`sudo journalctl -u redis-server`"), "{err}");
            assert!(err.contains("systemd-journal group"), "{err}");
        }
        // Readable entries are shown even when journalctl adds a hint.
        assert_eq!(
            classify(&cmd(system()), &output(true, "x\n", hint)).unwrap(),
            Tail::Text("x\n".into())
        );
    }

    #[test]
    fn missing_user_journal_means_no_logs_yet() {
        let out = output(false, "", "No journal files were found.\n");
        assert_eq!(
            classify(&cmd(LogCommandKind::UserJournal), &out).unwrap(),
            Tail::Empty
        );
        let err = classify(
            &cmd(LogCommandKind::UserJournal),
            &output(false, "", "Failed to connect\n"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("Failed to connect"), "{err}");
    }

    #[test]
    fn missing_container_means_no_logs_yet() {
        for msg in [
            "Error response from daemon: No such container: devy-app-redis\n",
            "Error: no such container devy-app-redis\n",
        ] {
            assert_eq!(
                classify(&cmd(LogCommandKind::Container), &output(false, msg, "")).unwrap(),
                Tail::Empty
            );
        }
        let err = classify(
            &cmd(LogCommandKind::Container),
            &output(false, "Cannot connect to the Docker daemon\n", ""),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "`docker logs` failed: Cannot connect to the Docker daemon"
        );
    }

    #[test]
    fn missing_tool_is_named() {
        let source = LogCommand {
            program: "devy-no-such-log-tool".into(),
            args: vec![],
            kind: LogCommandKind::UserJournal,
        };
        let err = collect(&LogSource::Command(source), 10, None).unwrap_err();
        assert!(err.to_string().contains("`devy-no-such-log-tool`"), "{err}");
    }

    /// With a timeout, a process the log command leaves behind holding its pipes open
    /// can't keep `capture` waiting past it, merged (container) or not; without one, a
    /// command that finishes is read in full.
    #[cfg(unix)]
    #[test]
    fn capture_does_not_wait_on_pipes_held_past_the_timeout() {
        let sh = |script: &str, kind: LogCommandKind| LogCommand {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            kind,
        };
        for kind in [LogCommandKind::Container, LogCommandKind::UserJournal] {
            let start = Instant::now();
            let err = capture(
                &sh("sleep 5 & echo started", kind.clone()),
                Some(Duration::from_millis(300)),
            )
            .unwrap_err();
            assert!(
                start.elapsed() < Duration::from_secs(4),
                "{:?}",
                start.elapsed()
            );
            assert_eq!(err.to_string(), "`sh` timed out after 300 ms");

            let out = capture(&sh("echo out; echo err >&2", kind.clone()), None).unwrap();
            assert!(out.success);
            assert!(out.stdout.contains("out\n"), "{out:?}");
            let all = format!("{}{}", out.stdout, out.stderr);
            assert!(all.contains("err\n"), "{out:?}");
        }
    }

    // ── logs_impl: one service ────────────────────────────────────────────────

    #[test]
    fn one_service_prints_its_last_lines() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("redis.log"), numbered(1..=500)).unwrap();
        let out = run_logs(&dir, "nix", &["redis"], &opts(Some("redis"), 20, false)).unwrap();
        assert_eq!(out, numbered(481..=500));
    }

    #[test]
    fn alias_reads_the_canonical_services_log() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("postgresql.log"), "ready\n").unwrap();
        let o = opts(Some("postgres"), 100, false);
        let out = run_logs(&dir, "nix", &["postgresql"], &o).unwrap();
        assert_eq!(out, "ready\n");
    }

    #[test]
    fn missing_log_reports_no_logs_yet_with_the_path() {
        let dir = crate::test_support::tmp_dir();
        let out = run_logs(&dir, "nix", &["redis"], &opts(Some("redis"), 100, false)).unwrap();
        assert!(
            out.contains(&format!(
                "No logs yet for redis (expected at {})",
                dir.join("redis.log").display()
            )),
            "{out}"
        );
    }

    #[test]
    fn existing_extra_log_files_are_hinted_under_nix_only() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("nginx.log"), "started\n").unwrap();
        let data = modules::nix_data_dir(&dir, "nginx");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("error.log"), "boom\n").unwrap();
        let o = opts(Some("nginx"), 100, false);
        let out = run_logs(&dir, "nix", &["nginx"], &o).unwrap();
        assert!(
            out.starts_with("started\n")
                && out.contains(&format!("also see {}", data.join("error.log").display())),
            "{out}"
        );
        assert!(
            !out.contains("access.log"),
            "missing files are not hinted: {out}"
        );
        assert!(!out.contains("boom"), "contents are not printed: {out}");
        let out = run_logs(&dir, "brew", &["nginx"], &o).unwrap();
        assert!(!out.contains("also see"), "{out}");
    }

    // ── nix sources ──────────────────────────────────────────────────────────

    /// `devy logs <name>` against the real nix backend, with its launchd agents (when
    /// `launchd`) or systemd units in `units` and its log files in `logs`.
    fn nix_logs(root: &Path, launchd: bool, units: &Path, logs: &Path, name: &str) -> String {
        let pm = crate::package_manager::NixPackageManager::with_test_dirs(
            root, "app", launchd, units, logs,
        );
        let config = crate::test_support::make_config(&[name], HashMap::new());
        let mut out = Vec::new();
        logs_impl(
            &config,
            root,
            &package_runners(&pm, root),
            &opts(Some(name), 10, false),
            &mut out,
            &AtomicBool::new(false),
            false,
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    /// The journal unit `devy logs` reads for `name` under nix with systemd units in `units`.
    fn nix_journal_unit(root: &Path, units: &Path, name: &str) -> String {
        let pm = crate::package_manager::NixPackageManager::with_test_dirs(
            root, "app", false, units, units,
        );
        let dep = Dependency::simple(name);
        let LogSource::Command(cmd) = package_runners(&pm, root)
            .runner_for(&dep)
            .log_source(&dep, 10, false)
            .unwrap()
        else {
            panic!("expected a journal command");
        };
        assert_eq!(cmd.kind, LogCommandKind::UserJournal);
        cmd.args[2].clone()
    }

    #[test]
    fn nix_on_macos_reads_the_per_project_log_file() {
        let (root, units, logs) = (tmp_dir(), tmp_dir(), private_tmp_dir());
        let pm = crate::package_manager::NixPackageManager::with_test_dirs(
            &root, "app", true, &units, &logs,
        );
        let (_, log_file) = pm.unit_and_log_names("redis");
        assert!(log_file.starts_with("devy-app-") && log_file.ends_with("-redis.log"));
        std::fs::write(logs.join(&log_file), "per-project\n").unwrap();
        std::fs::write(logs.join("redis.log"), "legacy\n").unwrap();
        assert_eq!(
            nix_logs(&root, true, &units, &logs, "redis"),
            "per-project\n"
        );
    }

    /// The redis data directory of a legacy unit, which makes it `root`'s.
    fn legacy_data_dir(root: &Path) -> String {
        modules::nix_data_dir(root, "redis").display().to_string()
    }

    #[test]
    fn nix_on_macos_falls_back_to_the_legacy_log_file() {
        let (root, units, logs) = (tmp_dir(), tmp_dir(), private_tmp_dir());
        std::fs::write(logs.join("redis.log"), "legacy\n").unwrap();
        // Only this project's legacy agent makes the shared legacy log file its own.
        let agent = units.join("sh.devy.redis.plist");
        let plist = |dir: &str| {
            format!("<plist><dict><key>WorkingDirectory</key><string>{dir}</string></dict></plist>")
        };
        std::fs::write(&agent, plist(&legacy_data_dir(Path::new("/src/other")))).unwrap();
        assert!(nix_logs(&root, true, &units, &logs, "redis").contains("No logs yet"));
        std::fs::write(&agent, plist(&legacy_data_dir(&root))).unwrap();
        assert_eq!(nix_logs(&root, true, &units, &logs, "redis"), "legacy\n");
    }

    /// `devy logs` against the nix backend with launchd agents, returning the error.
    #[cfg(unix)]
    fn nix_logs_err(root: &Path, units: &Path, logs: &Path, name: &str, follow: bool) -> String {
        let pm = crate::package_manager::NixPackageManager::with_test_dirs(
            root, "app", true, units, logs,
        );
        let config = crate::test_support::make_config(&[name], HashMap::new());
        let err = logs_impl(
            &config,
            root,
            &package_runners(&pm, root),
            &opts(Some(name), 10, follow),
            &mut Vec::new(),
            &AtomicBool::new(true),
            false,
        )
        .unwrap_err();
        format!("{err:#}")
    }

    /// The launchd log directory must be private to the user: one another user owns, or
    /// that others can write to, is refused (in follow mode too) rather than read.
    #[cfg(unix)]
    #[test]
    fn nix_on_macos_refuses_a_log_dir_that_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let (root, units, logs) = (tmp_dir(), tmp_dir(), private_tmp_dir());
        let pm = crate::package_manager::NixPackageManager::with_test_dirs(
            &root, "app", true, &units, &logs,
        );
        let (_, log_file) = pm.unit_and_log_names("redis");
        std::fs::write(logs.join(&log_file), "planted\n").unwrap();
        let foreign = crate::fs_safe::with_fake_owner(crate::fs_safe::current_uid() + 1, || {
            nix_logs_err(&root, &units, &logs, "redis", false)
        });
        assert!(
            foreign.contains("refusing to read service logs")
                && foreign.contains("not owned by the current user"),
            "{foreign}"
        );
        for (mode, follow) in [(0o777, false), (0o770, true), (0o757, false)] {
            std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(mode)).unwrap();
            let err = nix_logs_err(&root, &units, &logs, "redis", follow);
            assert!(err.contains(&format!("has mode {mode:03o}")), "{err}");
            assert!(!err.contains("planted"), "{err}");
        }
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(nix_logs(&root, true, &units, &logs, "redis"), "planted\n");
    }

    /// Before the log directory exists there are no logs, and reading doesn't create it.
    #[test]
    fn nix_on_macos_without_a_log_dir_reports_no_logs_and_creates_nothing() {
        let (root, units, base) = (tmp_dir(), tmp_dir(), tmp_dir());
        let logs = base.join("devy-logs");
        let out = nix_logs(&root, true, &units, &logs, "redis");
        let expected = format!("No logs yet for redis (expected at {}", logs.display());
        assert!(out.contains(&expected), "{out}");
        assert!(!logs.exists());
    }

    /// A launchd log file replaced by a symlink is refused, not followed.
    #[cfg(unix)]
    #[test]
    fn nix_on_macos_refuses_a_symlinked_log_file() {
        let (root, units, logs, outside) = (tmp_dir(), tmp_dir(), private_tmp_dir(), tmp_dir());
        let pm = crate::package_manager::NixPackageManager::with_test_dirs(
            &root, "app", true, &units, &logs,
        );
        let (_, log_file) = pm.unit_and_log_names("redis");
        std::fs::write(outside.join("secret"), "secret\n").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), logs.join(&log_file)).unwrap();
        for follow in [false, true] {
            let err = nix_logs_err(&root, &units, &logs, "redis", follow);
            assert!(err.contains("is a symlink"), "{err}");
            assert!(!err.contains("secret\n"), "{err}");
        }
    }

    #[test]
    fn nix_on_linux_reads_the_per_project_unit_journal() {
        let (root, units) = (tmp_dir(), tmp_dir());
        let pm = crate::package_manager::NixPackageManager::with_test_dirs(
            &root, "app", false, &units, &units,
        );
        let (unit, _) = pm.unit_and_log_names("redis");
        assert!(unit.starts_with("devy-app-") && unit.ends_with("-redis.service"));
        assert_eq!(nix_journal_unit(&root, &units, "redis"), unit);
        // With both units present, the per-project one is read.
        std::fs::write(units.join("devy-redis.service"), "").unwrap();
        std::fs::write(units.join(&unit), "").unwrap();
        assert_eq!(nix_journal_unit(&root, &units, "redis"), unit);
    }

    #[test]
    fn nix_on_linux_falls_back_to_the_legacy_unit_journal() {
        let (root, units) = (tmp_dir(), tmp_dir());
        let unit = |dir: &str| format!("[Service]\nWorkingDirectory={dir}\n");
        let path = units.join("devy-redis.service");
        std::fs::write(&path, unit(&legacy_data_dir(Path::new("/src/other")))).unwrap();
        assert_ne!(
            nix_journal_unit(&root, &units, "redis"),
            "devy-redis.service",
            "another project's legacy unit is not read"
        );
        std::fs::write(&path, unit(&legacy_data_dir(&root))).unwrap();
        assert_eq!(
            nix_journal_unit(&root, &units, "redis"),
            "devy-redis.service"
        );
    }

    #[test]
    fn unsupported_backend_fails_with_its_message() {
        let pm = crate::package_manager::MockPackageManager {
            log_source_result: Some(LogSource::Unsupported("no logs here".into())),
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let config = crate::test_support::make_config(&["mysql"], HashMap::new());
        let err = logs_impl(
            &config,
            &dir,
            &package_runners(&pm, &dir),
            &opts(Some("mysql"), 100, false),
            &mut Vec::new(),
            &AtomicBool::new(false),
            false,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "no logs here");
    }

    #[test]
    fn two_log_files_are_labeled_in_order() {
        let dir = crate::test_support::tmp_dir();
        let (o, e) = (dir.join("out.log"), dir.join("err.log"));
        std::fs::write(&o, "to stdout\n").unwrap();
        std::fs::write(&e, "to stderr\n").unwrap();
        let source = LogSource::Files(vec![o.clone(), e.clone()]);
        let Tail::Text(text) = collect(&source, 10, None).unwrap() else {
            panic!("expected text");
        };
        let at = |s: &str| {
            text.find(s)
                .unwrap_or_else(|| panic!("{s} missing: {text}"))
        };
        assert!(at(&o.display().to_string()) < at("to stdout"));
        assert!(at("to stdout") < at(&e.display().to_string()));
        assert!(at(&e.display().to_string()) < at("to stderr"));
    }

    // ── logs_impl: every service ──────────────────────────────────────────────

    #[test]
    fn every_service_gets_a_section_in_declaration_order() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("redis.log"), "redis says hi\n").unwrap();
        std::fs::write(dir.join("postgresql.log"), "pg says hi\n").unwrap();
        let deps = ["redis", "node", "postgres"];
        let out = run_logs(&dir, "nix", &deps, &opts(None, 100, false)).unwrap();
        let at = |s: &str| out.find(s).unwrap_or_else(|| panic!("{s} missing: {out}"));
        assert!(at("redis") < at("redis says hi"));
        assert!(at("redis says hi") < at("postgres"));
        assert!(at("postgres") < at("pg says hi"));
        assert!(!out.contains("node"), "{out}");
    }

    #[test]
    fn a_service_without_logs_does_not_fail_the_others() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("postgresql.log"), "pg says hi\n").unwrap();
        let o = opts(None, 100, false);
        let out = run_logs(&dir, "nix", &["redis", "postgres"], &o).unwrap();
        assert!(out.contains("No logs yet for redis"), "{out}");
        assert!(out.contains("pg says hi"), "{out}");
    }

    #[test]
    fn follow_streams_every_service_with_prefixes() {
        let dir = crate::test_support::tmp_dir();
        let (redis, pg) = (dir.join("redis.log"), dir.join("postgresql.log"));
        std::fs::write(&redis, "r1\nr2\n").unwrap();
        std::fs::write(&pg, "p1\n").unwrap();
        let pm = FilesPm {
            name: "nix",
            dir: dir.to_path_buf(),
        };
        let config = crate::test_support::make_config(&["redis", "postgres"], HashMap::new());
        let stop = AtomicBool::new(false);
        let mut out = Vec::new();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                let append = |p: &Path, text: &str| {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(p)
                        .unwrap()
                        .write_all(text.as_bytes())
                        .unwrap()
                };
                append(&redis, "r3\n");
                append(&pg, "p2\n");
                std::thread::sleep(Duration::from_millis(800));
                stop.store(true, Ordering::SeqCst);
            });
            logs_impl(
                &config,
                &dir,
                &package_runners(&pm, &dir),
                &opts(None, 1, true),
                &mut out,
                &stop,
                false,
            )
            .unwrap();
        });
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(&lines[..2], ["redis | r2", "postgres | p1"], "{out}");
        assert!(lines.contains(&"redis | r3"), "{out}");
        assert!(lines.contains(&"postgres | p2"), "{out}");
        assert_eq!(lines.len(), 4, "{out}");
    }

    #[test]
    fn following_one_service_has_no_prefix() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("redis.log"), "r1\n").unwrap();
        let pm = FilesPm {
            name: "nix",
            dir: dir.to_path_buf(),
        };
        let config = crate::test_support::make_config(&["redis"], HashMap::new());
        // Already stopped: prints the tail and returns.
        let stop = AtomicBool::new(true);
        let mut out = Vec::new();
        logs_impl(
            &config,
            &dir,
            &package_runners(&pm, &dir),
            &opts(Some("redis"), 10, true),
            &mut out,
            &stop,
            false,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "r1\n");
    }

    // ── a docker service whose container can't be checked ───────────────────────

    use crate::service_runner::docker::{
        CmdOutput as DockerOutput, FakeRunner, fail as docker_fail, ok as docker_ok,
    };

    /// redis runs in docker, nginx through the package manager (logging to `<dir>`).
    const MIXED: &str =
        "name: app\ndependencies:\n  - redis: { service_manager: docker }\n  - nginx\n";

    /// `container inspect` refused by an unreachable daemon, with a hostile escape.
    fn daemon_down() -> DockerOutput {
        docker_fail(
            "Cannot connect to the Docker daemon\x1b]52;c;ZWNobyBoaQ==\x07 at unix:///x.sock",
        )
    }

    /// `container inspect` of a container another project created.
    fn other_projects_container() -> DockerOutput {
        let labels = serde_json::json!({ crate::service_runner::PROJECT_LABEL: "/src/evil" });
        docker_ok(
            &serde_json::json!({"State": {"Running": true}, "Config": {"Labels": labels}})
                .to_string(),
        )
    }

    /// Runs `logs_impl` for [`MIXED`] with the container CLI answering `inspect`.
    fn run_mixed(
        dir: &Path,
        inspect: fn() -> DockerOutput,
        o: &Options,
        out: &mut dyn Write,
        stop: &AtomicBool,
    ) -> Result<()> {
        let config: DevyConfig = serde_norway::from_str(MIXED).unwrap();
        let pm = FilesPm {
            name: "brew",
            dir: dir.to_path_buf(),
        };
        let fake = FakeRunner::new(move |call| {
            assert_eq!(call[1..3], ["container", "inspect"], "only the owner check");
            inspect()
        });
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &fake),
            &config,
            dir,
            None,
            false,
        );
        logs_impl(&config, dir, &runners, o, out, stop, false)
    }

    /// With several services, a docker service whose container can't be checked (the
    /// daemon is down, or the container is another project's) is noted in place, the
    /// other services are still shown, and the command fails at the end.
    #[test]
    fn a_docker_service_that_cannot_be_checked_does_not_hide_the_others() {
        for (inspect, expected) in [
            (
                daemon_down as fn() -> DockerOutput,
                "Cannot connect to the Docker daemon",
            ),
            (other_projects_container, "belongs to another project"),
        ] {
            let dir = crate::test_support::tmp_dir();
            std::fs::write(dir.join("nginx.log"), "nginx up\n").unwrap();
            let mut out = Vec::new();
            let err = run_mixed(
                &dir,
                inspect,
                &opts(None, 10, false),
                &mut out,
                &AtomicBool::new(false),
            )
            .unwrap_err();
            let out = String::from_utf8(out).unwrap();
            assert!(out.contains("redis"), "{out}");
            assert!(out.contains(expected), "{out}");
            assert!(out.contains("nginx up\n"), "{out}");
            assert!(!out.contains('\x1b') && !out.contains('\x07'), "{out:?}");
            assert_eq!(err.to_string(), "Could not read the logs of redis");
        }
    }

    /// With no container, `devy logs` (with or without follow) and `recent` report no
    /// logs yet without running `<cli> logs` (by name) at all: `run_mixed` allows only
    /// the owner check.
    #[test]
    fn a_docker_service_without_a_container_has_no_logs_yet() {
        let no_container = || docker_fail("Error: No such container: devy-app-redis");
        for follow in [false, true] {
            let dir = crate::test_support::tmp_dir();
            let mut out = Vec::new();
            run_mixed(
                &dir,
                no_container,
                &opts(Some("redis"), 10, follow),
                &mut out,
                &AtomicBool::new(false),
            )
            .unwrap();
            let out = String::from_utf8(out).unwrap();
            assert!(out.contains("No logs yet for redis"), "{out}");
        }

        let config: DevyConfig = serde_norway::from_str(MIXED).unwrap();
        let dir = crate::test_support::tmp_dir();
        let pm = FilesPm {
            name: "brew",
            dir: dir.to_path_buf(),
        };
        let fake = FakeRunner::new(move |call| {
            assert_eq!(call[1..3], ["container", "inspect"], "only the owner check");
            no_container()
        });
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &fake),
            &config,
            &dir,
            None,
            false,
        );
        let deps = config.normalized_dependencies().unwrap();
        let redis = deps.iter().find(|d| d.name == "redis").unwrap();
        let tail = recent(
            runners.runner_for(redis),
            redis,
            &dir,
            10,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(tail, Tail::Empty);
    }

    /// With one service, a container that can't be checked is the command's error.
    #[test]
    fn one_docker_service_that_cannot_be_checked_fails_immediately() {
        let dir = crate::test_support::tmp_dir();
        let mut out = Vec::new();
        let err = run_mixed(
            &dir,
            other_projects_container,
            &opts(Some("redis"), 10, false),
            &mut out,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
        assert!(
            err.to_string().contains("belongs to another project"),
            "{err}"
        );
    }

    /// `recent` (doctor, ask) is bounded by its timeout even when the container owner
    /// check never answers: the hung CLI is killed and the error names it.
    #[cfg(unix)]
    #[test]
    fn recent_times_out_when_the_owner_check_hangs() {
        let config: DevyConfig = serde_norway::from_str(MIXED).unwrap();
        let dir = crate::test_support::tmp_dir();
        let pm = FilesPm {
            name: "brew",
            dir: dir.to_path_buf(),
        };
        let hanging = crate::service_runner::docker::HangingRunner;
        let runners = Runners::new(
            &pm,
            ContainerRuntime::new(config.container_cli, &hanging),
            &config,
            &dir,
            None,
            false,
        );
        let deps = config.normalized_dependencies().unwrap();
        let redis = deps.iter().find(|d| d.name == "redis").unwrap();
        let start = Instant::now();
        let err = recent(
            runners.runner_for(redis),
            redis,
            &dir,
            10,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
        assert!(
            err.to_string().contains("`docker` did not answer within"),
            "{err:#}"
        );
    }

    /// In follow mode, a docker service whose container can't be checked is noted while
    /// the others are still followed; once they stop on their own the command fails.
    #[cfg(unix)]
    #[test]
    fn following_continues_past_a_docker_service_that_cannot_be_checked() {
        for (inspect, expected) in [
            (
                daemon_down as fn() -> DockerOutput,
                "Cannot connect to the Docker daemon",
            ),
            (other_projects_container, "belongs to another project"),
        ] {
            let dir = crate::test_support::tmp_dir();
            let nginx = dir.join("nginx.log");
            std::fs::write(&nginx, "nginx up\n").unwrap();
            std::fs::write(dir.join("secret"), "secret\n").unwrap();
            let stop = AtomicBool::new(false);
            let out = SharedOut::default();
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut writer = out.clone();
            let result = std::thread::scope(|scope| {
                let handle = scope
                    .spawn(|| run_mixed(&dir, inspect, &opts(None, 10, true), &mut writer, &stop));
                while !out.text().contains("nginx | nginx up\n") && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&nginx)
                    .unwrap()
                    .write_all(b"nginx later\n")
                    .unwrap();
                while !out.text().contains("nginx | nginx later\n") && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                // End the remaining follower on its own.
                std::fs::remove_file(&nginx).unwrap();
                std::os::unix::fs::symlink(dir.join("secret"), &nginx).unwrap();
                while !handle.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                stop.store(true, Ordering::SeqCst);
                handle.join().unwrap()
            });
            let text = out.text();
            assert!(text.contains("redis | not following:"), "{text}");
            assert!(text.contains(expected), "{text}");
            assert!(text.contains("nginx | nginx later\n"), "{text}");
            assert!(!text.contains('\x1b') && !text.contains('\x07'), "{text:?}");
            // nginx was refused too (swapped for a symlink): both reasons are reported.
            let err = result.unwrap_err().to_string();
            assert_eq!(
                err,
                "Could not follow the logs of redis; \
                 Stopped following a log file devy refused to read"
            );
        }
    }

    #[test]
    fn follow_error_reports_every_reason() {
        assert_eq!(follow_error(&[], &[], false), None);
        assert_eq!(
            follow_error(&["docker"], &[], false).as_deref(),
            Some("`docker` exited with an error")
        );
        assert_eq!(
            follow_error(&[], &["redis", "minio"], false).as_deref(),
            Some("Could not follow the logs of redis, minio")
        );
        assert_eq!(
            follow_error(&[], &[], true).as_deref(),
            Some("Stopped following a log file devy refused to read")
        );
        assert_eq!(
            follow_error(&["docker", "journalctl", "docker"], &["redis"], true).as_deref(),
            Some(
                "`docker` exited with an error; `journalctl` exited with an error; \
                 Could not follow the logs of redis; \
                 Stopped following a log file devy refused to read"
            )
        );
    }

    /// A multi-line error (e.g. a CLI's stderr) noted for one service keeps its later
    /// lines indented under the note, so they can't pass for a `<name> | ` log line or a
    /// service header.
    #[test]
    fn multi_line_service_errors_stay_inside_their_note() {
        let mut out = Vec::new();
        info(&mut out, "first\nnginx | forged\n\nredis\n").unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.ends_with(" first\n    nginx | forged\n    \n    redis\n"),
            "{out:?}"
        );

        fn forged() -> DockerOutput {
            docker_fail("Cannot connect to the Docker daemon\nnginx | forged line\nredis")
        }
        for follow in [false, true] {
            let dir = crate::test_support::tmp_dir();
            let mut out = Vec::new();
            // When following, stop at once: the note is printed before following starts.
            let result = run_mixed(
                &dir,
                forged,
                &opts(None, 10, follow),
                &mut out,
                &AtomicBool::new(follow),
            );
            assert_eq!(result.is_err(), !follow, "{result:?}");
            let out = String::from_utf8(out).unwrap();
            assert!(
                out.contains("\n    nginx | forged line\n    redis\n"),
                "{out}"
            );
            assert!(
                !out.lines().any(|l| l.starts_with("nginx | forged")),
                "{out}"
            );
        }
    }
}
