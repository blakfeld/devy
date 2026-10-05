//! Installer scripts pinned to exact URLs and SHA-256 digests, downloaded and verified before running.
//!
//! devy never pipes a download into a shell. Every installer it runs is listed here with a
//! URL that pins an exact release or commit and the SHA-256 of what that URL served when
//! the pin was made. The download is HTTPS-only (redirects included), capped in size,
//! streamed into a private 0700 directory while being hashed, and deleted without running
//! when the digest differs. The script then runs as `sh <file> args…` / `bash <file> args…`
//! with an argument vector — nothing is interpolated into a shell command line.
//!
//! The script runs from a private directory with a scrubbed environment: only an allowlist
//! of variables (locale, terminal, home, proxies) is passed on, and PATH loses relative and
//! project-local entries, so a repository can't steer what a verified script downloads
//! (`NIX_INSTALLER_BINARY_ROOT`, `RUSTUP_UPDATE_ROOT`, bun's `GITHUB`, `BASH_ENV`, …) or
//! which `curl` it runs.
//!
//! Only this first stage is pinned: the scripts themselves download release binaries over
//! https without a digest devy knows (the gcloud archive is the exception — devy verifies
//! the archive itself).
//!
//! Bumping an installer means updating its URL and digest here and releasing devy; a stale
//! pin fails closed with the checksum-mismatch error, which names the manual install URL.

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use crate::fs_safe::{
    PrivateTempDir, create_new_nofollow, dir_outside_project, path_outside_project,
    which_outside_project,
};

/// A downloadable installer pinned to an exact URL and SHA-256 digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Installer {
    /// Name used in messages, e.g. `deno installer`.
    pub name: &'static str,
    /// HTTPS URL pinned to an exact release or commit.
    pub url: &'static str,
    /// Lowercase hex SHA-256 of the file at `url`.
    pub sha256: &'static str,
    /// Where users can install the tool by hand when the download fails verification.
    pub manual_url: &'static str,
    /// Largest body accepted, in bytes.
    pub max_bytes: u64,
    /// Deadline for the whole download, in seconds.
    pub timeout_secs: u64,
}

const SCRIPT_MAX_BYTES: u64 = 2 * 1024 * 1024;
const ARCHIVE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Determinate Systems nix-installer v3.23.0.
pub const NIX: Installer = Installer {
    name: "Nix installer",
    url: "https://install.determinate.systems/nix/tag/v3.23.0",
    sha256: "0fdcb03e96f3ea00d838e766c9704abc54f2e2c3d3e3d5922ef1b0a148cbb35b",
    manual_url: "https://nixos.org/download/",
    max_bytes: SCRIPT_MAX_BYTES,
    timeout_secs: 300,
};

/// Homebrew `install.sh` at Homebrew/install commit 35da687 (2026-10-02).
/// Only the brew backend (macOS) bootstraps Homebrew.
#[cfg(any(test, target_os = "macos"))]
pub const HOMEBREW: Installer = Installer {
    name: "Homebrew installer",
    url: "https://raw.githubusercontent.com/Homebrew/install/35da6871c4be7d7fdab2fd505fb7fa667926a2a5/install.sh",
    sha256: "5f333bbe53bc490e51e7ccb1df8779b3dd6ee73a1a7379efda216edb08ccb148",
    manual_url: "https://brew.sh",
    max_bytes: SCRIPT_MAX_BYTES,
    timeout_secs: 300,
};

/// deno `install.sh` at denoland/deno_install commit 41d4676 (2026-07-15).
pub const DENO: Installer = Installer {
    name: "deno installer",
    url: "https://raw.githubusercontent.com/denoland/deno_install/41d4676f8677ec16449b9e2303e7bd52ed81f03b/install.sh",
    sha256: "9c1a4a0ab8ec6c7e81ec7e4d82465693dc197e65632fec646d703a096b981778",
    manual_url: "https://docs.deno.com/runtime/getting_started/installation/",
    max_bytes: SCRIPT_MAX_BYTES,
    timeout_secs: 300,
};

