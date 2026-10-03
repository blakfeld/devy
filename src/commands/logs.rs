//! `devy logs`: prints and follows service logs from wherever each service's backend
//! sends them — files tailed in-process, or `journalctl` / `<container_cli> logs`.
//! Read-only: nothing is started, stopped or written.

use anyhow::{Context, Result, anyhow, bail};
use colored::Colorize;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::config::{Dependency, DevyConfig};
use crate::modules;
use crate::package_manager::{self, LogCommand, LogCommandKind, LogSource};
use crate::service_runner::docker::ContainerRuntime;
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
    let result = logs_impl(
        &config,
        &project_root,
        &runners,
        &opts,
        &mut stdout.lock(),
        &INTERRUPTED,
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
pub(crate) fn logs_impl(
    config: &DevyConfig,
    project_root: &Path,
    runners: &Runners,
    opts: &Options,
    out: &mut dyn Write,
    stop: &AtomicBool,
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

    let mut sources = Vec::new();
    for dep in &services {
        let source = runners
            .runner_for(dep)
            .log_source(dep, opts.lines, opts.follow)?;
        if let LogSource::Unsupported(msg) = source {
            bail!("{msg}");
        }
        sources.push((dep, source));
    }

    if opts.follow {
        return follow(&sources, single, opts.lines, out, stop);
    }
    for (dep, source) in &sources {
        if !single {
            writeln!(out, "\n{}", dep.name.bold())?;
        }
        match collect(source, opts.lines, None)? {
            Tail::Text(text) => {
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
    Ok(())
}

/// `· <msg>`, as `output::info` prints it.
pub(crate) fn info(out: &mut dyn Write, msg: &str) -> std::io::Result<()> {
    writeln!(out, "  {} {}", "·".cyan(), msg)
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

/// The last `lines` lines of `dep`'s log, read through its runner as `devy logs` does.
/// Command sources are killed after `timeout`; an unsupported backend is an error.
pub(crate) fn recent(
    runner: &dyn ServiceRunner,
    dep: &Dependency,
    lines: u32,
    timeout: Duration,
) -> Result<Tail> {
    collect(&runner.log_source(dep, lines, false)?, lines, Some(timeout))
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

/// The last `n` lines of the file at `path` and its length, scanning backwards from the
/// end in `BLOCK`-sized reads. A missing file has no lines.
pub(crate) fn tail_file(path: &Path, n: u32) -> std::io::Result<(Vec<String>, u64)> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((vec![], 0)),
        Err(e) => return Err(e),
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
    let mut command = Command::new(&cmd.program);
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

    let read_all = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let (stdout, stderr) = match merged_reader {
        Some(reader) => (read_all(Box::new(reader)), None),
        None => (
            read_all(Box::new(child.stdout.take().expect("stdout is piped"))),
            Some(read_all(Box::new(
                child.stderr.take().expect("stderr is piped"),
            ))),
        ),
    };

    let deadline = timeout.map(|t| Instant::now() + t);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "`{}` timed out after {} s",
                cmd.program,
                timeout.unwrap_or_default().as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(CmdOutput {
        success: status.success(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr
            .map(|s| s.join().unwrap_or_default())
            .unwrap_or_default(),
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
}

impl FileFollower {
    pub fn new(path: PathBuf, offset: u64) -> Self {
        Self {
            path,
            offset,
            partial: Vec::new(),
        }
    }

    /// Complete lines written since the last poll.
    pub fn poll(&mut self) -> std::io::Result<Vec<String>> {
        let mut file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
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
        let mut buf = Vec::new();
        (&mut file).take(len - self.offset).read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;
        self.partial.extend_from_slice(&buf);
        let mut lines = Vec::new();
        while let Some(i) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line[..i]);
            lines.push(line.strip_suffix('\r').unwrap_or(&line).to_string());
        }
        Ok(lines)
    }
}

/// Prints the last lines of every source, then streams new lines until `stop` is set.
/// With several services each line is prefixed `<name> | `.
fn follow(
    sources: &[(&Dependency, LogSource)],
    single: bool,
    lines: u32,
    out: &mut dyn Write,
    stop: &AtomicBool,
) -> Result<()> {
    let prefix = |name: &str| {
        if single {
            String::new()
        } else {
            format!("{name} | ")
        }
    };

    // Files: print their tails now, then poll from where each tail ended.
    let mut followers: Vec<(String, FileFollower)> = Vec::new();
    for (dep, source) in sources {
        if let LogSource::Files(paths) = source {
            let mut any = false;
            for path in paths {
                let (tail, len) = tail_file(path, lines)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                any |= !tail.is_empty();
                for line in tail {
                    writeln!(out, "{}{line}", prefix(&dep.name))?;
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
        let (tx, rx) = mpsc::channel::<(String, String)>();

        for (prefix, mut follower) in followers {
            let tx = tx.clone();
            scope.spawn(move || {
                while !stop.load(Ordering::SeqCst) && !done.load(Ordering::SeqCst) {
                    for line in follower.poll().unwrap_or_default() {
                        if tx.send((prefix.clone(), line)).is_err() {
                            return;
                        }
                    }
                    std::thread::sleep(POLL);
                }
            });
        }

        let streamed = || -> Result<()> {
            for (dep, source) in sources {
                let LogSource::Command(cmd) = source else {
                    continue;
                };
                let mut child = Command::new(&cmd.program)
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
                        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                            if tx.send((prefix.clone(), line)).is_err() {
                                return;
                            }
                        }
                    });
                }
            }
            drop(tx);
            print_until_stopped(&rx, out, stop)
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
    let failed = children
        .iter_mut()
        .find_map(|(program, child)| match child.try_wait() {
            Ok(Some(status)) if !status.success() => Some(program.clone()),
            _ => None,
        });
    match failed {
        Some(program) => Err(anyhow!("`{program}` exited with an error")),
        None => Ok(()),
    }
}

/// Writes received lines until `stop` is set or every sender is gone.
fn print_until_stopped(
    rx: &mpsc::Receiver<(String, String)>,
    out: &mut dyn Write,
    stop: &AtomicBool,
) -> Result<()> {
    while !stop.load(Ordering::SeqCst) {
        match rx.recv_timeout(POLL) {
            Ok((prefix, line)) => {
                writeln!(out, "{prefix}{line}")?;
                out.flush()?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Dependency;
    use crate::modules::LaunchSpec;
    use crate::package_manager::PackageManager;
    use crate::service_runner::package_runners;
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
        )?;
        Ok(String::from_utf8(out).unwrap())
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
    fn follower_restarts_after_truncation() {
        let dir = crate::test_support::tmp_dir();
        let path = dir.join("a.log");
        std::fs::write(&path, numbered(1..=3)).unwrap();
        let mut f = FileFollower::new(path.clone(), numbered(1..=3).len() as u64);
        std::fs::write(&path, "fresh\n").unwrap();
        assert_eq!(f.poll().unwrap(), ["fresh"]);
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
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "r1\n");
    }
}
