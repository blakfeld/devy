//! `devy ask` and `devy logs <name> --explain`: questions about the project environment,
//! answered by Claude from a bounded, redacted snapshot of the configuration, status and
//! service logs. Read-only: nothing is installed, started, stopped or written.

use anyhow::{Result, bail};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use crate::ai::{self, Request, redact};
use crate::config::{Dependency, DevyConfig};
use crate::modules;
use crate::output;
use crate::package_manager::{self, LogSource};
use crate::service_runner::Runners;
use crate::service_runner::docker::ContainerRuntime;

use super::doctor::BACKEND_NOTES;
use super::logs::{self, Tail};
use super::{failure_record, ports, service, shared};

/// Log lines collected per service for `devy ask`.
const ASK_LOG_LINES: u32 = 50;
/// How long one service's log command may run while collecting context.
const LOG_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on the context; the oldest log lines are dropped first to meet it.
pub(crate) const CONTEXT_CAP: usize = 60 * 1024;

const ASK_SYSTEM: &str = "You answer questions about a developer environment managed by devy, a \
declarative developer environment manager. Use the reference and the environment context in the \
prompt. Prefer concrete devy commands and devy.yml edits. Say when the context is not enough to \
answer and what is missing. Be brief.";

const EXPLAIN_SYSTEM: &str = "You diagnose a failing service in a project managed by devy, a \
declarative developer environment manager, from its recent logs and configuration. Reply with the \
most likely cause, the log lines that show it, and numbered steps to fix it, with shell commands \
in backticks. Say so when the logs don't show the cause. Be brief.";

/// What the AI layer needs, injected so tests use a fake transport.
pub(crate) struct Ai<'a> {
    pub client: &'a dyn Fn() -> Result<ai::Client>,
    /// Whether stdout is a terminal; the progress indicator is shown only then.
    pub is_tty: bool,
}

impl Ai<'static> {
    #[cfg_attr(test, mutants::skip)] // reads the real terminal and PATH
    pub fn system() -> Self {
        Ai {
            client: &ai::Client::from_env,
            is_tty: std::io::stdout().is_terminal(),
        }
    }
}

#[cfg_attr(test, mutants::skip)] // binds the real devy.yml, backends and claude; logic is in ask_with
pub fn run(question: &str, show_context: bool) -> Result<()> {
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
    let context = collect_context(&config, &project_root, &runners, Scope::Project);
    ask_with(
        &context,
        question,
        show_context,
        &Ai::system(),
        &mut std::io::stdout(),
    )
}

/// Asks `question` about the environment described by `context`, or with
/// `show_context` prints the request instead of sending it.
pub(crate) fn ask_with(
    context: &str,
    question: &str,
    show_context: bool,
    ai: &Ai<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let user = format!(
        "{BACKEND_NOTES}\n\n{context}\n=== question ===\n{}\n",
        redact::text(question.trim())
    );
    send(ASK_SYSTEM, user, show_context, ai, out)
}

/// `devy logs <name> --explain`: sends the service's last `--lines` log lines and its
/// configuration for a diagnosis. Nothing is sent when the service has no logs.
pub(crate) fn explain_impl(
    config: &DevyConfig,
    project_root: &Path,
    runners: &Runners,
    opts: &logs::Options,
    ai: &Ai<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let Some(name) = opts.name.as_deref() else {
        bail!("--explain needs a service name");
    };
    let lines = opts.lines;
    let dep = service::resolve_service(config, name, runners.package.pm(), project_root, false)?;
    let source = runners.runner_for(&dep).log_source(&dep, lines, false)?;
    if let LogSource::Unsupported(msg) = &source {
        bail!("{msg}");
    }
    let log = match logs::collect(&source, lines, None)? {
        Tail::Text(text) => text,
        Tail::Empty => {
            logs::info(out, &format!("No logs yet for {}", dep.name))?;
            return Ok(());
        }
    };
    let context = collect_context(
        config,
        project_root,
        runners,
        Scope::Service {
            dep: &dep,
            log: &log,
            lines,
        },
    );
    let user = format!(
        "{BACKEND_NOTES}\n\n{context}\n=== task ===\nExplain why {} is failing, based on its logs above.\n",
        dep.name
    );
    send(EXPLAIN_SYSTEM, user, opts.show_context, ai, out)
}