/// bun `install.sh` (what `https://bun.sh/install` serves) at oven-sh/bun `bun-v1.4.2`.
pub const BUN: Installer = Installer {
    name: "bun installer",
    url: "https://raw.githubusercontent.com/oven-sh/bun/744846f844374847c902b5e7fd59b4342a51ef99/src/runtime/cli/install.sh",
    sha256: "04882bf41679d49d9af108657a1e5515bf04fdf2940d12c0d0b1e5d79dc53be8",
    manual_url: "https://bun.sh/docs/installation",
    max_bytes: SCRIPT_MAX_BYTES,
    timeout_secs: 300,
};

/// The rustup release `RUSTUP` is pinned to; passed as `RUSTUP_VERSION` so the script
/// fetches the matching archived `rustup-init` rather than the latest one.
pub const RUSTUP_VERSION: &str = "1.29.1";

/// `rustup-init.sh` (what `https://sh.rustup.rs` serves) at rust-lang/rustup `1.29.1`.
pub const RUSTUP: Installer = Installer {
    name: "rustup installer",
    url: "https://raw.githubusercontent.com/rust-lang/rustup/d95a37b6ab92cc1e455d1576039333c97ca3e2c5/rustup-init.sh",
    sha256: "7d0ea0f8eba7fa1ebfe998091cd7ec4501e33ec5ca6b884eb4d894d7da5170af",
    manual_url: "https://rustup.rs",
    max_bytes: SCRIPT_MAX_BYTES,
    timeout_secs: 300,
};

const GCLOUD_MANUAL_URL: &str = "https://cloud.google.com/sdk/docs/install";

/// Google Cloud CLI 587.0.0 archives, one per supported platform.
pub const GCLOUD_LINUX_X86_64: Installer = Installer {
    name: "gcloud archive",
    url: "https://dl.google.com/dl/cloudsdk/channels/rapid/downloads/google-cloud-cli-587.0.0-linux-x86_64.tar.gz",
    sha256: "57df2448d259c654796a3703af8e5b53a02d439715b2034d6bb811efc2d6dd7b",
    manual_url: GCLOUD_MANUAL_URL,
    max_bytes: ARCHIVE_MAX_BYTES,
    timeout_secs: 1800,
};
pub const GCLOUD_LINUX_ARM64: Installer = Installer {
    name: "gcloud archive",
    url: "https://dl.google.com/dl/cloudsdk/channels/rapid/downloads/google-cloud-cli-587.0.0-linux-arm.tar.gz",
    sha256: "8349b151da42f07136294da0908624fe8ea16fea200cb2f4412ad081a86e890c",
    manual_url: GCLOUD_MANUAL_URL,
    max_bytes: ARCHIVE_MAX_BYTES,
    timeout_secs: 1800,
};
pub const GCLOUD_DARWIN_X86_64: Installer = Installer {
    name: "gcloud archive",
    url: "https://dl.google.com/dl/cloudsdk/channels/rapid/downloads/google-cloud-cli-587.0.0-darwin-x86_64.tar.gz",
    sha256: "8e008577a1dd2da070f8a7aeb116aae7a525824f7b7761e912f848b51e3c0145",
    manual_url: GCLOUD_MANUAL_URL,
    max_bytes: ARCHIVE_MAX_BYTES,
    timeout_secs: 1800,
};
pub const GCLOUD_DARWIN_ARM64: Installer = Installer {
    name: "gcloud archive",
    url: "https://dl.google.com/dl/cloudsdk/channels/rapid/downloads/google-cloud-cli-587.0.0-darwin-arm.tar.gz",
    sha256: "088d923d11dd5f922bfdf0ff12fc8a94934264e32a57c0cd3afc24ce7bfa5452",
    manual_url: GCLOUD_MANUAL_URL,
    max_bytes: ARCHIVE_MAX_BYTES,
    timeout_secs: 1800,
};

/// Every pinned installer, for table checks.
#[cfg(test)]
pub const ALL: &[Installer] = &[
    NIX,
    HOMEBREW,
    DENO,
    BUN,
    RUSTUP,
    GCLOUD_LINUX_X86_64,
    GCLOUD_LINUX_ARM64,
    GCLOUD_DARWIN_X86_64,
    GCLOUD_DARWIN_ARM64,
];

