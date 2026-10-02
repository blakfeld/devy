//! `devy doctor`: diagnoses the project's environment and its most recent failed
//! `devy up`. devy's own checks always run; a diagnosis from Claude is added when the
//! `claude` CLI is available. The only file doctor may write is devy.yml, and only after
//! the user accepts a validated fix.

use anyhow::{Context, Result, anyhow, bail};
use colored::Colorize;
use serde::Deserialize;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use crate::ai::{self, Request, init_prompt, redact};
use crate::config::DevyConfig;
use crate::env_manager::{EnvManager, Shadowenv};
use crate::lock::LockFile;
use crate::modules;
use crate::output;
use crate::package_manager::{self, PackageManager};
use crate::service_runner::docker::ContainerRuntime;

use super::check::{self, Findings};
use super::failure_record::{self, FailureRecord};
use super::ports::{self, PortMode};

/// Log lines sent per affected service.
const LOG_LINES: usize = 50;

/// How doctor uses AI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiMode {
    /// Ask Claude when there is something to diagnose and `claude` is available.
    On,
    /// `--no-ai`.
    Off,
    /// `--show-context`: print the request instead of sending it.
    Preview,
}

pub struct Options {
    pub yes: bool,
    pub ai: AiMode,
}

/// Recent log output for a service.
pub trait LogSource {
    /// Up to the last `lines` lines of `service`'s log, or `None` when none are available.
    fn tail(&self, project_root: &Path, service: &str, lines: usize) -> Option<String>;
}

/// No service logs: the retrieval API arrives with the service-logs work. Until then
/// every affected service is reported as having no logs available.
pub struct NoLogs;

impl LogSource for NoLogs {
    fn tail(&self, _: &Path, _: &str, _: usize) -> Option<String> {
        None
    }
}

/// Selects the package manager for a config, as `package_manager::detect` does.
pub(crate) type DetectFn = dyn Fn(&DevyConfig, &Path) -> Result<Box<dyn PackageManager>>;

/// What doctor inspects and talks to, injected so tests use mocks.
pub(crate) struct Deps<'a> {
    pub detect: &'a DetectFn,
    pub env_mgr: &'a dyn EnvManager,
    pub client: &'a dyn Fn() -> Result<ai::Client>,
    pub logs: &'a dyn LogSource,
    /// Answers to the apply prompt.
    pub input: &'a mut dyn BufRead,
    pub is_tty: bool,
}

#[cfg_attr(test, mutants::skip)] // binds stdin, the real backends and claude; logic is in run_with
pub fn run(yes: bool, no_ai: bool, show_context: bool) -> Result<()> {
    let start = std::env::current_dir().context("Failed to get current directory")?;
    let config_path = DevyConfig::find_config(&start)
        .ok_or_else(|| anyhow!("devy.yml not found — are you inside a devy project?"))?;
    let project_root = config_path
        .parent()
        .ok_or_else(|| anyhow!("devy.yml has no parent directory"))?
        .to_path_buf();
    let ai = if show_context {
        AiMode::Preview
    } else if no_ai {
        AiMode::Off
    } else {
        AiMode::On
    };
    let stdin = std::io::stdin();
    let is_tty = stdin.is_terminal();
    let mut input = stdin.lock();
    run_with(
        &config_path,
        &project_root,
        Options { yes, ai },
        Deps {
            detect: &|config, root| package_manager::detect(config, root),
            env_mgr: &Shadowenv,
            client: &ai::Client::from_env,
            logs: &NoLogs,
            input: &mut input,
            is_tty,
        },
    )
}

