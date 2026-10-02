use anyhow::{Context, Result, bail};
use std::process::Command;

pub fn run() -> Result<()> {
    let branch = current_branch()?;
    if branch == "main" || branch == "master" {
        bail!("already on the default branch — create a feature branch first");
    }

    let remote_url = remote_url()?;
    let repo = github_repo_path(&remote_url).with_context(|| {
        format!(
            "could not parse GitHub repo from remote URL: {}",
            remote_url
        )
    })?;

    let pr_url = format!(
        "https://github.com/{}/compare/{}?expand=1",
        repo,
        encode_path_segment(&branch)
    );

    let url = gh_pr_url().unwrap_or(pr_url);
    open_browser(&url)
}

fn gh_pr_url() -> Option<String> {
    let out = Command::new("gh")
        .args(["pr", "view", "--json", "url", "--jq", ".url"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

fn current_branch() -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!("not inside a git repository");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn remote_url() -> Result<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!("no remote named 'origin' found");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn github_repo_path(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("https://github.com/"))?;
    Some(path.trim_end_matches(".git").to_string())
}

fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    return Command::new("open")
        .arg(url)
        .status()
        .map(|_| ())
        .context("failed to open browser");
    #[cfg(target_os = "linux")]
    return Command::new("xdg-open")
        .arg(url)
        .status()
        .map(|_| ())
        .context("failed to open browser");
    #[cfg(target_os = "windows")]
    return Command::new("cmd")
        .args(["/c", "start", url])
        .status()
        .map(|_| ())
        .context("failed to open browser");
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    anyhow::bail!("opening a browser is not supported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_remote() {
        assert_eq!(
            github_repo_path("git@github.com:acme/myrepo.git"),
            Some("acme/myrepo".into())
        );
    }

    #[test]
    fn parses_https_remote() {
        assert_eq!(
            github_repo_path("https://github.com/acme/myrepo.git"),
            Some("acme/myrepo".into())
        );
    }

    #[test]
    fn returns_none_for_non_github() {
        assert_eq!(github_repo_path("https://gitlab.com/acme/myrepo.git"), None);
    }

    #[test]
    fn encodes_slash_in_branch() {
        assert_eq!(
            encode_path_segment("feature/my-branch"),
            "feature%2Fmy-branch"
        );
    }

    #[test]
    fn encodes_space_in_branch() {
        assert_eq!(encode_path_segment("fix/my branch"), "fix%2Fmy%20branch");
    }

    #[test]
    fn encodes_hash_in_branch() {
        // '#' would otherwise be treated as a URL fragment, cutting off the branch name
        assert_eq!(encode_path_segment("fix#123"), "fix%23123");
    }

    #[test]
    fn encodes_percent_in_branch() {
        // '%' must be encoded to avoid double-encoding issues
        assert_eq!(encode_path_segment("fix/100%done"), "fix%2F100%25done");
    }

    #[test]
    fn leaves_unreserved_chars_unencoded() {
        assert_eq!(
            encode_path_segment("feat/my-branch_v1.0~rc"),
            "feat%2Fmy-branch_v1.0~rc"
        );
    }
}