/// Prints the request (`show_context`) or sends it and prints the answer.
fn send(
    system: &str,
    user: String,
    show_context: bool,
    ai: &Ai<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    if show_context {
        let preview = Request::new(ai::model_from_env().as_deref(), system.into(), user);
        out.write_all(preview.render_preview().as_bytes())?;
        return Ok(());
    }
    let client = (ai.client)()?;
    if ai.is_tty {
        output::step(&format!(
            "Asking claude{}…",
            client
                .model
                .as_deref()
                .map(|m| format!(" ({m})"))
                .unwrap_or_default()
        ));
    }
    let req = Request::new(client.model.as_deref(), system.into(), user);
    let reply = client.complete(&req)?;
    writeln!(out, "{}", reply.text.trim_end())?;
    Ok(())
}

// ── context ──────────────────────────────────────────────────────────────────

/// How much of the environment the context covers.
pub(crate) enum Scope<'a> {
    /// Every dependency, with the last 50 log lines of each service.
    Project,
    /// One service (port already resolved) and its already-collected log.
    Service {
        dep: &'a Dependency,
        log: &'a str,
        lines: u32,
    },
}

/// One service's log, or why it has none.
enum LogEntry {
    Lines(Vec<String>),
    Note(String),
}

/// Builds the redacted, size-capped environment context sent with a question.
pub(crate) fn collect_context(
    config: &DevyConfig,
    project_root: &Path,
    runners: &Runners,
    scope: Scope<'_>,
) -> String {
    let mut head = format!(
        "=== environment ===\nPlatform: {}\nPackage manager: {}\n",
        failure_record::platform(),
        runners.package.pm().name()
    );
    let lock = ports::load_lock(project_root).ok().flatten();
    let mut logs: Vec<(String, LogEntry)> = Vec::new();

    let log_heading = match scope {
        Scope::Project => {
            let devy_yml = std::fs::read_to_string(project_root.join("devy.yml"))
                .unwrap_or_else(|e| format!("(could not be read: {e})"));
            let devy_lock = std::fs::read_to_string(project_root.join(crate::lock::PATH)).ok();
            section(&mut head, "devy.yml", &devy_yml);
            section(
                &mut head,
                "devy.lock",
                devy_lock.as_deref().unwrap_or("(not present)\n"),
            );
            head.push_str(&status_section(config, runners));

            let services: Vec<Dependency> = config
                .normalized_dependencies()
                .unwrap_or_default()
                .into_iter()
                .filter(|d| modules::get(&d.name).is_service())
                .collect();
            for dep in &services {
                logs.push((dep.name.clone(), service_log(runners, dep)));
            }
            format!("service logs (last {ASK_LOG_LINES} lines each)")
        }
        Scope::Service { dep, log, lines } => {
            let name = &dep.name;
            section(&mut head, &format!("{name} in devy.yml"), &dep_entry(dep));
            let locked = lock
                .as_ref()
                .and_then(|l| l.get(modules::canonical_name(name)))
                .and_then(|entry| serde_yml::to_string(entry).ok())
                .unwrap_or_else(|| "(not locked)\n".into());
            section(&mut head, &format!("{name} in devy.lock"), &locked);
            let running = match runners.runner_for(dep).is_running(dep) {
                Ok(true) => "yes".to_string(),
                Ok(false) => "no".to_string(),
                Err(e) => format!("unknown ({e:#})"),
            };
            let module = modules::get(name);
            let port = module
                .port_key()
                .and_then(|key| {
                    modules::helpers::extra_port(dep, key, module.default_port().unwrap_or(0)).ok()
                })
                .map_or("none".to_string(), |p| p.to_string());
            section(
                &mut head,
                &format!("{name} status"),
                &format!("running: {running}\nport: {port}\n"),
            );
            logs.push((name.clone(), LogEntry::Lines(redacted_lines(log))));
            format!("{name} logs (last {lines} lines)")
        }
    };

    let mut out = head;
    out.push_str(&format!("\n=== {log_heading} ===\n"));
    if logs.is_empty() {
        out.push_str("no services declared\n");
    }
    let dropped = fit_logs(&mut logs, CONTEXT_CAP.saturating_sub(out.len()));
    for (name, entry) in &logs {
        out.push_str(&format!("--- {name} ---\n"));
        if dropped.contains(name) {
            out.push_str("(earlier lines dropped to fit the size limit)\n");
        }
        match entry {
            LogEntry::Lines(lines) => lines.iter().for_each(|l| {
                out.push_str(l);
                out.push('\n');
            }),
            LogEntry::Note(note) => {
                out.push_str(note);
                out.push('\n');
            }
        }
    }
    out
}

