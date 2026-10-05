//! The opt-in AI layer shared by devy's AI commands. Requests run through the user's
//! own `claude` CLI (Claude Code), which owns authentication, model defaults and
//! retries. This module adds redaction of anything that leaves the machine and the
//! request preview behind `--show-context`.
//!
//! Only AI code paths call `Client::from_env`, so no other command looks for `claude`.

pub mod init_prompt;
pub mod redact;

use anyhow::{Result, anyhow, bail};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MODEL_VAR: &str = "DEVY_AI_MODEL";
pub const MISSING_CLI: &str = "the `claude` CLI was not found on PATH — install Claude Code (https://claude.com/claude-code) and sign in to use AI features";

const TIMEOUT: Duration = Duration::from_secs(300);

/// The model named by `DEVY_AI_MODEL`, or `None` to use claude's configured default.
pub fn model_from_env() -> Option<String> {
    non_empty(std::env::var(MODEL_VAR).ok())
}

fn non_empty(model: Option<String>) -> Option<String> {
    model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// One request to `claude`. The same value produces the arguments and stdin that are
/// sent and the `--show-context` preview, so the preview cannot differ from what is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// `None` leaves the model to claude's own configuration.
    pub model: Option<String>,
    pub system: String,
    pub messages: Vec<Message>,
}

impl Request {
    pub fn new(model: Option<&str>, system: String, user: String) -> Self {
        Self {
            model: model.map(String::from),
            system,
            messages: vec![Message {
                role: Role::User,
                content: user,
            }],
        }
    }

    /// Appends the model's reply and a follow-up user turn.
    pub fn follow_up(&self, reply: &str, user: String) -> Self {
        let mut next = self.clone();
        next.messages.push(Message {
            role: Role::Assistant,
            content: reply.to_string(),
        });
        next.messages.push(Message {
            role: Role::User,
            content: user,
        });
        next
    }

    /// Arguments for a single, tool-less, non-persistent `claude -p` turn that loads only
    /// user-level settings: no project or local settings, hooks or `CLAUDE.md`.
    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "json",
            "--tools",
            "",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-session-persistence",
            "--setting-sources",
            "user",
            "--system-prompt",
        ]
        .map(String::from)
        .to_vec();
        args.push(self.system.clone());
        if let Some(model) = &self.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        args
    }

    /// The prompt written to claude's stdin. `claude -p` takes one prompt, so a follow-up
    /// carries the earlier turns as a transcript.
    pub fn prompt(&self) -> String {
        let mut out = String::new();
        for (i, message) in self.messages.iter().enumerate() {
            if i > 0 {
                let heading = match message.role {
                    Role::User => "=== follow-up ===",
                    Role::Assistant => "=== your previous reply ===",
                };
                out.push_str(&format!("\n\n{heading}\n"));
            }
            out.push_str(&message.content);
        }
        out
    }

    /// The complete request as `--show-context` prints it.
    pub fn render_preview(&self) -> String {
        format!(
            "model: {}\n\n=== system ===\n{}\n\n=== prompt ===\n{}\n",
            self.model
                .as_deref()
                .unwrap_or("claude's configured default"),
            self.system,
            self.prompt()
        )
    }
}

/// What the `claude` process produced.
#[derive(Debug, Clone, Default)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `claude` with the given arguments and stdin. `Err` means the process could not
/// run to completion (spawn failure or timeout).
pub trait Transport {
    fn run(&self, args: &[String], stdin: &str) -> std::result::Result<Output, String>;
}

pub struct CliTransport {
    program: PathBuf,
}

impl Transport for CliTransport {
    #[cfg_attr(test, mutants::skip)] // process I/O
    fn run(&self, args: &[String], stdin: &str) -> std::result::Result<Output, String> {
        // An empty working directory keeps the scanned project's .claude settings, hooks
        // and CLAUDE.md out of the session.
        // It is a fresh random-named 0700 directory, removed when `cwd` drops.
        let cwd = crate::fs_safe::PrivateTempDir::new("ai").map_err(|e| format!("{e:#}"))?;
        run_with_timeout(&self.program, args, stdin, cwd.path())
    }
}

