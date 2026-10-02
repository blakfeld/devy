//! Redaction and size caps applied to everything sent to an AI provider.
//!
//! Redaction is deliberately over-eager: a key that merely contains `KEY` loses its value.
//! Losing a harmless value costs a little context; leaking a credential costs much more.

pub const REDACTED: &str = "<redacted>";
pub const TRUNCATED: &str = "… [truncated]";
pub const FILE_CAP: usize = 8 * 1024;
pub const TOTAL_CAP: usize = 100 * 1024;

const SECRET_KEY_PARTS: &[&str] = &[
    "KEY",
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "PRIVATE",
];

/// Whether an environment-style key names a secret (case-insensitive).
pub fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SECRET_KEY_PARTS.iter().any(|part| upper.contains(part))
}

/// Redacts one key/value pair: the whole value when the key names a secret, otherwise
/// only the credential-looking parts of the value.
pub fn value(key: &str, value: &str) -> String {
    if is_secret_key(key) && !value.trim().is_empty() {
        REDACTED.to_string()
    } else {
        patterns(value)
    }
}

/// Whether `value` would be changed by redaction under `key`.
pub fn is_secret(key: &str, val: &str) -> bool {
    value(key, val) != val
}

/// Redacts free text: PEM blocks, `KEY=value` / `key: value` lines whose key names a
/// secret, URL passwords and well-known token prefixes.
pub fn text(s: &str) -> String {
    let without_pem = pem_blocks(s);
    let mut out = String::with_capacity(without_pem.len());
    for line in without_pem.split_inclusive('\n') {
        out.push_str(&patterns(&assignment(line)));
    }
    out
}

/// Caps one file's content at `FILE_CAP` bytes.
pub fn cap_file(s: &str) -> String {
    cap(s, FILE_CAP)
}

/// Caps the whole user content at `TOTAL_CAP` bytes.
pub fn cap_total(s: &str) -> String {
    cap(s, TOTAL_CAP)
}

fn cap(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(TRUNCATED);
    out.push('\n');
    out
}