/// `=== title ===` followed by the redacted, per-file-capped `content`.
fn section(out: &mut String, title: &str, content: &str) {
    out.push_str(&format!("\n=== {title} ===\n"));
    out.push_str(&redact::cap_file(&redact::text(content)));
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

/// One line per dependency: installed and, for services, running.
fn status_section(config: &DevyConfig, runners: &Runners) -> String {
    let mut s = String::new();
    match config.normalized_dependencies() {
        Err(e) => s.push_str(&format!("(dependencies could not be read: {e:#})\n")),
        Ok(deps) if deps.is_empty() => s.push_str("none declared\n"),
        Ok(deps) => {
            let (rows, err) = shared::dep_rows(&deps, &[], runners);
            for row in rows {
                let mut line = format!(
                    "{}: {}",
                    row.label,
                    if row.installed {
                        "installed"
                    } else {
                        "not installed"
                    }
                );
                match row.running {
                    Some(true) => line.push_str(", running"),
                    Some(false) => line.push_str(", stopped"),
                    None => {}
                }
                s.push_str(&line);
                s.push('\n');
            }
            if let Some(e) = err {
                s.push_str(&format!("(status check stopped early: {e:#})\n"));
            }
        }
    }
    let mut out = String::new();
    section(&mut out, "dependency status", &s);
    out
}

/// The last `ASK_LOG_LINES` lines of `dep`'s log, collected as `devy logs` does, or a
/// note saying why there are none.
fn service_log(runners: &Runners, dep: &Dependency) -> LogEntry {
    match logs::recent(runners.runner_for(dep), dep, ASK_LOG_LINES, LOG_TIMEOUT) {
        Ok(Tail::Text(text)) => LogEntry::Lines(redacted_lines(&text)),
        Ok(Tail::Empty) => LogEntry::Note("(no log output yet)".into()),
        Err(e) => LogEntry::Note(format!(
            "(logs unavailable: {})",
            redact::text(&format!("{e:#}"))
        )),
    }
}

fn redacted_lines(text: &str) -> Vec<String> {
    redact::text(text).lines().map(String::from).collect()
}

/// `dep` as a devy.yml entry, with docker management made explicit.
fn dep_entry(dep: &Dependency) -> String {
    let mut map = serde_yml::Mapping::new();
    let mut put = |k: &str, v: serde_yml::Value| {
        map.insert(serde_yml::Value::String(k.into()), v);
    };
    put("name", dep.name.clone().into());
    if let Some(v) = &dep.version {
        put("version", v.clone().into());
    }
    if let Some(image) = &dep.image {
        put("image", image.clone().into());
    }
    put(
        "service_manager",
        if dep.docker { "docker" } else { "package" }.into(),
    );
    let mut keys: Vec<&String> = dep.extra.keys().collect();
    keys.sort();
    for key in keys {
        put(key, dep.extra[key].clone());
    }
    serde_yml::to_string(&map).unwrap_or_default()
}

/// Drops the oldest log lines, longest log first, until the logs fit in `budget` bytes.
/// Returns the names of the logs that lost lines.
fn fit_logs(logs: &mut [(String, LogEntry)], budget: usize) -> Vec<String> {
    let size = |logs: &[(String, LogEntry)]| -> usize {
        logs.iter()
            .map(|(name, entry)| {
                name.len()
                    + 60
                    + match entry {
                        LogEntry::Lines(lines) => lines.iter().map(|l| l.len() + 1).sum(),
                        LogEntry::Note(note) => note.len() + 1,
                    }
            })
            .sum()
    };
    let mut total = size(logs);
    let mut dropped: Vec<String> = Vec::new();
    while total > budget {
        let longest = logs
            .iter_mut()
            .filter_map(|(name, entry)| match entry {
                LogEntry::Lines(lines) if !lines.is_empty() => Some((name, lines)),
                _ => None,
            })
            .max_by_key(|(_, lines)| lines.iter().map(String::len).sum::<usize>());
        let Some((name, lines)) = longest else {
            break;
        };
        total -= lines.remove(0).len() + 1;
        if !dropped.contains(name) {
            dropped.push(name.clone());
        }
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::fake::{self, FakeTransport};
    use crate::package_manager::MockPackageManager;
    use crate::service_runner::package_runners;

    const WINGET_MSG: &str = "Logs are not available for winget-managed services — check Windows Event Viewer or the service's own log directory";

    /// A project directory holding `yaml` as devy.yml, and its parsed config.
    fn project(yaml: &str) -> (crate::test_support::TempDir, DevyConfig) {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("devy.yml"), yaml).unwrap();
        (dir, serde_yml::from_str(yaml).unwrap())
    }

    fn files_pm(path: &Path) -> MockPackageManager {
        MockPackageManager {
            name: "nix",
            installed: true,
            service_running: true,
            log_source_result: Some(LogSource::Files(vec![path.to_path_buf()])),
            ..Default::default()
        }
    }

    fn no_client() -> Result<ai::Client> {
        panic!("the AI client must not be created")
    }

    fn ai_with(fake: &FakeTransport) -> impl Fn() -> Result<ai::Client> {
        let fake = fake.clone();
        move || Ok(ai::Client::with_transport(None, Box::new(fake.clone())))
    }

    // ── collect_context ───────────────────────────────────────────────────────

    #[test]
    fn context_has_config_status_and_logs() {
        let (dir, config) = project("dependencies:\n  - redis\n  - node\n");
        std::fs::write(dir.join("redis.log"), "Ready to accept connections\n").unwrap();
        let pm = files_pm(&dir.join("redis.log"));
        let ctx = collect_context(&config, &dir, &package_runners(&pm, &dir), Scope::Project);
        for needle in [
            "Package manager: nix",
            &format!("Platform: {}", failure_record::platform()),
            "=== devy.yml ===\ndependencies:\n  - redis\n",
            "=== devy.lock ===\n(not present)\n",
            "redis: installed, running\n",
            "node: installed\n",
            "=== service logs (last 50 lines each) ===\n--- redis ---\nReady to accept connections\n",
        ] {
            assert!(ctx.contains(needle), "missing {needle:?} in:\n{ctx}");
        }
        assert_eq!(*pm.log_queries.borrow(), [("redis".into(), 50, false)]);
    }

    #[test]
    fn context_redacts_secrets() {
        let (dir, config) = project(
            "dependencies:\n  - redis\nenvironment:\n  API_TOKEN: abc123\n  DATABASE_URL: postgres://u:hunter2@localhost/db\n",
        );
        std::fs::write(
            dir.join("redis.log"),
            "connecting to redis://default:s3cret@127.0.0.1:6379\n",
        )
        .unwrap();
        let pm = files_pm(&dir.join("redis.log"));
        let ctx = collect_context(&config, &dir, &package_runners(&pm, &dir), Scope::Project);
        for secret in ["abc123", "hunter2", "s3cret"] {
            assert!(!ctx.contains(secret), "{secret} leaked:\n{ctx}");
        }
        assert!(ctx.contains(redact::REDACTED), "{ctx}");
    }

    #[test]
    fn unavailable_logs_are_a_note() {
        let (dir, config) = project("dependencies:\n  - mysql\n");
        let pm = MockPackageManager {
            name: "winget",
            log_source_result: Some(LogSource::Unsupported(WINGET_MSG.into())),
            ..Default::default()
        };
        let ctx = collect_context(&config, &dir, &package_runners(&pm, &dir), Scope::Project);
        assert!(
            ctx.contains(&format!(
                "--- mysql ---\n(logs unavailable: {WINGET_MSG})\n"
            )),
            "{ctx}"
        );
        let empty = files_pm(&dir.join("missing.log"));
        let ctx = collect_context(
            &config,
            &dir,
            &package_runners(&empty, &dir),
            Scope::Project,
        );
        assert!(
            ctx.contains("--- mysql ---\n(no log output yet)\n"),
            "{ctx}"
        );
    }

    #[test]
    fn context_is_capped_by_dropping_the_oldest_log_lines() {
        let (dir, config) = project("dependencies:\n  - redis\n  - mysql\n  - postgres\n");
        let log: String = (1..=50)
            .map(|i| format!("{i:02} {}\n", "x".repeat(2000)))
            .collect();
        std::fs::write(dir.join("big.log"), &log).unwrap();
        let pm = files_pm(&dir.join("big.log"));
        let ctx = collect_context(&config, &dir, &package_runners(&pm, &dir), Scope::Project);
        assert!(ctx.len() <= CONTEXT_CAP, "{} > {CONTEXT_CAP}", ctx.len());
        assert!(ctx.contains("(earlier lines dropped to fit the size limit)"));
        let newest = format!("50 {}", "x".repeat(2000));
        assert_eq!(
            ctx.matches(&newest).count(),
            3,
            "each service keeps its newest line"
        );
        assert!(
            !ctx.contains(&format!("\n01 {}", "x".repeat(10))),
            "oldest lines go first"
        );
    }

    // ── ask ───────────────────────────────────────────────────────────────────

    #[test]
    fn question_and_context_reach_the_request() {
        let fake = FakeTransport::replies(&["Redis is listening on 6379."]);
        let client = ai_with(&fake);
        let ai = Ai {
            client: &client,
            is_tty: false,
        };
        let mut out = Vec::new();
        ask_with(
            "CONTEXT-MARKER\n",
            "  why can't my app reach redis?  ",
            false,
            &ai,
            &mut out,
        )
        .unwrap();
        assert_eq!(fake.count(), 1);
        let stdin = fake.stdin(0);
        assert!(stdin.contains("CONTEXT-MARKER"), "{stdin}");
        assert!(
            stdin.ends_with("=== question ===\nwhy can't my app reach redis?\n"),
            "{stdin}"
        );
        let args = fake.args(0);
        let i = args.iter().position(|a| a == "--system-prompt").unwrap();
        assert_eq!(args[i + 1], ASK_SYSTEM);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Redis is listening on 6379.\n"
        );
    }

    #[test]
    fn show_context_prints_the_request_without_claude() {
        let ai = Ai {
            client: &no_client,
            is_tty: true,
        };
        let mut out = Vec::new();
        ask_with("CONTEXT-MARKER\n", "is postgres ok?", true, &ai, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("=== system ===\n"), "{out}");
        assert!(out.contains("CONTEXT-MARKER"), "{out}");
        assert!(out.contains("is postgres ok?"), "{out}");
    }

    #[test]
    fn missing_claude_is_an_error() {
        let ai = Ai {
            client: &|| Err(anyhow::anyhow!(ai::MISSING_CLI)),
            is_tty: false,
        };
        let err = ask_with("c", "q", false, &ai, &mut Vec::new()).unwrap_err();
        assert_eq!(err.to_string(), ai::MISSING_CLI);
    }

    #[test]
    fn request_failures_keep_the_ai_error() {
        let fake = FakeTransport::new(vec![Ok(fake::error("Not logged in · Please run /login"))]);
        let client = ai_with(&fake);
        let ai = Ai {
            client: &client,
            is_tty: false,
        };
        let mut out = Vec::new();
        let err = ask_with("c", "q", false, &ai, &mut out).unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            "AI request failed: Not logged in · Please run /login"
        );
        assert!(out.is_empty(), "nothing is printed on failure");
    }

    // ── explain ───────────────────────────────────────────────────────────────

    fn explain_opts(name: &str, lines: u32, show_context: bool) -> logs::Options {
        logs::Options {
            name: Some(name.into()),
            lines,
            follow: false,
            explain: true,
            show_context,
        }
    }

    #[test]
    fn explain_sends_the_services_logs_and_config() {
        let (dir, config) =
            project("dependencies:\n  - postgres:\n      version: \"16\"\n      port: 5433\n");
        let log: String = (1..=30)
            .map(|i| format!("log line {i}\n"))
            .collect::<String>()
            + "FATAL: database files are incompatible with server\n";
        std::fs::write(dir.join("pg.log"), log).unwrap();
        let pm = files_pm(&dir.join("pg.log"));
        let fake =
            FakeTransport::replies(&["The data directory was initialized by PostgreSQL 15."]);
        let client = ai_with(&fake);
        let ai = Ai {
            client: &client,
            is_tty: false,
        };
        let mut out = Vec::new();
        explain_impl(
            &config,
            &dir,
            &package_runners(&pm, &dir),
            &explain_opts("postgresql", 10, false),
            &ai,
            &mut out,
        )
        .unwrap();
        let stdin = fake.stdin(0);
        for needle in [
            "=== postgres in devy.yml ===\nname: postgres\nversion: '16'\nservice_manager: package\nport: 5433\n",
            "=== postgres in devy.lock ===\n(not locked)\n",
            "=== postgres status ===\nrunning: yes\nport: 5433\n",
            "=== postgres logs (last 10 lines) ===\n",
            "FATAL: database files are incompatible with server\n",
            "Explain why postgres is failing",
        ] {
            assert!(stdin.contains(needle), "missing {needle:?} in:\n{stdin}");
        }
        assert!(
            !stdin.contains("log line 21\n"),
            "only --lines lines: {stdin}"
        );
        assert!(stdin.contains("log line 22\n"), "{stdin}");
        assert_eq!(*pm.log_queries.borrow(), [("postgresql".into(), 10, false)]);
        let args = fake.args(0);
        let i = args.iter().position(|a| a == "--system-prompt").unwrap();
        assert_eq!(args[i + 1], EXPLAIN_SYSTEM);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "The data directory was initialized by PostgreSQL 15.\n"
        );
    }

    #[test]
    fn explain_without_logs_skips_claude() {
        let (dir, config) = project("dependencies:\n  - redis\n");
        let pm = files_pm(&dir.join("missing.log"));
        let ai = Ai {
            client: &no_client,
            is_tty: false,
        };
        for show_context in [false, true] {
            let mut out = Vec::new();
            explain_impl(
                &config,
                &dir,
                &package_runners(&pm, &dir),
                &explain_opts("redis", 100, show_context),
                &ai,
                &mut out,
            )
            .unwrap();
            assert!(
                String::from_utf8(out)
                    .unwrap()
                    .contains("No logs yet for redis"),
                "show_context={show_context}"
            );
        }
    }

    #[test]
    fn explain_show_context_previews_the_request() {
        let (dir, config) = project("dependencies:\n  - redis\n");
        std::fs::write(dir.join("r.log"), "oom\n").unwrap();
        let pm = files_pm(&dir.join("r.log"));
        let ai = Ai {
            client: &no_client,
            is_tty: false,
        };
        let mut out = Vec::new();
        explain_impl(
            &config,
            &dir,
            &package_runners(&pm, &dir),
            &explain_opts("redis", 100, true),
            &ai,
            &mut out,
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains(EXPLAIN_SYSTEM) && out.contains("oom\n"),
            "{out}"
        );
    }

    #[test]
    fn explain_on_winget_fails_with_its_message() {
        let (dir, config) = project("dependencies:\n  - mysql\n");
        let pm = MockPackageManager {
            name: "winget",
            log_source_result: Some(LogSource::Unsupported(WINGET_MSG.into())),
            ..Default::default()
        };
        let ai = Ai {
            client: &no_client,
            is_tty: false,
        };
        let err = explain_impl(
            &config,
            &dir,
            &package_runners(&pm, &dir),
            &explain_opts("mysql", 100, false),
            &ai,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.to_string(), WINGET_MSG);
    }
}