/// The gcloud archive for the platform devy was built for, if Google publishes one.
pub fn gcloud_archive() -> Result<&'static Installer> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(&GCLOUD_LINUX_X86_64)
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Ok(&GCLOUD_LINUX_ARM64)
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok(&GCLOUD_DARWIN_X86_64)
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(&GCLOUD_DARWIN_ARM64)
    } else {
        bail!(
            "devy has no pinned gcloud archive for this platform; install it manually: {GCLOUD_MANUAL_URL}"
        )
    }
}

/// The shell an installer script is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpreter {
    Sh,
    Bash,
}

impl Interpreter {
    pub fn program(self) -> &'static str {
        match self {
            Interpreter::Sh => "sh",
            Interpreter::Bash => "bash",
        }
    }
}

/// Which URL schemes a download may use. Plain HTTP exists only so tests can serve
/// bodies from a local `TcpListener`; release builds can only ever use `HttpsOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    HttpsOnly,
    #[cfg(test)]
    AllowHttp,
}

impl Transport {
    fn allows(self, url: &str) -> bool {
        match self {
            Transport::HttpsOnly => has_scheme(url, "https://"),
            #[cfg(test)]
            Transport::AllowHttp => has_scheme(url, "https://") || has_scheme(url, "http://"),
        }
    }
}

fn has_scheme(url: &str, scheme: &str) -> bool {
    url.get(..scheme.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(scheme))
}

fn agent(transport: Transport, timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        // Enforced by ureq on every hop, so an https URL can't redirect to http.
        .https_only(transport == Transport::HttpsOnly)
        .redirects(5)
        // Honour ALL_PROXY / HTTPS_PROXY / HTTP_PROXY (ureq 2 has no NO_PROXY support).
        // TLS is still verified end to end, so a proxy can't alter what is downloaded.
        // The test-only plain-http transport talks to a local server and never proxies.
        .try_proxy_from_env(transport == Transport::HttpsOnly)
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(60))
        .timeout(timeout)
        .user_agent(concat!("devy/", env!("CARGO_PKG_VERSION")))
        .build()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Downloads `inst` into a new file `dir/<file_name>` and verifies its digest. The file
/// is removed on any failure, including a checksum mismatch.
pub fn download(inst: &Installer, dir: &Path, file_name: &str) -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(result) = test_hooks::fake_download(inst, dir, file_name) {
        return result;
    }
    download_with(inst, dir, file_name, Transport::HttpsOnly)
}

fn download_with(
    inst: &Installer,
    dir: &Path,
    file_name: &str,
    transport: Transport,
) -> Result<PathBuf> {
    if !transport.allows(inst.url) {
        bail!(
            "refusing to download the {} from {}: only https is allowed",
            inst.name,
            inst.url
        );
    }
    let dest = dir.join(file_name);
    let mut file = create_new_nofollow(&dest)
        .with_context(|| format!("Failed to create {}", dest.display()))?;
    // From here on the file is ours, so any failure removes it.
    let result = fetch_into(inst, &mut file, transport).and_then(|()| {
        file.sync_all()
            .with_context(|| format!("Failed to write {}", dest.display()))
    });
    drop(file);
    match result {
        Ok(()) => Ok(dest),
        Err(e) => {
            let _ = fs::remove_file(&dest);
            Err(e)
        }
    }
}

