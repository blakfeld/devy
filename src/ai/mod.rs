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
use std::path::PathBuf;
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

    /// Arguments for a single, tool-less, non-persistent `claude -p` turn.
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
        let cwd = std::env::temp_dir().join(format!("devy-ai-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).map_err(|e| e.to_string())?;
        let result = run_with_timeout(&self.program, args, stdin, &cwd);
        let _ = std::fs::remove_dir_all(&cwd);
        result
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
        let program = which::which("claude").map_err(|_| anyhow!(MISSING_CLI))?;
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