fn pem_blocks(s: &str) -> String {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(BEGIN) {
        out.push_str(&rest[..start]);
        out.push_str(REDACTED);
        let after = &rest[start + BEGIN.len()..];
        rest = match after.find(END) {
            Some(end) => {
                let tail = &after[end + END.len()..];
                match tail.find("-----") {
                    Some(close) => &tail[close + 5..],
                    None => "",
                }
            }
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Redacts the value of a `KEY=value`, `KEY: value`, `- KEY=value` or `"key": value`
/// line when the key names a secret.
fn assignment(line: &str) -> String {
    let body = line.trim_end_matches(['\n', '\r']);
    let newline = &line[body.len()..];
    let indent = body.len() - body.trim_start().len();
    let mut i = indent;
    let bytes = body.as_bytes();
    for prefix in ["- ", "export "] {
        if body[i..].starts_with(prefix) {
            i += prefix.len();
        }
    }
    let quote = matches!(bytes.get(i), Some(b'"' | b'\'')).then(|| bytes[i]);
    if quote.is_some() {
        i += 1;
    }
    let key_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || b"_.-".contains(&bytes[i])) {
        i += 1;
    }
    if i == key_start {
        return line.to_string();
    }
    let key = &body[key_start..i];
    if let Some(q) = quote {
        if bytes.get(i) != Some(&q) {
            return line.to_string();
        }
        i += 1;
    }
    while bytes.get(i) == Some(&b' ') {
        i += 1;
    }
    match bytes.get(i) {
        Some(b'=') => i += 1,
        Some(b':') if matches!(bytes.get(i + 1), None | Some(b' ')) => i += 1,
        _ => return line.to_string(),
    }
    let value = body[i..].trim();
    if !is_secret_key(key) || value.is_empty() || matches!(value, "{" | "[" | "|" | ">") {
        return line.to_string();
    }
    let value_start = body.len() - body[i..].trim_start().len();
    let comma = if value.ends_with(',') { "," } else { "" };
    let quoted = if value.starts_with('"') {
        format!("\"{REDACTED}\"")
    } else {
        REDACTED.to_string()
    };
    format!("{}{quoted}{comma}{newline}", &body[..value_start])
}

fn patterns(s: &str) -> String {
    tokens(&url_passwords(s))
}

fn url_passwords(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(scheme_end) = rest.find("://") {
        let auth_start = scheme_end + 3;
        out.push_str(&rest[..auth_start]);
        let after = &rest[auth_start..];
        let auth_len = after
            .find(|c: char| {
                c == '/' || c == '?' || c == '#' || c == '"' || c == '\'' || c.is_whitespace()
            })
            .unwrap_or(after.len());
        let authority = &after[..auth_len];
        match authority.rfind('@') {
            Some(at) => {
                let userinfo = &authority[..at];
                match userinfo.split_once(':') {
                    Some((user, pass))
                        if !pass.is_empty() && !pass.starts_with('$') && pass != REDACTED =>
                    {
                        out.push_str(user);
                        out.push(':');
                        out.push_str(REDACTED);
                        out.push_str(&authority[at..]);
                    }
                    _ => out.push_str(authority),
                }
            }
            None => out.push_str(authority),
        }
        rest = &after[auth_len..];
    }
    out.push_str(rest);
    out
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Length of a credential token starting at the beginning of `s`, if it is one.
fn token_len(s: &str) -> Option<usize> {
    let len = s.find(|c| !is_token_char(c)).unwrap_or(s.len());
    let token = &s[..len];
    let long_enough = |prefix: &str| token.len() >= prefix.len() + 8;
    let is_token = (token.starts_with("sk-") && long_enough("sk-"))
        || (token.starts_with("ghp_") && long_enough("ghp_"))
        || (token.starts_with("github_pat_") && long_enough("github_pat_"))
        || (token.len() > 5
            && token.starts_with("xox")
            && b"abprs".contains(&token.as_bytes()[3])
            && token.as_bytes()[4] == b'-'
            && long_enough("xoxb-"))
        || (token.starts_with("AKIA")
            && token.len() >= 20
            && token[4..20]
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()));
    is_token.then_some(len)
}

fn tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if !prev.is_some_and(is_token_char)
            && let Some(len) = token_len(rest)
        {
            out.push_str(REDACTED);
            i += len;
            prev = Some('>');
            continue;
        }
        let c = rest.chars().next().expect("i is a char boundary");
        out.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_names_are_redacted() {
        for key in [
            "STRIPE_SECRET_KEY",
            "api_key",
            "GITHUB_TOKEN",
            "DB_PASSWORD",
            "MYSQL_PASSWD",
            "AWS_CREDENTIALS",
            "PRIVATE_THING",
        ] {
            assert_eq!(value(key, "abc"), REDACTED, "{key}");
        }
        assert_eq!(value("PORT", "5432"), "5432");
        assert_eq!(value("API_KEY", ""), "");
    }

    #[test]
    fn env_example_secret_line_redacted() {
        let out = text("STRIPE_SECRET_KEY=sk_test_abc\nPORT=3000\n");
        assert!(out.contains("STRIPE_SECRET_KEY=<redacted>"), "{out}");
        assert!(!out.contains("sk_test_abc"), "{out}");
        assert!(out.contains("PORT=3000"), "{out}");
    }

    #[test]
    fn yaml_and_json_secret_lines_redacted() {
        let out = text(
            "  environment:\n    POSTGRES_PASSWORD: hunter2\n    - MYSQL_ROOT_PASSWORD=pw\n  \"apiKey\": \"abc\",\n",
        );
        assert!(
            !out.contains("hunter2") && !out.contains("=pw") && !out.contains("abc"),
            "{out}"
        );
        assert!(out.contains("POSTGRES_PASSWORD: <redacted>"), "{out}");
        assert!(out.contains("\"apiKey\": \"<redacted>\","), "{out}");
        assert!(out.contains("  environment:\n"), "{out}");
    }

    #[test]
    fn url_password_redacted() {
        assert_eq!(
            value("DATABASE_URL", "postgres://app:hunter2@localhost/app"),
            "postgres://app:<redacted>@localhost/app"
        );
        assert_eq!(
            text("see postgres://app:hunter2@localhost/app\n"),
            "see postgres://app:<redacted>@localhost/app\n"
        );
    }

    #[test]
    fn url_without_password_kept() {
        for url in [
            "postgres://app@localhost/app",
            "postgres://localhost:5432/app",
            "postgres://${USER}:${PASS}@localhost/app",
            "https://example.com/a:b@c",
        ] {
            assert_eq!(value("DATABASE_URL", url), url);
        }
    }

    #[test]
    fn pem_block_redacted() {
        let pem = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\nafter\n";
        let out = text(pem);
        assert_eq!(out, "before\n<redacted>\nafter\n");
    }

    #[test]
    fn known_token_prefixes_redacted() {
        for token in [
            "sk-ant-api03-abcdefgh",
            "ghp_abcdefghijklmnop",
            "github_pat_11ABCDEFG0123",
            "xoxb-123456789-abc",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            let out = text(&format!("token is {token} here\n"));
            assert_eq!(out, "token is <redacted> here\n", "{token}");
        }
    }

    #[test]
    fn token_lookalikes_kept() {
        for s in [
            "task-runner-script",
            "xoxo",
            "AKIA",
            "sk-short",
            "desk-lamp-123456789",
        ] {
            assert_eq!(text(s), s);
        }
    }

    #[test]
    fn is_secret_detects_changes() {
        assert!(is_secret("API_TOKEN", "x"));
        assert!(is_secret("URL", "redis://:pw@localhost"));
        assert!(!is_secret("URL", "redis://localhost:6379"));
    }

    #[test]
    fn large_file_truncated_at_8_kib() {
        let big = "x".repeat(40 * 1024);
        let out = cap_file(&big);
        let (kept, marker) = out.split_once('\n').unwrap();
        assert_eq!(kept.len(), FILE_CAP);
        assert_eq!(marker, "… [truncated]\n");
    }

    #[test]
    fn small_file_untouched() {
        assert_eq!(cap_file("abc"), "abc");
    }

    #[test]
    fn total_capped_at_100_kib() {
        let big = "y\n".repeat(80 * 1024);
        let out = cap_total(&big);
        assert!(out.len() <= TOTAL_CAP + TRUNCATED.len() + 2);
        assert!(out.ends_with("… [truncated]\n"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let s = "é".repeat(FILE_CAP);
        let out = cap_file(&s);
        assert!(out.ends_with("… [truncated]\n"), "{out}");
        assert!(out.len() <= FILE_CAP + TRUNCATED.len() + 2);
    }
}