fn fetch_into(inst: &Installer, file: &mut fs::File, transport: Transport) -> Result<()> {
    let failed = |e: &dyn std::fmt::Display| {
        anyhow!(
            "Failed to download the {} from {}: {e}; install it manually: {}",
            inst.name,
            inst.url,
            inst.manual_url
        )
    };
    let response = agent(transport, Duration::from_secs(inst.timeout_secs))
        .get(inst.url)
        .call()
        .map_err(|e| failed(&e))?;
    if !transport.allows(response.get_url()) {
        bail!(
            "refusing the {} download: it was redirected to {}, which is not https",
            inst.name,
            response.get_url()
        );
    }
    if let Some(len) = response
        .header("Content-Length")
        .and_then(|v| v.trim().parse::<u64>().ok())
        && len > inst.max_bytes
    {
        bail!(
            "refusing the {} download: {len} bytes exceeds the {}-byte limit",
            inst.name,
            inst.max_bytes
        );
    }

    let mut reader = response.into_reader().take(inst.max_bytes + 1);
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| failed(&e))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > inst.max_bytes {
            bail!(
                "refusing the {} download: it exceeds the {}-byte limit",
                inst.name,
                inst.max_bytes
            );
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .with_context(|| format!("Failed to save the {}", inst.name))?;
    }

    let actual = hex(&hasher.finalize());
    if actual != inst.sha256 {
        bail!(
            "{} checksum mismatch: expected {}, got {} — the script was not run; install it manually: {}",
            inst.name,
            inst.sha256,
            actual,
            inst.manual_url
        );
    }
    Ok(())
}

/// Environment variables an installer may inherit. Everything else — including
/// variables a repository's environment could set to redirect an installer's downloads
/// (`NIX_INSTALLER_*`, `RUSTUP_*`, `GITHUB`, `HOMEBREW_*`, `CLOUDSDK_*`) or run code
/// (`BASH_ENV`, `ENV`, `SUDO_ASKPASS`, which `sudo -A` would run) — is dropped. `PATH`,
/// `TMPDIR` and `HOME` are passed separately: cleaned, and `HOME` from the user database
/// on Unix, so a repository's environment can't point curl or git at its own
/// `.curlrc`/`.gitconfig`. `SHELL` is dropped as a matter of hygiene, but that alone does
/// not change what a bash-run script sees: bash re-derives `SHELL` from the user database
/// when it is unset, so callers that care (the bun installer, which edits the rc file of
/// the shell `$SHELL` names) pass `SHELL=/bin/sh` explicitly.
///
/// `SSL_CERT_FILE` and `CURL_CA_BUNDLE` are deliberately dropped: the scripts' second-stage
/// downloads carry no digest devy knows, so a repository-supplied trust root would let it
/// intercept them. Users behind a TLS-inspecting proxy install the proxy's root in the
/// system trust store instead. `CI` and `NONINTERACTIVE` only make installers skip prompts
/// and can't redirect a download, so they pass through.
const INHERITED_ENV: &[&str] = &[
    "CI",
    "NONINTERACTIVE",
    "USER",
    "LOGNAME",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "LANG",
    "LANGUAGE",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    // Windows essentials, for completeness; the installers here are Unix scripts.
    "SystemRoot",
    "windir",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "PATHEXT",
    "ComSpec",
];

fn inherited(name: &str) -> bool {
    INHERITED_ENV.contains(&name) || name.starts_with("LC_")
}

/// A `Command` for `program` with a scrubbed environment (see `INHERITED_ENV`), a PATH
/// without relative or project-local entries, inherited stdio and `cwd` as its working
/// directory, so nothing in the repository can influence a verified installer.
pub fn installer_command(program: &Path, cwd: &Path) -> Command {
    installer_command_with(program, cwd, std::env::vars_os(), std::env::var_os("HOME"))
}

/// `installer_command` with the parent environment (`vars`) and the raw `$HOME` value
/// (`env_home`) injected.
fn installer_command_with(
    program: &Path,
    cwd: &Path,
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    env_home: Option<OsString>,
) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear()
        .envs(
            vars.into_iter()
                .filter(|(k, _)| k.to_str().is_some_and(inherited)),
        )
        .env("PATH", path_outside_project())
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(tmp) = std::env::var_os("TMPDIR").filter(|t| dir_outside_project(t)) {
        cmd.env("TMPDIR", tmp);
    }
    if let Some(home) = user_home_from(env_home) {
        cmd.env("HOME", home);
    }
    cmd
}

/// The home directory installers run with and devy looks for their output in: `$HOME`
/// when it is an existing absolute directory outside the project that the current user
/// owns (so container and CI setups that point HOME elsewhere keep working), otherwise the
/// user database entry. A repository can't redirect it into a directory it controls.
pub fn user_home() -> Option<OsString> {
    user_home_from(std::env::var_os("HOME"))
}