pub(crate) fn run_with(
    config_path: &Path,
    project_root: &Path,
    opts: Options,
    deps: Deps<'_>,
) -> Result<()> {
    let text = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let parsed: Result<DevyConfig> = serde_yml::from_str(&text)
        .with_context(|| format!("Failed to parse {}", config_path.display()));
    let config = parsed.as_ref().ok();
    let name = config.and_then(|c| c.name.as_deref()).unwrap_or("project");
    output::header(&format!("devy doctor · {name}"));

    // Checks: the same evaluation as `devy check`, with hard errors as findings.
    output::header("Checks");
    let mut pm = None;
    let findings = match &parsed {
        Err(e) => Findings::from_error(anyhow!("{e:#}")),
        Ok(config) => match (deps.detect)(config, project_root) {
            Err(e) => Findings::from_error(e),
            Ok(found) => {
                let findings = check::collect_findings(
                    config,
                    found.as_ref(),
                    ContainerRuntime::system(config.container_cli),
                    deps.env_mgr,
                    project_root,
                );
                pm = Some(found);
                findings
            }
        },
    };
    findings.print()?;
    if let Some(e) = &findings.hard_error {
        output::warn(&format!("{e:#}"));
    }
    let issues = findings.issues();
    if issues.is_empty() {
        output::success("all checks passed");
    }

    let record = failure_record::load(project_root).unwrap_or_else(|e| {
        output::warn(&format!("{e:#}"));
        None
    });
    if let Some(record) = &record {
        output::header("Last devy up failure");
        print_record(record);
    }

    output::blank_line();
    if issues.is_empty() && record.is_none() {
        output::success("no problems found");
        return Ok(());
    }

    let bundle = || Bundle {
        record: record.as_ref(),
        issues: &issues,
        warnings: findings.warnings(),
        devy_yml: &text,
        devy_lock: std::fs::read_to_string(project_root.join(crate::lock::PATH)).ok(),
        backend: pm
            .as_ref()
            .map(|p| p.name().to_string())
            .or_else(|| record.as_ref().and_then(|r| r.backend.clone())),
        logs: affected_services(record.as_ref(), &findings)
            .into_iter()
            .map(|s| {
                let lines = deps.logs.tail(project_root, &s, LOG_LINES);
                (s, lines)
            })
            .collect(),
    };

    let client = match opts.ai {
        AiMode::Off => {
            output::info("AI diagnosis unavailable — disabled with --no-ai");
            return Ok(());
        }
        AiMode::Preview => {
            let preview = request(&bundle(), ai::model_from_env().as_deref()).render_preview();
            // A closed pipe (e.g. `| head`) just ends the preview early.
            let _ = std::io::stdout().write_all(preview.as_bytes());
            return Ok(());
        }
        AiMode::On => match (deps.client)() {
            Ok(client) => client,
            Err(e) => {
                output::info(&format!("AI diagnosis unavailable — {e}"));
                return Ok(());
            }
        },
    };

    output::step(&format!(
        "Asking claude{} for a diagnosis",
        client
            .model
            .as_deref()
            .map(|m| format!(" ({m})"))
            .unwrap_or_default()
    ));
    let (diagnosis, model) = match diagnose(&client, &request(&bundle(), client.model.as_deref())) {
        Ok(d) => d,
        Err(e) => {
            let cause = format!("{e:#}");
            let cause = cause.strip_prefix("AI request failed: ").unwrap_or(&cause);
            output::warn(&format!("AI diagnosis failed: {cause}"));
            return Ok(());
        }
    };
    output::header(&format!(
        "Diagnosis (AI-generated with {model} — review before acting)"
    ));
    print!("{}", render_diagnosis(&diagnosis));

    let Some(proposed) = diagnosis.proposed_config() else {
        return Ok(());
    };
    let validated = validate_with_backend(&proposed, config, pm.as_deref(), project_root, &deps);
    if let Err(e) = validated {
        output::warn(&format!(
            "suggested devy.yml change was invalid and was not offered: {e:#}"
        ));
        return Ok(());
    }
    let Some(diff) = unified_diff(&text, &proposed) else {
        return Ok(());
    };
    output::header(&format!("Suggested fix (AI-generated with {model})"));
    print_diff(&diff);
    offer_fix(config_path, &proposed, opts.yes, deps.input, deps.is_tty)
}

fn print_record(record: &FailureRecord) {
    output::info(&format!("when: {}", record.timestamp));
    if let Some(step) = &record.step {
        output::info(&format!("step: {step}"));
    }
    if let Some(dep) = &record.dependency {
        output::info(&format!("dependency: {dep}"));
    }
    output::info(&format!("error: {}", record.error_chain));
}

/// Services named in the failure record or reported stopped, by their devy.yml name.
fn affected_services(record: Option<&FailureRecord>, findings: &Findings) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        let canonical = modules::canonical_name(name);
        if !names
            .iter()
            .any(|n| modules::canonical_name(n) == canonical)
        {
            names.push(name.to_string());
        }
    };
    if let Some(dep) = record.and_then(|r| r.dependency.as_deref())
        && modules::get(dep).is_service()
    {
        push(dep);
    }
    for row in findings.deps.iter().flatten() {
        if row.running == Some(false) {
            push(&row.dep);
        }
    }
    names
}

// ── request ──────────────────────────────────────────────────────────────────

/// Everything a diagnosis request may contain.
pub(crate) struct Bundle<'a> {
    pub record: Option<&'a FailureRecord>,
    pub issues: &'a [String],
    pub warnings: Vec<String>,
    pub devy_yml: &'a str,
    pub devy_lock: Option<String>,
    pub backend: Option<String>,
    /// (service, recent log lines) for affected services only.
    pub logs: Vec<(String, Option<String>)>,
}

/// Kept short: it travels on the command line. The reference goes on stdin.
const SYSTEM: &str = "You diagnose problems in projects managed by devy, a declarative developer \
environment manager. Follow the reference and the reply format in the prompt exactly.";

const BACKEND_NOTES: &str = "Backend notes:
- nix (default on macOS and Linux): packages install into the project-local profile .devy/nix-profile from nixpkgs; versions map to versioned attributes (e.g. nodejs_22) and an unmapped version falls back to the nixpkgs default. Services run as launchd agents (macOS) or systemd --user units (Linux), with data under .devy/data.
- brew (macOS) and apt (Linux): system packages; services use brew services or systemd. Ports set in devy.yml cannot be applied to every service with these backends.
- winget (Windows).
- docker/podman: with service_manager: docker, services run as per-project containers; images are pinned by digest in devy.lock.
- devy.lock records resolved versions and assigned service ports. A port conflict means two services resolve to the same port (explicit in devy.yml or assigned in devy.lock).
- Environment variables are written for shadowenv to .shadowenv.d/500_devy.lisp.";

const REPLY_RULES: &str = r#"Reply with only this JSON object, with no other text and no code fence:
{"summary": "<one paragraph explaining what went wrong>", "likely_cause": "<one or two sentences>", "steps": ["<step>", "..."], "devy_yml": "<the complete corrected devy.yml>" or null}
- steps: concrete actions in order. Put shell commands in backticks; the user runs them, devy never does.
- devy_yml: only when a change to devy.yml fixes or works around the problem, otherwise null. Return the whole file and keep its comments, key order and formatting apart from the fix.
- Never write secret values."#;