#[cfg_attr(test, mutants::skip)] // process I/O
fn run_with_timeout(
    program: &PathBuf,
    args: &[String],
    stdin: &str,
    cwd: &std::path::Path,
) -> std::result::Result<Output, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run claude: {e}"))?;

    let mut child_stdin = child.stdin.take().expect("stdin is piped");
    let input = stdin.to_string();
    let writer = std::thread::spawn(move || {
        let _ = child_stdin.write_all(input.as_bytes());
    });
    let reader = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = pipe.read_to_string(&mut buf);
            buf
        })
    };
    let stdout = reader(Box::new(child.stdout.take().expect("stdout is piped")));
    let stderr = reader(Box::new(child.stderr.take().expect("stderr is piped")));

    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {} s", TIMEOUT.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let _ = writer.join();
    Ok(Output {
        success: status.success(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// A model reply and the model that produced it, as claude reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub model: Option<String>,
}

pub struct Client {
    pub model: Option<String>,
    transport: Box<dyn Transport>,
}

impl Client {
    /// Finds `claude` on PATH and reads `DEVY_AI_MODEL`. Fails with `MISSING_CLI` when
    /// `claude` is not installed.
    #[cfg_attr(test, mutants::skip)] // reads PATH and process env
    pub fn from_env() -> Result<Self> {
        // Never a `claude` the project put on PATH: it would receive the prompt.
        let program =
            crate::fs_safe::which_outside_project("claude").ok_or_else(|| anyhow!(MISSING_CLI))?;
        Ok(Self {
            model: model_from_env(),
            transport: Box::new(CliTransport { program }),
        })
    }

    #[cfg(test)]
    pub fn with_transport(model: Option<&str>, transport: Box<dyn Transport>) -> Self {
        Self {
            model: model.map(String::from),
            transport,
        }
    }

    /// Runs `req` through claude and returns its reply.
    pub fn complete(&self, req: &Request) -> Result<Reply> {
        let out = self
            .transport
            .run(&req.args(), &req.prompt())
            .map_err(|reason| anyhow!("AI request failed: {reason}"))?;
        parse_output(&out)
    }
}

/// Interprets `claude -p --output-format json` output.
fn parse_output(out: &Output) -> Result<Reply> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(out.stdout.trim()) else {
        let detail = [out.stderr.trim(), out.stdout.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .and_then(|s| s.lines().last())
            .unwrap_or("no output");
        bail!(
            "AI request failed: claude {}: {detail}",
            if out.success {
                "returned unreadable output"
            } else {
                "exited with an error"
            }
        );
    };
    let result = value.get("result").and_then(|r| r.as_str()).unwrap_or("");
    let is_error = value.get("is_error").and_then(|e| e.as_bool()) == Some(true)
        || value
            .get("subtype")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s != "success")
        || !out.success;
    if is_error {
        let reason = if result.trim().is_empty() {
            value
                .get("subtype")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown error")
        } else {
            result.trim()
        };
        bail!("AI request failed: {reason}");
    }
    if result.trim().is_empty() {
        bail!("AI request failed: the reply contained no text");
    }
    Ok(Reply {
        text: result.to_string(),
        model: reply_model(&value),
    })
}

/// The model that wrote most of the output, from claude's per-model usage.
fn reply_model(value: &serde_json::Value) -> Option<String> {
    let usage = value.get("modelUsage")?.as_object()?;
    usage
        .iter()
        .max_by_key(|(_, u)| u.get("outputTokens").and_then(|t| t.as_u64()).unwrap_or(0))
        .map(|(model, _)| model.clone())
}

// ── project context files ────────────────────────────────────────────────────

/// The most of one project file [`context_file`] loads: far above anything detection or
/// a prompt (`redact::FILE_CAP`) needs, so a hostile repo cannot make devy load a huge
/// file.
pub(crate) const CONTEXT_READ_LIMIT: u64 = 1024 * 1024;

/// What a prompt shows in place of a project file that is not a context file.
pub(crate) const NOT_INCLUDED: &str = "(not included: not a regular file in the project)\n";

/// Reads `root/rel` for AI context or offline detection. The file counts as absent
/// unless it is a regular file, no component of `rel` is a symlink, and its canonical
/// path lies inside `root`, so `README.md -> ~/.npmrc` is never read. `rel` must be a
/// plain relative path (no `..`, root or drive prefix). At most [`CONTEXT_READ_LIMIT`]
/// bytes are read; a longer file is cut at a character boundary. Unreadable and
/// non-UTF-8 files count as absent.
///
/// Every project file devy reads for `init`, `ask`, `logs --explain` or `doctor` goes
/// through this function (or [`is_context_path`] when the caller reads it itself).
pub(crate) fn context_file(root: &Path, rel: &str) -> Option<String> {
    let rel = Path::new(rel);
    let meta = context_meta(root, rel)?;
    let mut file = open_no_follow(&root.join(rel)).ok()?;
    // The file opened must be the one checked: a swap to a symlink (or anything else)
    // between the check and the open is refused.
    let opened = file.metadata().ok()?;
    if !opened.file_type().is_file() || !same_file(&meta, &opened) {
        return None;
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(CONTEXT_READ_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > CONTEXT_READ_LIMIT {
        bytes.truncate(CONTEXT_READ_LIMIT as usize);
        let valid = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            // Only a character cut by the limit may be incomplete.
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => return None,
        };
        bytes.truncate(valid);
    }
    String::from_utf8(bytes).ok()
}

/// Whether `path` may be included in AI context. A path under `root` must pass the same
/// checks as [`context_file`]; a path elsewhere (a service log under a system log
/// directory) is not a project file and is allowed.
pub(crate) fn is_context_path(root: &Path, path: &Path) -> bool {
    if let Ok(rel) = path.strip_prefix(root) {
        return context_meta(root, rel).is_some();
    }
    // Spelled differently (e.g. `/tmp` vs `/private/tmp`): find the ancestor of `path`
    // that is the project root and check the rest from there, so symlinks below it are
    // still seen. Undecidable paths are refused.
    let (Ok(path), Ok(real_root)) = (std::path::absolute(path), root.canonicalize()) else {
        return false;
    };
    for ancestor in path.ancestors().skip(1) {
        if ancestor.canonicalize().is_ok_and(|a| a == real_root) {
            return path
                .strip_prefix(ancestor)
                .is_ok_and(|rel| context_meta(ancestor, rel).is_some());
        }
    }
    true
}

/// The `symlink_metadata` of `root/rel` when it is a context file (see [`context_file`]).
fn context_meta(root: &Path, rel: &Path) -> Option<std::fs::Metadata> {
    use std::path::Component;
    if rel.as_os_str().is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    // No component may be a symlink, including directories such as `.github`.
    let mut cur = root.to_path_buf();
    let mut meta = None;
    for component in rel.components() {
        cur.push(component);
        let m = std::fs::symlink_metadata(&cur).ok()?;
        if m.file_type().is_symlink() {
            return None;
        }
        meta = Some(m);
    }
    let meta = meta?;
    if !meta.file_type().is_file() {
        return None;
    }
    // Defence in depth against links the checks above cannot see (e.g. a Windows
    // junction on an ancestor): the real path must stay inside the real root. Volumes
    // that cannot be canonicalized rely on the component checks alone.
    if let (Ok(real), Ok(real_root)) = (cur.canonicalize(), root.canonicalize())
        && !real.starts_with(&real_root)
    {
        return None;
    }
    Some(meta)
}

/// Opens `path` for reading without following a symlink at it and without blocking on
/// a FIFO swapped in after the check (see [`crate::fs_safe::open_read_nofollow`]), so a
/// swapped-in link or FIFO fails the caller's checks instead of hanging or redirecting.
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    crate::fs_safe::open_read_nofollow(path)
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// One recorded `claude` invocation: its arguments and stdin.
    type Call = (Vec<String>, String);

    /// Replays canned claude outputs and records every call.
    #[derive(Clone, Default)]
    pub struct FakeTransport {
        pub outputs: Rc<RefCell<Vec<std::result::Result<Output, String>>>>,
        pub calls: Rc<RefCell<Vec<Call>>>,
    }

    impl FakeTransport {
        pub fn new(outputs: Vec<std::result::Result<Output, String>>) -> Self {
            Self {
                outputs: Rc::new(RefCell::new(outputs)),
                calls: Rc::default(),
            }
        }

        pub fn replies(texts: &[&str]) -> Self {
            Self::new(texts.iter().map(|t| Ok(ok(t))).collect())
        }

        pub fn count(&self) -> usize {
            self.calls.borrow().len()
        }

        pub fn stdin(&self, i: usize) -> String {
            self.calls.borrow()[i].1.clone()
        }

        pub fn args(&self, i: usize) -> Vec<String> {
            self.calls.borrow()[i].0.clone()
        }
    }

    impl Transport for FakeTransport {
        fn run(&self, args: &[String], stdin: &str) -> std::result::Result<Output, String> {
            self.calls
                .borrow_mut()
                .push((args.to_vec(), stdin.to_string()));
            let mut outputs = self.outputs.borrow_mut();
            assert!(!outputs.is_empty(), "unexpected request");
            outputs.remove(0)
        }
    }

    pub fn ok(text: &str) -> Output {
        Output {
            success: true,
            stdout: serde_json::json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": text,
                "modelUsage": {"claude-sonnet-5-5": {"outputTokens": 10}},
            })
            .to_string(),
            stderr: String::new(),
        }
    }

    pub fn error(message: &str) -> Output {
        Output {
            success: false,
            stdout: serde_json::json!({
                "type": "result",
                "subtype": "success",
                "is_error": true,
                "result": message,
            })
            .to_string(),
            stderr: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    #[test]
    fn context_file_reads_plain_relative_files_only() {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("README.md"), "# hi\n").unwrap();
        assert_eq!(context_file(&dir, "README.md").as_deref(), Some("# hi\n"));
        assert_eq!(context_file(&dir, "missing"), None);
        assert_eq!(context_file(&dir, ""), None);
        assert_eq!(context_file(&dir, "./README.md"), None);
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(context_file(&sub, "../README.md"), None);
        assert_eq!(
            context_file(&sub, dir.join("README.md").to_str().unwrap()),
            None
        );
        assert_eq!(context_file(&dir, "sub"), None);
    }

    #[cfg(unix)]
    #[test]
    fn context_file_refuses_symlinks_anywhere_on_the_path() {
        let home = crate::test_support::tmp_dir();
        std::fs::write(home.join(".npmrc"), "//registry/:_authToken=npm_secret\n").unwrap();
        let dir = crate::test_support::tmp_dir();
        std::os::unix::fs::symlink(home.join(".npmrc"), dir.join("README.md")).unwrap();
        assert_eq!(context_file(&dir, "README.md"), None);
        assert!(!is_context_path(&dir, &dir.join("README.md")));
        std::os::unix::fs::symlink(&*home, dir.join(".github")).unwrap();
        assert_eq!(context_file(&dir, ".github/.npmrc"), None);
        std::fs::write(dir.join("devy.yml"), "name: x\n").unwrap();
        std::os::unix::fs::symlink(dir.join("devy.yml"), dir.join("devy.lock")).unwrap();
        assert_eq!(context_file(&dir, "devy.lock"), None);
        assert!(is_context_path(&dir, &dir.join("devy.yml")));
    }

    #[cfg(unix)]
    #[test]
    fn context_file_refuses_fifos() {
        let dir = crate::test_support::tmp_dir();
        let status = std::process::Command::new("mkfifo")
            .arg(dir.join("README.md"))
            .status()
            .unwrap();
        assert!(status.success());
        // Must return at once instead of blocking on the open.
        assert_eq!(context_file(&dir, "README.md"), None);
    }

    #[test]
    fn is_context_path_allows_files_outside_the_project() {
        let dir = crate::test_support::tmp_dir();
        let elsewhere = crate::test_support::tmp_dir();
        std::fs::write(elsewhere.join("redis.log"), "up\n").unwrap();
        assert!(is_context_path(&dir, &elsewhere.join("redis.log")));
        assert!(is_context_path(&dir, &elsewhere.join("missing.log")));
        assert!(!is_context_path(&dir, &dir.join("missing.log")));
    }

    #[cfg(unix)]
    #[test]
    fn is_context_path_compares_real_paths() {
        // The same project spelled through a symlinked parent, like `/tmp` on macOS.
        let base = crate::test_support::tmp_dir();
        let real = base.join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, base.join("alias")).unwrap();
        std::fs::write(real.join("a.log"), "x\n").unwrap();
        let outside = crate::test_support::tmp_dir();
        std::fs::write(outside.join("secret"), "s\n").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), real.join("b.log")).unwrap();
        let root = base.join("alias");
        assert!(is_context_path(&root, &real.join("a.log")));
        assert!(!is_context_path(&root, &real.join("b.log")));
    }

    fn req() -> Request {
        Request::new(None, "sys".into(), "hello".into())
    }

    fn client(model: Option<&str>, fake: &FakeTransport) -> Client {
        Client::with_transport(model, Box::new(fake.clone()))
    }

    #[test]
    fn reply_text_and_model_are_returned() {
        let fake = FakeTransport::replies(&["hi"]);
        let reply = client(None, &fake).complete(&req()).unwrap();
        assert_eq!(reply.text, "hi");
        assert_eq!(reply.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(fake.stdin(0), "hello");
    }

    #[test]
    fn args_disable_tools_mcp_and_persistence() {
        let args = Request::new(None, "be brief".into(), "x".into()).args();
        let joined = args.join(" ");
        assert!(
            joined.starts_with("-p --output-format json --tools  "),
            "{joined}"
        );
        for flag in [
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-session-persistence",
        ] {
            assert!(args.contains(&flag.to_string()), "missing {flag}");
        }
        let i = args.iter().position(|a| a == "--setting-sources").unwrap();
        assert_eq!(args[i + 1], "user");
        let i = args.iter().position(|a| a == "--system-prompt").unwrap();
        assert_eq!(args[i + 1], "be brief");
        assert!(!args.contains(&"--model".to_string()));
    }

    #[test]
    fn model_override_is_passed() {
        let args = Request::new(Some("claude-opus-5-5"), "s".into(), "u".into()).args();
        assert!(args.ends_with(&["--model".to_string(), "claude-opus-5-5".to_string()]));
    }

    #[test]
    fn empty_model_override_means_default() {
        assert_eq!(non_empty(Some("  ".into())), None);
        assert_eq!(non_empty(None), None);
        assert_eq!(non_empty(Some("opus".into())).as_deref(), Some("opus"));
    }

    #[test]
    fn claude_error_reports_its_message() {
        let fake = FakeTransport::new(vec![Ok(error("Not logged in · Please run /login"))]);
        let err = client(None, &fake)
            .complete(&req())
            .unwrap_err()
            .to_string();
        assert_eq!(err, "AI request failed: Not logged in · Please run /login");
    }

    #[test]
    fn non_json_failure_reports_stderr() {
        let fake = FakeTransport::new(vec![Ok(Output {
            success: false,
            stdout: String::new(),
            stderr: "warning\nerror: unknown option '--tools'\n".into(),
        })]);
        let err = client(None, &fake)
            .complete(&req())
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "AI request failed: claude exited with an error: error: unknown option '--tools'"
        );
    }

    #[test]
    fn spawn_failure_or_timeout_reports_reason() {
        let fake = FakeTransport::new(vec![Err("timed out after 300 s".into())]);
        let err = client(None, &fake)
            .complete(&req())
            .unwrap_err()
            .to_string();
        assert_eq!(err, "AI request failed: timed out after 300 s");
    }

    #[test]
    fn empty_result_is_an_error() {
        let fake = FakeTransport::replies(&["  "]);
        let err = client(None, &fake)
            .complete(&req())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no text"), "{err}");
    }

    #[test]
    fn reply_model_picks_main_model() {
        let value = serde_json::json!({"modelUsage": {
            "claude-haiku-4-5": {"outputTokens": 5},
            "claude-opus-5-5": {"outputTokens": 900},
        }});
        assert_eq!(reply_model(&value).as_deref(), Some("claude-opus-5-5"));
        assert_eq!(reply_model(&serde_json::json!({})), None);
    }

    #[test]
    fn follow_up_sends_transcript() {
        let r = req().follow_up("a1", "fix it".into());
        assert_eq!(
            r.prompt(),
            "hello\n\n=== your previous reply ===\na1\n\n=== follow-up ===\nfix it"
        );
    }

    #[test]
    fn preview_shows_exactly_what_is_sent() {
        let r = Request::new(
            Some("claude-opus-5-5"),
            "system rules".into(),
            "files".into(),
        )
        .follow_up("first", "fix".into());
        let preview = r.render_preview();
        let args = r.args();
        let i = args.iter().position(|a| a == "--system-prompt").unwrap();
        assert!(preview.contains(&format!("=== system ===\n{}\n", args[i + 1])));
        assert!(preview.contains(&format!("=== prompt ===\n{}\n", r.prompt())));
        assert!(preview.starts_with("model: claude-opus-5-5\n"));
        let default = Request::new(None, "s".into(), "u".into()).render_preview();
        assert!(default.starts_with("model: claude's configured default\n"));
    }
}