/// `user_home` with the raw `$HOME` value injected.
fn user_home_from(env_home: Option<OsString>) -> Option<OsString> {
    let env_home = env_home.filter(|h| trusted_home(h));
    #[cfg(unix)]
    if env_home.is_none() {
        return passwd_home();
    }
    env_home
}

fn trusted_home(home: &std::ffi::OsStr) -> bool {
    if !dir_outside_project(home) {
        return false;
    }
    let Ok(meta) = fs::metadata(home) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.is_dir() && meta.uid() == crate::fs_safe::current_uid()
    }
    #[cfg(not(unix))]
    {
        meta.is_dir()
    }
}

#[cfg(unix)]
fn passwd_home() -> Option<OsString> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: an all-zero `passwd` is a valid out-parameter (null pointers, zero ids).
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to a live, correctly sized local; `getpwuid_r` writes
    // strings only into `buf` and sets `result` to `&mut pwd` or null.
    let rc = unsafe {
        libc::getpwuid_r(
            crate::fs_safe::current_uid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: `pw_dir` is non-null and points to a NUL-terminated string inside `buf`.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) }.to_bytes();
    (!dir.is_empty() && dir.starts_with(b"/"))
        .then(|| std::ffi::OsStr::from_bytes(dir).to_os_string())
}

/// Resolves `sh` / `bash` from PATH, skipping project-local directories.
fn interpreter_path(interpreter: Interpreter) -> Result<PathBuf> {
    which_outside_project(interpreter.program())
        .ok_or_else(|| anyhow!("`{}` was not found on PATH", interpreter.program()))
}

/// Downloads and verifies `inst` into a private temporary directory, then runs it as
/// `<interpreter> <file> <args…>` from that directory, through `installer_command`, with
/// `envs` added to the scrubbed environment. Returns the script's exit status; callers
/// decide what a failure means.
pub fn run_script(
    inst: &Installer,
    interpreter: Interpreter,
    args: &[OsString],
    envs: &[(&str, &str)],
) -> Result<ExitStatus> {
    let tmp = PrivateTempDir::new("installer")?;
    let script = download(inst, tmp.path(), "install.sh")?;
    let program = interpreter_path(interpreter)?;
    #[cfg(test)]
    test_hooks::record_run(inst, interpreter, args, envs);
    installer_command(&program, tmp.path())
        .arg(&script)
        .args(args)
        .envs(envs.iter().copied())
        .status()
        .with_context(|| format!("Failed to run the {}", inst.name))
}

/// Test-only replacements for the network: tests register a body per installer URL,
/// which `download` writes without a digest check, and every `run_script` call is
/// recorded. With nothing registered a download fails, so no test reaches the network.
#[cfg(test)]
pub mod test_hooks {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static BODIES: RefCell<HashMap<&'static str, Vec<u8>>> = RefCell::new(HashMap::new());
        static RUNS: RefCell<Vec<(&'static str, Interpreter, Vec<OsString>)>> =
            const { RefCell::new(Vec::new()) };
        static ENVS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    }

    /// Serves `body` for `inst` on this thread until `clear` is called.
    pub fn serve(inst: &Installer, body: &[u8]) {
        BODIES.with(|b| b.borrow_mut().insert(inst.url, body.to_vec()));
    }

    /// Forgets registered bodies and recorded runs on this thread.
    pub fn clear() {
        BODIES.with(|b| b.borrow_mut().clear());
        RUNS.with(|r| r.borrow_mut().clear());
        ENVS.with(|e| e.borrow_mut().clear());
    }

    /// The `(installer name, interpreter, args)` of every `run_script` on this thread.
    pub fn runs() -> Vec<(&'static str, Interpreter, Vec<OsString>)> {
        RUNS.with(|r| r.borrow().clone())
    }

    /// The extra environment variables passed to the last `run_script` on this thread.
    pub fn envs() -> Vec<(String, String)> {
        ENVS.with(|e| e.borrow().clone())
    }

    pub(super) fn record_run(
        inst: &Installer,
        interpreter: Interpreter,
        args: &[OsString],
        envs: &[(&str, &str)],
    ) {
        RUNS.with(|r| r.borrow_mut().push((inst.name, interpreter, args.to_vec())));
        ENVS.with(|e| {
            *e.borrow_mut() = envs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        });
    }