pub(crate) fn request(bundle: &Bundle<'_>, model: Option<&str>) -> Request {
    Request::new(model, SYSTEM.to_string(), user_content(bundle))
}

fn user_content(b: &Bundle<'_>) -> String {
    let catalog = serde_json::to_string(&modules::catalog()).expect("catalog serializes");
    let mut out = format!(
        "{}\n\nModule catalog (JSON):\n{catalog}\n\n{BACKEND_NOTES}\n\n{REPLY_RULES}\n\n\
         Diagnose this devy project.\n\n\
         Platform: {}\nBackend: {}\ndevy version: {}\n",
        init_prompt::SCHEMA,
        failure_record::platform(),
        b.backend.as_deref().unwrap_or("unknown"),
        env!("CARGO_PKG_VERSION"),
    );

    out.push_str("\n=== last devy up failure ===\n");
    match b.record {
        None => out.push_str("none recorded\n"),
        Some(r) => {
            let mut s = format!("when: {}\n", r.timestamp);
            for (label, value) in [
                ("step", &r.step),
                ("dependency", &r.dependency),
                ("backend", &r.backend),
            ] {
                if let Some(v) = value {
                    s.push_str(&format!("{label}: {v}\n"));
                }
            }
            s.push_str(&format!(
                "platform: {}\ndevy version: {}\nerror: {}\n",
                r.platform, r.devy_version, r.error_chain
            ));
            out.push_str(&redact::cap_file(&redact::text(&s)));
        }
    }

    out.push_str("\n=== findings from devy's checks ===\n");
    let mut findings = String::new();
    if b.issues.is_empty() {
        findings.push_str("issues: none\n");
    } else {
        findings.push_str("issues:\n");
        b.issues
            .iter()
            .for_each(|i| findings.push_str(&format!("- {i}\n")));
    }
    if !b.warnings.is_empty() {
        findings.push_str("warnings:\n");
        b.warnings
            .iter()
            .for_each(|w| findings.push_str(&format!("- {w}\n")));
    }
    out.push_str(&redact::cap_file(&redact::text(&findings)));

    for (name, content) in [
        ("devy.yml", Some(b.devy_yml)),
        ("devy.lock", b.devy_lock.as_deref()),
    ] {
        out.push_str(&format!("\n=== {name} ===\n"));
        match content {
            Some(c) => out.push_str(&redact::cap_file(&redact::text(c))),
            None => out.push_str("(not present)\n"),
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    out.push_str("\n=== service logs ===\n");
    if b.logs.is_empty() {
        out.push_str("no affected services\n");
    }
    for (service, lines) in &b.logs {
        match lines {
            Some(l) => {
                out.push_str(&format!("--- {service} (last {LOG_LINES} lines) ---\n"));
                out.push_str(&redact::cap_file(&redact::text(&last_lines(l, LOG_LINES))));
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str(&format!("--- {service} ---\nno logs available\n")),
        }
    }
    redact::cap_total(&out)
}

fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

// ── reply ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Diagnosis {
    pub summary: String,
    pub likely_cause: String,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub devy_yml: Option<String>,
}

impl Diagnosis {
    /// The proposed devy.yml, newline-terminated, when there is one.
    fn proposed_config(&self) -> Option<String> {
        let text = self.devy_yml.as_deref()?;
        if text.trim().is_empty() {
            return None;
        }
        let mut text = text.to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Some(text)
    }
}

/// Reads the JSON object in `text`, tolerating a code fence or surrounding prose.
pub(crate) fn parse_diagnosis(text: &str) -> Result<Diagnosis> {
    let (start, end) = text
        .find('{')
        .zip(text.rfind('}'))
        .filter(|(s, e)| s < e)
        .ok_or_else(|| anyhow!("the reply contained no JSON object"))?;
    serde_json::from_str(&text[start..=end]).context("the reply was not the requested JSON")
}

/// Asks for a diagnosis, asking once more if the reply is not the requested JSON.
/// Returns the diagnosis and the model that wrote it.
fn diagnose(client: &ai::Client, req: &Request) -> Result<(Diagnosis, String)> {
    let reply = client.complete(req)?;
    let (diagnosis, reply) = match parse_diagnosis(&reply.text) {
        Ok(d) => (d, reply),
        Err(e) => {
            let retry = req.follow_up(
                &reply.text,
                format!("devy could not read that reply ({e:#}). Reply with only the JSON object described above."),
            );
            let reply = client.complete(&retry)?;
            (parse_diagnosis(&reply.text)?, reply)
        }
    };
    let model = reply
        .model
        .or_else(|| client.model.clone())
        .unwrap_or_else(|| "claude".to_string());
    Ok((diagnosis, model))
}

/// The diagnosis as printed under its header. Steps are text only: nothing is run.
pub(crate) fn render_diagnosis(d: &Diagnosis) -> String {
    let mut out = format!(
        "  {}\n\n  {} {}\n",
        d.summary.trim(),
        "Likely cause:".bold(),
        d.likely_cause.trim()
    );
    if !d.steps.is_empty() {
        out.push_str(&format!("\n  {}\n", "Suggested steps:".bold()));
        for (i, step) in d.steps.iter().enumerate() {
            out.push_str(&format!("    {}. {}\n", i + 1, step.trim()));
        }
    }
    out
}

// ── suggested fix ────────────────────────────────────────────────────────────

/// Checks that `text` is a devy.yml that `devy check` would not reject outright:
/// it parses, normalizes, passes the backend's config validation, has no unknown keys or
/// invalid shells, and its ports resolve without conflict. Install, service and
/// environment state are not considered.
pub(crate) fn validate_proposed_config(
    text: &str,
    pm: &dyn PackageManager,
    lock: Option<&LockFile>,
) -> Result<DevyConfig> {
    let config: DevyConfig = serde_yml::from_str(text).context("it does not parse")?;
    let issues = check::static_issues(&config)?;
    if !issues.is_empty() {
        bail!("{}", issues.join("; "));
    }
    let mut deps = config.normalized_dependencies()?;
    for dep in deps.iter().filter(|d| !d.docker) {
        pm.validate_config(dep)
            .with_context(|| format!("{}: config validation failed", dep.name))?;
    }
    ports::resolve_and_check(&mut deps, lock, pm, PortMode::ReadOnly)?;
    Ok(config)
}

/// `validate_proposed_config` with the current backend, or the one the proposal selects
/// when it changes `package_manager`.
fn validate_with_backend(
    text: &str,
    current: Option<&DevyConfig>,
    pm: Option<&dyn PackageManager>,
    project_root: &Path,
    deps: &Deps<'_>,
) -> Result<()> {
    let lock = ports::load_lock(project_root).ok().flatten();
    let proposed: DevyConfig = serde_yml::from_str(text).context("it does not parse")?;
    let same_backend = current.is_some_and(|c| c.package_manager == proposed.package_manager);
    match pm {
        Some(pm) if same_backend => validate_proposed_config(text, pm, lock.as_ref()).map(drop),
        _ => {
            let pm = (deps.detect)(&proposed, project_root)?;
            validate_proposed_config(text, pm.as_ref(), lock.as_ref()).map(drop)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Same,
    Removed,
    Added,
}

/// A unified diff of `old` against `new` with three lines of context, or `None` when the
/// two are identical. Line-based LCS; inputs are small config files.
pub(crate) fn unified_diff(old: &str, new: &str) -> Option<String> {
    if old == new {
        return None;
    }
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = diff_ops(&a, &b);
    if ops.iter().all(|(op, _)| *op == Op::Same) {
        return None; // differs only in a trailing newline
    }

    const CONTEXT: usize = 3;
    // Line numbers (0-based) in `a` and `b` at the start of each op.
    let mut positions = Vec::with_capacity(ops.len() + 1);
    let (mut i, mut j) = (0usize, 0usize);
    for (op, _) in &ops {
        positions.push((i, j));
        match op {
            Op::Same => (i, j) = (i + 1, j + 1),
            Op::Removed => i += 1,
            Op::Added => j += 1,
        }
    }
    positions.push((i, j));

    let changes: Vec<usize> = (0..ops.len()).filter(|&k| ops[k].0 != Op::Same).collect();
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &k in &changes {
        let start = k.saturating_sub(CONTEXT);
        let end = (k + CONTEXT + 1).min(ops.len());
        match hunks.last_mut() {
            Some(last) if start <= last.1 => last.1 = end,
            _ => hunks.push((start, end)),
        }
    }

    let mut out = String::from("--- devy.yml\n+++ devy.yml (suggested)\n");
    for (start, end) in hunks {
        let slice = &ops[start..end];
        let old_len = slice.iter().filter(|(op, _)| *op != Op::Added).count();
        let new_len = slice.iter().filter(|(op, _)| *op != Op::Removed).count();
        let (old_at, new_at) = positions[start];
        let old_start = if old_len == 0 { old_at } else { old_at + 1 };
        let new_start = if new_len == 0 { new_at } else { new_at + 1 };
        out.push_str(&format!(
            "@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"
        ));
        for (op, line) in slice {
            let prefix = match op {
                Op::Same => ' ',
                Op::Removed => '-',
                Op::Added => '+',
            };
            out.push(prefix);
            out.push_str(line);
            out.push('\n');
        }
    }
    Some(out)
}

fn diff_ops<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(Op, &'a str)> {
    let (n, m) = (a.len(), b.len());
    // lcs[i][j]: LCS length of a[i..] and b[j..].
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push((Op::Same, a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push((Op::Removed, a[i]));
            i += 1;
        } else {
            ops.push((Op::Added, b[j]));
            j += 1;
        }
    }
    ops.extend(a[i..].iter().map(|l| (Op::Removed, *l)));
    ops.extend(b[j..].iter().map(|l| (Op::Added, *l)));
    ops
}

fn print_diff(diff: &str) {
    for line in diff.lines() {
        let styled = if line.starts_with("---") || line.starts_with("+++") {
            line.bold().to_string()
        } else if line.starts_with("@@") {
            line.cyan().to_string()
        } else if line.starts_with('-') {
            line.red().to_string()
        } else if line.starts_with('+') {
            line.green().to_string()
        } else {
            line.to_string()
        };
        println!("  {styled}");
    }
}

/// Applies `proposed` to `path` with `--yes` or an interactive yes; never otherwise.
pub(crate) fn offer_fix(
    path: &Path,
    proposed: &str,
    yes: bool,
    input: &mut dyn BufRead,
    is_tty: bool,
) -> Result<()> {
    output::blank_line();
    if !yes {
        if !is_tty {
            output::info("not applied — re-run with --yes to apply");
            return Ok(());
        }
        print!("  Apply this change to devy.yml? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        input.read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            output::info("not applied");
            return Ok(());
        }
    }
    write_preserving_permissions(path, proposed)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    output::success("updated devy.yml — run devy up to apply it");
    Ok(())
}

/// Replaces `path` atomically (temp file in the same directory, then rename), keeping
/// its permissions. A read-only file is refused rather than replaced.
fn write_preserving_permissions(path: &Path, content: &str) -> Result<()> {
    let perms = std::fs::metadata(path)?.permissions();
    if perms.readonly() {
        bail!("the file is read-only");
    }
    let tmp = path.with_file_name(format!(".devy.yml.{}.tmp", std::process::id()));
    let result = std::fs::write(&tmp, content)
        .and_then(|()| std::fs::set_permissions(&tmp, perms))
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::fake::FakeTransport;
    use crate::env_manager::MockEnvManager;
    use crate::package_manager::MockPackageManager;
    use crate::test_support::TempDir;

    fn project(yaml: &str) -> TempDir {
        let dir = crate::test_support::tmp_dir();
        std::fs::write(dir.join("devy.yml"), yaml).unwrap();
        dir
    }

    fn record_failure(dir: &Path, dependency: Option<&str>) {
        let progress = super::super::up::UpProgress {
            step: Some("start services"),
            dependency: dependency.map(String::from),
            backend: Some("brew".into()),
        };
        let err = anyhow!("not ready").context("Failed to start postgresql service");
        failure_record::write(dir, &FailureRecord::new(&err, &progress)).unwrap();
    }

    fn running_pm(_: &DevyConfig, _: &Path) -> Result<Box<dyn PackageManager>> {
        Ok(Box::new(MockPackageManager {
            name: "brew",
            installed: true,
            service_running: true,
            ..Default::default()
        }))
    }

    fn missing_pm(_: &DevyConfig, _: &Path) -> Result<Box<dyn PackageManager>> {
        Ok(Box::new(MockPackageManager {
            name: "brew",
            ..Default::default()
        }))
    }

    fn no_claude() -> Result<ai::Client> {
        Err(anyhow!(ai::MISSING_CLI))
    }

    fn must_not_ask() -> Result<ai::Client> {
        panic!("claude must not be looked up")
    }

    struct FakeLogs;
    impl LogSource for FakeLogs {
        fn tail(&self, _: &Path, service: &str, _: usize) -> Option<String> {
            Some(format!("starting\nLOG-{service}\n"))
        }
    }

    /// Runs doctor in `dir` and returns its result and every warning it printed.
    struct Run<'a> {
        dir: &'a Path,
        ai: AiMode,
        yes: bool,
        detect: &'a DetectFn,
        client: &'a dyn Fn() -> Result<ai::Client>,
        input: &'a str,
        is_tty: bool,
    }

    impl<'a> Run<'a> {
        fn new(dir: &'a Path) -> Self {
            Self {
                dir,
                ai: AiMode::On,
                yes: false,
                detect: &running_pm,
                client: &must_not_ask,
                input: "",
                is_tty: false,
            }
        }

        fn go(&self) -> (Result<()>, Vec<String>) {
            let mut input = std::io::Cursor::new(self.input.as_bytes().to_vec());
            let mut result = None;
            let warnings = crate::output::with_warn_messages(|| {
                result = Some(run_with(
                    &self.dir.join("devy.yml"),
                    self.dir,
                    Options {
                        yes: self.yes,
                        ai: self.ai,
                    },
                    Deps {
                        detect: self.detect,
                        env_mgr: &MockEnvManager::default(),
                        client: self.client,
                        logs: &FakeLogs,
                        input: &mut input,
                        is_tty: self.is_tty,
                    },
                ));
            });
            (result.unwrap(), warnings)
        }
    }

    fn fake(replies: &[&str]) -> FakeTransport {
        FakeTransport::replies(replies)
    }

    fn client_for(fake: &FakeTransport) -> impl Fn() -> Result<ai::Client> + '_ {
        move || Ok(ai::Client::with_transport(None, Box::new(fake.clone())))
    }

    fn reply(devy_yml: Option<&str>, steps: &[&str]) -> String {
        serde_json::json!({
            "summary": "postgresql did not start.",
            "likely_cause": "Its port is taken.",
            "steps": steps,
            "devy_yml": devy_yml,
        })
        .to_string()
    }

    // ── offline paths ─────────────────────────────────────────────────────────

    #[test]
    fn healthy_project_makes_no_ai_request() {
        let dir = project("name: shop\ndependencies:\n  - jq\n");
        let (result, warnings) = Run::new(&dir).go();
        assert!(result.is_ok());
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn no_ai_never_looks_up_claude() {
        let dir = project("name: shop\ndependencies:\n  - jq\n");
        record_failure(&dir, None);
        let run = Run {
            ai: AiMode::Off,
            detect: &missing_pm,
            ..Run::new(&dir)
        };
        assert!(run.go().0.is_ok());
    }

    #[test]
    fn missing_claude_still_succeeds() {
        let dir = project("name: shop\ndependencies:\n  - jq\n");
        let run = Run {
            detect: &missing_pm,
            client: &no_claude,
            ..Run::new(&dir)
        };
        assert!(run.go().0.is_ok());
    }

    #[test]
    fn invalid_yaml_is_a_finding_and_reaches_diagnosis() {
        let dir = project("name: [unclosed\n");
        let fake = fake(&[&reply(None, &[])]);
        let client = client_for(&fake);
        let run = Run {
            client: &client,
            ..Run::new(&dir)
        };
        let (result, warnings) = run.go();
        assert!(result.is_ok(), "{result:?}");
        assert!(
            warnings.iter().any(|w| w.starts_with("Failed to parse")),
            "{warnings:?}"
        );
        assert_eq!(fake.count(), 1, "the diagnosis step still runs");
        assert!(fake.stdin(0).contains("Failed to parse"));
    }

    #[test]
    fn port_conflict_is_a_finding_not_an_error() {
        let dir =
            project("dependencies:\n  - redis: { port: 5432 }\n  - postgresql: { port: 5432 }\n");
        let run = Run {
            ai: AiMode::Off,
            ..Run::new(&dir)
        };
        let (result, warnings) = run.go();
        assert!(result.is_ok());
        assert!(
            warnings.iter().any(|w| w.contains("port conflict")),
            "{warnings:?}"
        );
    }

    #[test]
    fn doctor_writes_nothing_but_devy_yml() {
        let dir = project("name: shop\ndependencies:\n  - jq\nenvironment:\n  A: b\n");
        record_failure(&dir, None);
        let before = std::fs::read_to_string(failure_record::path(&dir)).unwrap();
        let run = Run {
            ai: AiMode::Off,
            detect: &missing_pm,
            ..Run::new(&dir)
        };
        run.go().0.unwrap();
        let mut entries: Vec<String> = std::fs::read_dir(&*dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries, vec![".devy", "devy.yml"]);
        assert_eq!(
            std::fs::read_to_string(failure_record::path(&dir)).unwrap(),
            before,
            "the failure record is left as it was"
        );
    }

    // ── request bundle ────────────────────────────────────────────────────────

    #[test]
    fn request_includes_only_affected_service_logs_and_redacts() {
        let dir = project(
            "name: shop\ndependencies:\n  - postgresql\n  - redis\nenvironment:\n  DB_PASSWORD: hunter2\n",
        );
        std::fs::create_dir_all(dir.join(".shadowenv.d")).unwrap();
        std::fs::write(
            dir.join(".shadowenv.d/500_devy.lisp"),
            "(env/set \"SHADOW_SENTINEL\" \"1\")",
        )
        .unwrap();
        std::fs::write(
            dir.join(crate::lock::PATH),
            "version: 1\ndependencies: {}\n",
        )
        .unwrap();
        record_failure(&dir, Some("postgresql"));
        let fake = fake(&[&reply(None, &[])]);
        let client = client_for(&fake);
        let run = Run {
            client: &client,
            ..Run::new(&dir)
        };
        run.go().0.unwrap();
        let sent = fake.stdin(0);
        assert!(sent.contains("LOG-postgresql"), "{sent}");
        assert!(!sent.contains("LOG-redis"), "healthy services send no logs");
        assert!(
            !sent.contains("SHADOW_SENTINEL"),
            "the environment file is never sent"
        );
        assert!(!sent.contains("hunter2"), "secrets are redacted");
        assert!(sent.contains("DB_PASSWORD: <redacted>"), "{sent}");
        assert!(sent.contains("=== devy.lock ===\nversion: 1"), "{sent}");
        assert!(sent.contains("step: start services"), "{sent}");
        assert!(sent.contains("Backend: brew"), "{sent}");
    }

    #[test]
    fn stopped_services_are_affected_and_missing_logs_are_noted() {
        let dir = project("name: shop\ndependencies:\n  - redis\n");
        let findings = check::collect_findings(
            &serde_yml::from_str("dependencies:\n  - redis\n  - jq\n").unwrap(),
            &MockPackageManager {
                installed: true,
                ..Default::default()
            },
            ContainerRuntime::system(Default::default()),
            &MockEnvManager::default(),
            &dir,
        );
        assert_eq!(affected_services(None, &findings), vec!["redis"]);

        let issues = vec!["redis: service stopped".to_string()];
        let bundle = Bundle {
            record: None,
            issues: &issues,
            warnings: vec![],
            devy_yml: "dependencies:\n  - redis\n",
            devy_lock: None,
            backend: None,
            logs: vec![("redis".into(), None)],
        };
        let content = user_content(&bundle);
        assert!(
            content.contains("--- redis ---\nno logs available\n"),
            "{content}"
        );
        assert!(
            content.contains("=== devy.lock ===\n(not present)"),
            "{content}"
        );
    }

    #[test]
    fn failed_non_service_dependency_sends_no_logs() {
        let record = FailureRecord::new(
            &anyhow!("boom"),
            &super::super::up::UpProgress {
                step: Some("install"),
                dependency: Some("jq".into()),
                backend: None,
            },
        );
        assert!(affected_services(Some(&record), &Findings::default()).is_empty());
    }

    #[test]
    fn last_lines_keeps_the_tail() {
        let text: String = (1..=60).map(|i| format!("l{i}\n")).collect();
        let tail = last_lines(&text, 50);
        assert!(tail.starts_with("l11\n") && tail.ends_with("l60"), "{tail}");
    }

    // ── reply parsing ─────────────────────────────────────────────────────────

    #[test]
    fn parse_diagnosis_accepts_fenced_json_and_prose() {
        let text = format!(
            "Here you go:\n```json\n{}\n```\n",
            reply(Some("a: b\n"), &["x"])
        );
        let d = parse_diagnosis(&text).unwrap();
        assert_eq!(d.likely_cause, "Its port is taken.");
        assert_eq!(d.steps, vec!["x"]);
        assert_eq!(d.devy_yml.as_deref(), Some("a: b\n"));
    }

    #[test]
    fn parse_diagnosis_rejects_non_json() {
        assert!(parse_diagnosis("I think postgres is down.").is_err());
        assert!(parse_diagnosis("{\"summary\": 1}").is_err());
    }

    fn failing_project() -> TempDir {
        let dir = project("name: shop\ndependencies:\n  - jq\n");
        record_failure(&dir, None);
        dir
    }

    #[test]
    fn malformed_reply_is_retried_once() {
        let dir = failing_project();
        let fake = fake(&["not json", &reply(None, &["restart"])]);
        let client = client_for(&fake);
        let (result, warnings) = Run {
            client: &client,
            ..Run::new(&dir)
        }
        .go();
        assert!(result.is_ok());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(fake.count(), 2);
        assert!(fake.stdin(1).contains("devy could not read that reply"));
    }

    #[test]
    fn twice_malformed_reply_warns_and_exits_ok() {
        let dir = failing_project();
        let fake = fake(&["not json", "still not json"]);
        let client = client_for(&fake);
        let (result, warnings) = Run {
            client: &client,
            ..Run::new(&dir)
        }
        .go();
        assert!(result.is_ok());
        assert_eq!(fake.count(), 2, "only one retry");
        assert_eq!(
            warnings,
            vec!["AI diagnosis failed: the reply contained no JSON object".to_string()]
        );
    }

    #[test]
    fn request_error_warns_and_exits_ok() {
        let dir = failing_project();
        let fake = FakeTransport::new(vec![Err("timed out after 300 s".into())]);
        let client = client_for(&fake);
        let (result, warnings) = Run {
            client: &client,
            ..Run::new(&dir)
        }
        .go();
        assert!(result.is_ok());
        assert_eq!(
            warnings,
            vec!["AI diagnosis failed: timed out after 300 s".to_string()]
        );
    }

    // ── diagnosis output ──────────────────────────────────────────────────────

    #[test]
    fn render_diagnosis_numbers_steps() {
        let d =
            parse_diagnosis(&reply(None, &["Run `nix-collect-garbage`", "Run devy up"])).unwrap();
        let text = render_diagnosis(&d);
        assert!(text.contains("postgresql did not start."), "{text}");
        assert!(text.contains("Its port is taken."), "{text}");
        assert!(
            text.contains("    1. Run `nix-collect-garbage`\n"),
            "{text}"
        );
        assert!(text.contains("    2. Run devy up\n"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn suggested_commands_are_never_run() {
        let dir = failing_project();
        let marker = dir.join("ran");
        let step = format!("Run `touch {}`", marker.display());
        let fake = fake(&[&reply(None, &[&step])]);
        let client = client_for(&fake);
        Run {
            client: &client,
            yes: true,
            ..Run::new(&dir)
        }
        .go()
        .0
        .unwrap();
        assert!(
            !marker.exists(),
            "doctor must not execute suggested commands"
        );
    }

    // ── suggested fix ─────────────────────────────────────────────────────────

    fn validate(text: &str) -> Result<DevyConfig> {
        validate_proposed_config(
            text,
            &MockPackageManager {
                name: "brew",
                ..Default::default()
            },
            None,
        )
    }

    #[test]
    fn validate_accepts_valid_config() {
        assert!(validate("name: x\ndependencies:\n  - mysql: { port: 3307 }\n").is_ok());
    }

    #[test]
    fn validate_rejects_multi_key_entry() {
        let err = validate("dependencies:\n  - node: {}\n    redis: {}\n").unwrap_err();
        assert!(err.to_string().contains("multiple keys"), "{err}");
    }

    #[test]
    fn validate_rejects_unknown_key() {
        let err = validate("dependencies:\n  - redis: { prot: 1 }\n").unwrap_err();
        assert!(
            err.to_string().contains("unrecognized config key `prot`"),
            "{err}"
        );
    }

    #[test]
    fn validate_rejects_port_conflict() {
        let err =
            validate("dependencies:\n  - redis: { port: 5432 }\n  - postgresql: { port: 5432 }\n")
                .unwrap_err();
        assert!(format!("{err:#}").contains("port conflict"), "{err:#}");
    }

    #[test]
    fn validate_rejects_unparseable_yaml() {
        assert!(validate("dependencies: [\n").is_err());
    }

    #[test]
    fn diff_shows_changed_line_with_context() {
        let old = "name: x\ndependencies:\n  - mysql:\n      port: 3306\n  - jq\n";
        let new = "name: x\ndependencies:\n  - mysql:\n      port: 3307\n  - jq\n";
        assert_eq!(
            unified_diff(old, new).unwrap(),
            "--- devy.yml\n+++ devy.yml (suggested)\n@@ -1,5 +1,5 @@\n name: x\n dependencies:\n   - mysql:\n-      port: 3306\n+      port: 3307\n   - jq\n"
        );
    }

    #[test]
    fn diff_shows_added_lines() {
        let diff = unified_diff("a\nb\n", "a\nb\nc\n").unwrap();
        assert!(diff.ends_with("@@ -1,2 +1,3 @@\n a\n b\n+c\n"), "{diff}");
    }

    #[test]
    fn diff_shows_removed_lines() {
        let diff = unified_diff("a\nb\nc\n", "a\nc\n").unwrap();
        assert!(diff.ends_with("@@ -1,3 +1,2 @@\n a\n-b\n c\n"), "{diff}");
    }

    #[test]
    fn diff_splits_distant_changes_into_hunks() {
        let old: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l2\n", "L2\n").replace("l19\n", "L19\n");
        let diff = unified_diff(&old, &new).unwrap();
        assert_eq!(diff.matches("@@ ").count(), 2, "{diff}");
        assert!(diff.contains("@@ -1,5 +1,5 @@\n"), "{diff}");
        assert!(diff.contains("@@ -16,5 +16,5 @@\n"), "{diff}");
    }

    #[test]
    fn identical_files_have_no_diff() {
        assert_eq!(unified_diff("a\n", "a\n"), None);
        assert_eq!(unified_diff("a\n", "a"), None);
    }

    fn offer(answer: &str, yes: bool, is_tty: bool) -> (TempDir, Result<()>) {
        let dir = project("port: 1\n");
        let mut input = std::io::Cursor::new(answer.as_bytes().to_vec());
        let result = offer_fix(&dir.join("devy.yml"), "port: 2\n", yes, &mut input, is_tty);
        (dir, result)
    }

    fn content(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("devy.yml")).unwrap()
    }

    #[test]
    fn offer_fix_applies_on_y_and_yes_in_any_case() {
        for answer in ["y\n", "YES\n", "Yes"] {
            let (dir, result) = offer(answer, false, true);
            result.unwrap();
            assert_eq!(content(&dir), "port: 2\n", "answer {answer:?}");
        }
    }

    #[test]
    fn offer_fix_defaults_to_no() {
        for answer in ["\n", "", "n\n", "yep\n"] {
            let (dir, result) = offer(answer, false, true);
            result.unwrap();
            assert_eq!(content(&dir), "port: 1\n", "answer {answer:?}");
        }
    }

    #[test]
    fn offer_fix_never_applies_without_tty_or_yes() {
        let (dir, result) = offer("y\n", false, false);
        result.unwrap();
        assert_eq!(content(&dir), "port: 1\n");
    }

    #[test]
    fn offer_fix_applies_with_yes_flag() {
        let (dir, result) = offer("", true, false);
        result.unwrap();
        assert_eq!(content(&dir), "port: 2\n");
    }

    #[cfg(unix)]
    #[test]
    fn offer_fix_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = project("port: 1\n");
        let path = dir.join("devy.yml");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        offer_fix(&path, "port: 2\n", true, &mut std::io::empty(), false).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[test]
    fn offer_fix_on_read_only_file_errors_naming_devy_yml() {
        let dir = project("port: 1\n");
        let path = dir.join("devy.yml");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms.clone()).unwrap();
        let err = offer_fix(&path, "port: 2\n", true, &mut std::io::empty(), false).unwrap_err();
        assert!(format!("{err:#}").contains("devy.yml"), "{err:#}");
        assert_eq!(content(&dir), "port: 1\n");
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    const MYSQL: &str = "name: shop\n# keep me\ndependencies:\n  - mysql:\n      port: 3306\n";

    #[test]
    fn accepted_fix_is_written() {
        let dir = project(MYSQL);
        record_failure(&dir, Some("mysql"));
        let fixed = MYSQL.replace("3306", "3307");
        let fake = fake(&[&reply(Some(&fixed), &[])]);
        let client = client_for(&fake);
        let (result, warnings) = Run {
            client: &client,
            input: "y\n",
            is_tty: true,
            ..Run::new(&dir)
        }
        .go();
        assert!(result.is_ok(), "{result:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(content(&dir), fixed);
    }

    #[test]
    fn invalid_fix_is_discarded_without_prompting() {
        let dir = project(MYSQL);
        record_failure(&dir, Some("mysql"));
        let bad = "dependencies:\n  - mysql: {}\n    redis: {}\n";
        let fake = fake(&[&reply(Some(bad), &[])]);
        let client = client_for(&fake);
        let (result, warnings) = Run {
            client: &client,
            input: "y\n",
            is_tty: true,
            ..Run::new(&dir)
        }
        .go();
        assert!(result.is_ok());
        assert_eq!(content(&dir), MYSQL);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].starts_with("suggested devy.yml change was invalid and was not offered: "),
            "{warnings:?}"
        );
    }

    #[test]
    fn accepted_fix_write_failure_is_an_error() {
        let dir = project(MYSQL);
        record_failure(&dir, Some("mysql"));
        let path = dir.join("devy.yml");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms.clone()).unwrap();
        let fixed = MYSQL.replace("3306", "3307");
        let fake = fake(&[&reply(Some(&fixed), &[])]);
        let client = client_for(&fake);
        let (result, _) = Run {
            client: &client,
            yes: true,
            ..Run::new(&dir)
        }
        .go();
        assert!(format!("{:#}", result.unwrap_err()).contains("devy.yml"));
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    fn collecting_findings_leaves_stopped_services_stopped() {
        let pm = MockPackageManager {
            installed: true,
            service_running: false,
            ..Default::default()
        };
        let dir = crate::test_support::tmp_dir();
        let findings = check::collect_findings(
            &serde_yml::from_str("dependencies:\n  - redis\n").unwrap(),
            &pm,
            ContainerRuntime::system(Default::default()),
            &MockEnvManager::default(),
            &dir,
        );
        assert_eq!(findings.issues(), vec!["redis: service stopped"]);
        assert!(pm.started_services.borrow().is_empty());
        assert!(pm.installed_packages.borrow().is_empty());
    }
}