    pub(super) fn fake_download(
        inst: &Installer,
        dir: &Path,
        file_name: &str,
    ) -> Option<Result<PathBuf>> {
        let body = BODIES.with(|b| b.borrow().get(inst.url).cloned());
        Some(match body {
            Some(body) => {
                let dest = dir.join(file_name);
                fs::write(&dest, body)
                    .map(|()| dest)
                    .map_err(anyhow::Error::from)
            }
            None => Err(anyhow!(
                "network downloads are disabled in tests (no body registered for {})",
                inst.url
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tmp_dir;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    /// Serves one HTTP response on a local port: `headers` then `body`, then closes the
    /// connection. Returns the URL to fetch.
    fn serve_once(status: &'static str, headers: String, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let _ = write!(stream, "HTTP/1.1 {status}\r\n{headers}\r\n");
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/install.sh")
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    fn digest_of(bytes: &[u8]) -> &'static str {
        leak(hex(&Sha256::digest(bytes)))
    }

    fn installer(url: String, sha256: &'static str) -> Installer {
        Installer {
            name: "test installer",
            url: leak(url),
            sha256,
            manual_url: "https://example.invalid/manual",
            max_bytes: 1024,
            timeout_secs: 10,
        }
    }

    fn files_in(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }

    #[test]
    fn verified_download_is_written() {
        let body = b"echo hello\n".to_vec();
        let sha = digest_of(&body);
        let url = serve_once(
            "200 OK",
            format!("Content-Length: {}\r\n", body.len()),
            body.clone(),
        );
        let dir = tmp_dir();
        let path = download_with(&installer(url, sha), &dir, "s.sh", Transport::AllowHttp).unwrap();
        assert_eq!(fs::read(path).unwrap(), body);
    }

    #[test]
    fn digest_mismatch_fails_closed_and_removes_the_file() {
        let body = b"echo tampered\n".to_vec();
        let expected = digest_of(b"echo original\n");
        let actual = digest_of(&body);
        let url = serve_once(
            "200 OK",
            format!("Content-Length: {}\r\n", body.len()),
            body,
        );
        let dir = tmp_dir();
        let err = download_with(
            &installer(url, expected),
            &dir,
            "s.sh",
            Transport::AllowHttp,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains(&format!(
                "test installer checksum mismatch: expected {expected}, got {actual}"
            )),
            "{err}"
        );
        assert!(err.contains("https://example.invalid/manual"), "{err}");
        assert!(
            files_in(&dir).is_empty(),
            "the mismatched script must be deleted"
        );
    }

    #[test]
    fn truncated_body_fails_and_removes_the_file() {
        let full = b"echo first half; echo second half\n".to_vec();
        let sha = digest_of(&full);
        // Promise the full length, send half, close the connection.
        let url = serve_once(
            "200 OK",
            format!("Content-Length: {}\r\n", full.len()),
            full[..full.len() / 2].to_vec(),
        );
        let dir = tmp_dir();
        let result = download_with(&installer(url, sha), &dir, "s.sh", Transport::AllowHttp);
        assert!(result.is_err(), "a truncated download must fail");
        assert!(files_in(&dir).is_empty(), "no partial script may remain");
    }

    #[test]
    fn oversized_content_length_is_refused() {
        let url = serve_once(
            "200 OK",
            "Content-Length: 4096\r\n".into(),
            vec![b'x'; 4096],
        );
        let dir = tmp_dir();
        let err = download_with(
            &installer(url, digest_of(b"")),
            &dir,
            "s.sh",
            Transport::AllowHttp,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("limit"), "{err}");
        assert!(files_in(&dir).is_empty());
    }

    #[test]
    fn oversized_body_without_length_is_refused() {
        // No Content-Length: the body ends when the connection closes.
        let url = serve_once("200 OK", "Connection: close\r\n".into(), vec![b'x'; 4096]);
        let dir = tmp_dir();
        let err = download_with(
            &installer(url, digest_of(b"")),
            &dir,
            "s.sh",
            Transport::AllowHttp,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("limit"), "{err}");
        assert!(files_in(&dir).is_empty());
    }

    #[test]
    fn http_error_status_fails() {
        let url = serve_once("404 Not Found", "Content-Length: 0\r\n".into(), Vec::new());
        let dir = tmp_dir();
        let err = download_with(
            &installer(url, digest_of(b"")),
            &dir,
            "s.sh",
            Transport::AllowHttp,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to download the test installer"),
            "{msg}"
        );
        assert!(
            msg.contains("install it manually: https://example.invalid/manual"),
            "{msg}"
        );
    }

    #[test]
    fn https_only_refuses_plain_http_without_connecting() {
        let dir = tmp_dir();
        let inst = installer("http://127.0.0.1:9/install.sh".into(), digest_of(b""));
        let err = download_with(&inst, &dir, "s.sh", Transport::HttpsOnly)
            .unwrap_err()
            .to_string();
        assert!(err.contains("only https is allowed"), "{err}");
    }

    #[test]
    fn scheme_policy_accepts_only_https() {
        assert!(!Transport::HttpsOnly.allows("http://example.com/x"));
        assert!(!Transport::HttpsOnly.allows("ftp://example.com/x"));
        assert!(!Transport::HttpsOnly.allows("file:///etc/passwd"));
        assert!(!Transport::HttpsOnly.allows("https"));
        assert!(Transport::HttpsOnly.allows("HTTPS://example.com/x"));
    }

    #[test]
    fn https_only_agent_refuses_http_urls() {
        // ureq applies `https_only` to every request it makes, redirects included, so a
        // redirect from the pinned https URL to an http one is refused the same way.
        let err = agent(Transport::HttpsOnly, Duration::from_secs(5))
            .get("http://127.0.0.1:9/x")
            .call()
            .unwrap_err();
        assert!(err.to_string().contains("https"), "{err}");
    }

    #[test]
    fn existing_file_is_not_overwritten_or_removed() {
        let dir = tmp_dir();
        fs::write(dir.join("s.sh"), b"mine").unwrap();
        // Never contacted: creating the file fails first.
        let url = "http://127.0.0.1:9/x".to_string();
        assert!(
            download_with(
                &installer(url, digest_of(b"ok")),
                &dir,
                "s.sh",
                Transport::AllowHttp
            )
            .is_err()
        );
        assert_eq!(fs::read(dir.join("s.sh")).unwrap(), b"mine");
    }

    #[test]
    fn trusted_home_requires_an_existing_owned_absolute_dir() {
        let dir = tmp_dir();
        assert!(trusted_home(dir.as_os_str()));
        assert!(!trusted_home("relative/home".as_ref()));
        assert!(!trusted_home(dir.join("missing").as_os_str()));
        let file = dir.join("file");
        fs::write(&file, b"").unwrap();
        assert!(!trusted_home(file.as_os_str()));
        #[cfg(unix)]
        assert!(!trusted_home("/".as_ref()) || crate::fs_safe::current_uid() == 0);
    }

    #[test]
    fn installer_env_allowlist() {
        for keep in [
            "LANG",
            "LC_ALL",
            "TERM",
            "HTTPS_PROXY",
            "no_proxy",
            "CI",
            "NONINTERACTIVE",
        ] {
            assert!(inherited(keep), "{keep}");
        }
        for drop in [
            "BASH_ENV",
            "ENV",
            "GITHUB",
            "NIX_INSTALLER_BINARY_ROOT",
            "NIX_INSTALLER_OVERRIDE_URL",
            "RUSTUP_UPDATE_ROOT",
            "RUSTUP_VERSION",
            "BUN_INSTALL",
            "DENO_INSTALL",
            "HOMEBREW_BREW_GIT_REMOTE",
            "CLOUDSDK_PYTHON",
            "SSL_CERT_FILE",
            "CURL_CA_BUNDLE",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "PATH",
            "TMPDIR",
            "HOME",
            "SHELL",
            "SUDO_ASKPASS",
            "TAR_OPTIONS",
        ] {
            assert!(!inherited(drop), "{drop}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn installer_command_scrubs_env_and_runs_outside_the_project() {
        let dir = tmp_dir();
        let out = dir.join("env");
        // The parent environment is injected rather than set with `set_var`, which
        // every concurrently running test would see.
        let parent_env = std::env::vars_os().chain(
            [
                ("NIX_INSTALLER_BINARY_ROOT", "https://evil.example"),
                ("SUDO_ASKPASS", "/tmp/evil-askpass"),
                ("HOME", "/tmp/evil-home"),
                ("LC_DEVY_TEST", "kept"),
            ]
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
        );
        let status = installer_command_with(
            &interpreter_path(Interpreter::Sh).unwrap(),
            &dir,
            parent_env,
            Some("/tmp/evil-home".into()),
        )
        .arg("-c")
        .arg(format!("{{ env; pwd; }} > '{}'", out.display()))
        .status();
        assert!(status.unwrap().success());
        let seen = fs::read_to_string(&out).unwrap();
        assert!(seen.contains("LC_DEVY_TEST=kept"), "{seen}");
        assert!(!seen.contains("NIX_INSTALLER_BINARY_ROOT"), "{seen}");
        assert!(!seen.contains("evil-askpass"), "{seen}");
        // `/tmp/evil-home` doesn't exist, so it is not trusted.
        assert!(!seen.contains("HOME=/tmp/evil-home"), "{seen}");
        assert!(seen.contains("PATH="), "{seen}");
        let cwd = seen.lines().last().unwrap();
        assert_eq!(
            Path::new(cwd).canonicalize().unwrap(),
            dir.canonicalize().unwrap()
        );
    }

    #[test]
    fn table_entries_are_pinned_https_with_sha256_digests() {
        for inst in ALL {
            assert!(
                inst.url.starts_with("https://"),
                "{} is not https",
                inst.name
            );
            assert_eq!(inst.sha256.len(), 64, "{} digest length", inst.name);
            assert!(
                inst.sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{} digest is not lowercase hex",
                inst.name
            );
            assert!(inst.manual_url.starts_with("https://"), "{}", inst.name);
            assert!(inst.max_bytes > 0);
            // Pinned: no moving refs.
            for moving in ["/HEAD/", "/main/", "/master/", "/latest"] {
                assert!(!inst.url.contains(moving), "{} is not pinned", inst.url);
            }
        }
        let raw = "https://raw.githubusercontent.com/";
        for inst in [HOMEBREW, DENO, BUN, RUSTUP] {
            let rest = inst
                .url
                .strip_prefix(raw)
                .expect("raw.githubusercontent URL");
            let commit = rest.split('/').nth(2).unwrap();
            assert!(
                commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
                "{} is not pinned to a commit",
                inst.url
            );
        }
        assert!(
            NIX.url
                .starts_with("https://install.determinate.systems/nix/tag/v")
        );
    }

    #[test]
    fn gcloud_archive_matches_target_or_errors() {
        match gcloud_archive() {
            Ok(inst) => assert!(inst.url.ends_with(".tar.gz")),
            Err(e) => assert!(e.to_string().contains(GCLOUD_MANUAL_URL)),
        }
    }

    #[test]
    fn downloads_are_disabled_in_tests_without_a_registered_body() {
        test_hooks::clear();
        let dir = tmp_dir();
        let err = download(&NIX, &dir, "install.sh").unwrap_err().to_string();
        assert!(err.contains("disabled in tests"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn run_script_passes_args_as_argv() {
        test_hooks::clear();
        let dir = tmp_dir();
        let out = dir.join("args");
        let script = format!("printf '%s\\n' \"$@\" > '{}'\n", out.display());
        test_hooks::serve(&DENO, script.as_bytes());
        let args: Vec<OsString> = ["-s", "v1; touch /tmp/pwned", "$(id)"]
            .iter()
            .map(OsString::from)
            .collect();
        let status = run_script(&DENO, Interpreter::Sh, &args, &[("CI", "1")]).unwrap();
        assert_eq!(
            test_hooks::envs(),
            vec![("CI".to_string(), "1".to_string())]
        );
        test_hooks::clear();
        assert!(status.success());
        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            "-s\nv1; touch /tmp/pwned\n$(id)\n"
        );
    }
}
