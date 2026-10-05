//! Redaction and size caps applied to everything sent to an AI provider.
//!
//! Redaction is deliberately over-eager: a key that merely contains `KEY` loses its value.
//! Losing a harmless value costs a little context; leaking a credential costs much more.
//!
//! [`text`] applies an ordered list of rules ([`RULES`]). Multi-line shapes (PEM blocks,
//! YAML block scalars) go first so later, line-oriented rules never see their bodies;
//! specific credential shapes go before the generic key-based rule so each keeps its
//! surrounding structure; well-known token prefixes are the final catch-all.

use regex::{Captures, Regex};
use std::sync::LazyLock;

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
    "PASS",
    "PWD",
    "CREDENTIAL",
    "PRIVATE",
    "AUTH",
    "COOKIE",
    "SESSION",
    "DSN",
    "CERT",
];

/// Key parts that name a secret only as a whole word of the key (split at `_`, `-`, `.`
/// and camelCase humps), optionally plural: `sig` (Azure SAS), `X-Amz-Signature`,
/// `PASSWORD_SALT`, `GITHUB_PAT`. As substrings they would catch `PATH`, `SIGNAL` or
/// `DESIGN`.
const SECRET_KEY_WORDS: &[&str] = &["SIG", "SIGNATURE", "SALT", "PAT", "PW"];

/// Compiles a redaction regex once, on first use.
macro_rules! regex {
    ($pattern:expr) => {{
        static RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(&$pattern).expect("redaction regex is valid"));
        &*RE
    }};
}

/// Regex fragment for a key that names a secret: key characters around a
/// [`SECRET_KEY_PARTS`] entry (case-insensitive), or a [`SECRET_KEY_WORDS`] entry as a
/// whole word: between separators or after a `-D` system-property flag
/// (case-insensitive), or as a camelCase hump (`githubPat`,
/// `apiSig`). The callers' regexes bound the key on both sides, so `PATH` and `myPath`
/// never match `PAT`.
fn secret_key_pattern() -> String {
    let humps: Vec<String> = SECRET_KEY_WORDS
        .iter()
        .map(|w| {
            let lower = w.to_ascii_lowercase();
            format!("{}{}", &w[..1], &lower[1..])
        })
        .collect();
    format!(
        r"(?:(?i:[a-z0-9_.\-]*(?:{})[a-z0-9_.\-]*|(?:-D|[a-z0-9_.\-]*[_.\-])?(?:{})s?(?:[_.\-][a-z0-9_.\-]*)?)|[A-Za-z0-9_.\-]*[a-z0-9](?:{}|{})s?(?:[_.\-][A-Za-z0-9_.\-]*)?)",
        SECRET_KEY_PARTS.join("|"),
        SECRET_KEY_WORDS.join("|"),
        humps.join("|"),
        SECRET_KEY_WORDS.join("|")
    )
}

/// Regex fragment for a YAML/JSON key naming a secret: a bare key from
/// [`secret_key_pattern`] (optionally quoted), or a quoted key that may contain spaces.
fn secret_key_alternatives() -> String {
    let parts = SECRET_KEY_PARTS.join("|");
    let words = SECRET_KEY_WORDS.join("|");
    format!(
        r#"(?:"[^"\n]*(?i:{parts})[^"\n]*"|'[^'\n]*(?i:{parts})[^'\n]*'|"(?:[^"\n]*[^A-Za-z0-9"\n])?(?i:{words})s?(?:[^A-Za-z0-9"\n][^"\n]*)?"|["']?{}["']?)"#,
        secret_key_pattern()
    )
}

/// The words of `key`, upper-cased: split at anything but ASCII letters and digits, and
/// at camelCase humps (`apiKey` → `API`, `KEY`).
pub(crate) fn key_words(key: &str) -> Vec<String> {
    let mut words: Vec<String> = vec![String::new()];
    let mut prev_lower = false;
    for ch in key.chars() {
        if !ch.is_ascii_alphanumeric() {
            words.push(String::new());
            prev_lower = false;
            continue;
        }
        if ch.is_ascii_uppercase() && prev_lower {
            words.push(String::new());
        }
        prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        if let Some(w) = words.last_mut() {
            w.push(ch.to_ascii_uppercase());
        }
    }
    words.retain(|w| !w.is_empty());
    words
}

/// Whether a word of `key` is a [`SECRET_KEY_WORDS`] entry or its plural.
fn has_secret_word(key: &str) -> bool {
    key_words(key).iter().any(|w| {
        SECRET_KEY_WORDS
            .iter()
            .any(|part| w == part || w.strip_suffix('S') == Some(part))
    })
}

/// Whether an environment-style key names a secret (case-insensitive).
pub fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SECRET_KEY_PARTS.iter().any(|part| upper.contains(part)) || has_secret_word(key)
}

/// Redacts one key/value pair: the whole value when the key names a secret, otherwise
/// the credential-looking parts of the value. Inside a value, only `key=value` and
/// `key: value` assignments count, so prose like `My Auth Service` is kept.
pub fn value(key: &str, value: &str) -> String {
    if is_secret_key(key) && !value.trim().is_empty() {
        REDACTED.to_string()
    } else {
        credential_parts(value)
    }
}

/// Redacts only the credential-looking parts of a value (URL passwords, `key=value`
/// assignments naming a secret, well-known token shapes), whatever its key.
pub fn credential_parts(value: &str) -> String {
    RULES.iter().fold(value.to_string(), |acc, (name, rule)| {
        if *name == "key-assignment" {
            key_assignments_with(&acc, false)
        } else {
            rule(&acc)
        }
    })
}

/// [`credential_parts`] for a value that is shown as one item (an `environment` value in
/// the trust summary): a secret-named assignment inside it hides only its own value up
/// to the next whitespace or quote, never the rest of the value, so
/// `TOKEN=1 sh -c 'curl x|sh'` keeps the command visible.
pub fn credential_parts_inline(value: &str) -> String {
    // A leading non-key character means no assignment is at the start of a line, which
    // is what makes the key rule take the rest of the line.
    const LEAD: &str = "\u{1}";
    // One line: a line break would start a new line where the key rule takes the rest of
    // it (the summary collapses whitespace runs anyway).
    let marked = format!("{LEAD}{}", value.replace(['\n', '\r'], " "));
    let out = RULES.iter().fold(marked, |acc, (name, rule)| {
        if *name == "key-assignment" {
            key_assignments_with(&acc, false)
        } else {
            rule(&acc)
        }
    });
    out.strip_prefix(LEAD).map(str::to_string).unwrap_or(out)
}

/// Whether `value` would be changed by redaction under `key`.
pub fn is_secret(key: &str, val: &str) -> bool {
    value(key, val) != val
}

/// Redacts free text by applying every rule in [`RULES`], in order.
pub fn text(s: &str) -> String {
    RULES
        .iter()
        .fold(s.to_string(), |acc, (_name, rule)| rule(&acc))
}

/// One redaction rule: rewrites text, replacing what it matches with [`REDACTED`].
type Rule = fn(&str) -> String;

/// The redaction rules, in the order [`text`] applies them.
const RULES: &[(&str, Rule)] = &[
    ("pem-block", pem_blocks),
    ("yaml-block-scalar", block_scalars),
    ("yaml-nested-value", nested_values),
    ("url-userinfo", url_userinfo),
    ("webhook-url", webhook_urls),
    ("query-parameter", query_parameters),
    ("header", headers),
    ("bearer", bearer_tokens),
    ("jwt", jwts),
    ("mysql-password-flag", mysql_password_flags),
    ("redis-cli-password", redis_cli_passwords),
    ("command-password-flag", command_password_flags),
    ("htpasswd", htpasswd_passwords),
    ("sqlcmd-password", sqlcmd_passwords),
    ("cf-auth", cf_auth_passwords),
    ("curl-user", curl_users),
    ("xml-element", xml_elements),
    ("key-assignment", key_assignments),
    ("token-prefix", token_prefixes),
];

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

/// PEM blocks, from `-----BEGIN ` to the matching `-----END …-----` (or the end of the
/// text when the block is unterminated).
fn pem_blocks(s: &str) -> String {
    regex!(r"(?s)-----BEGIN .*?(?:-----END [^\n]*?-----|\z)")
        .replace_all(s, REDACTED)
        .into_owned()
}

/// The indented body of a YAML block scalar (`key: |` or `key: >`) under a secret key.
fn block_scalars(s: &str) -> String {
    // Optional YAML tags (`!!binary`) and anchors (`&k`) may precede the indicator.
    let header = regex!(format!(
        r#"^([ \t]*)(?:- )?{}[ \t]*:[ \t]*(?:[!&][^\s]*[ \t]+)*[|>][0-9+\-]*[ \t]*(?:#.*)?$"#,
        secret_key_alternatives()
    ));
    let lines: Vec<&str> = s.split_inclusive('\n').collect();
    let indent_of = |l: &str| l.len() - l.trim_start_matches([' ', '\t']).len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push_str(line);
        i += 1;
        let Some(caps) = header.captures(line.trim_end_matches(['\n', '\r'])) else {
            continue;
        };
        let indent = caps[1].len();
        // The body runs while lines are blank or indented deeper than the key;
        // trailing blank lines are left in place.
        let mut body_end = i;
        let mut j = i;
        while j < lines.len() {
            let l = lines[j].trim_end_matches(['\n', '\r']);
            if l.trim().is_empty() {
                j += 1;
            } else if indent_of(l) > indent && !l[..indent_of(l)].contains('\t') {
                // YAML forbids tabs in indentation, so a tab-indented line (a Makefile
                // recipe) ends the body.
                j += 1;
                body_end = j;
            } else {
                break;
            }
        }
        if body_end > i {
            out.push_str(&" ".repeat(indent + 2));
            out.push_str(REDACTED);
            let last = lines[body_end - 1];
            out.push_str(&last[last.trim_end_matches(['\n', '\r']).len()..]);
            i = body_end;
        }
    }
    out
}

/// Everything nested below a secret key with no value of its own: list items
/// (`api_keys:\n  - abc`), plain scalars (`password:\n  hunter2`) and the values of
/// nested mappings (`secrets:\n  db: hunter2`), whose keys are kept. The parent's
/// secrecy is inherited, since the nested keys need not name a secret themselves.
fn nested_values(s: &str) -> String {
    // Optional YAML tags (`!!str`) and anchors (`&pw`) may follow the colon.
    let header = regex!(format!(
        r#"^([ \t]*)(- )?({})[ \t]*:[ \t]*(?:[!&][^\s]*[ \t]*)*(?:#.*)?$"#,
        secret_key_alternatives()
    ));
    let mapping =
        regex!(r#"^((?:- )?(?:"[^"\n]*"|'[^'\n]*'|[^\s:"'#][^:\n]*?)[ \t]*:)(?:[ \t]+(.*))?$"#);
    let indent_of = |l: &str| l.len() - l.trim_start_matches([' ', '\t']).len();
    let lines: Vec<&str> = s.split_inclusive('\n').collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push_str(line);
        i += 1;
        let Some(caps) = header.captures(line.trim_end_matches(['\n', '\r'])) else {
            continue;
        };
        if !names_a_secret(&caps[3]) {
            continue;
        }
        let indent = caps[1].len();
        let header_is_item = caps.get(2).is_some();
        while i < lines.len() {
            let raw = lines[i];
            let l = raw.trim_end_matches(['\n', '\r']);
            let ending = &raw[l.len()..];
            let trimmed = l.trim_start();
            let lead = &l[..l.len() - trimmed.len()];
            if !trimmed.is_empty() {
                // YAML forbids tabs in indentation: a tab-indented line is a Makefile
                // recipe or similar, not a value.
                if lead.contains('\t') {
                    break;
                }
                // YAML also allows a key's list items at the key's own indent.
                let same_indent_item =
                    !header_is_item && (trimmed == "-" || trimmed.starts_with("- "));
                if indent_of(l) < indent || (indent_of(l) == indent && !same_indent_item) {
                    break;
                }
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                out.push_str(raw);
            } else if let Some(m) = mapping.captures(trimmed) {
                // `key: value` keeps its key; `key:` with nothing after it opens a
                // deeper level, whose lines are handled as they come.
                match m.get(2).map(|v| v.as_str().trim()) {
                    Some(v) if !v.is_empty() && !v.starts_with('#') => {
                        out.push_str(&format!("{lead}{} {REDACTED}{ending}", &m[1]));
                    }
                    _ => out.push_str(raw),
                }
            } else if let Some(item) = trimmed.strip_prefix("- ") {
                // A `- |` item's body lines are redacted as plain scalars below it.
                if item.trim().is_empty() || item.starts_with(['|', '>']) {
                    out.push_str(raw);
                } else {
                    out.push_str(&format!("{lead}- {REDACTED}{ending}"));
                }
            } else if trimmed == "-" {
                out.push_str(raw);
            } else {
                out.push_str(&format!("{lead}{REDACTED}{ending}"));
            }
            i += 1;
        }
    }
    out
}

/// Whether one of the words of `key` (split at `_`, `-`, `.`, spaces and camelCase
/// humps) ends in a secret part or its plural: `api_keys`, `secrets`, `apiKey`,
/// `db_password` and `credentials` do; `keycloak`, `authelia`, `certbot` and `passport`
/// do not. Used where a whole subtree would otherwise be redacted.
fn names_a_secret(key: &str) -> bool {
    if has_secret_word(key) {
        return true;
    }
    let words = key_words(key);
    // `authentication`, `authorization`, `authn`, `passphrase`, `keystore`, `keyring` and
    // `oauth2` name a secret too, though they do not end in a part.
    const SUFFIXES: &[&str] = &[
        "",
        "S",
        "ENTICATION",
        "ORIZATION",
        "N",
        "Z",
        "PHRASE",
        "WORD",
        "WORDS",
        "STORE",
        "CHAIN",
        "RING",
    ];
    words.iter().any(|w| {
        let w = w.trim_end_matches(|c: char| c.is_ascii_digit());
        SECRET_KEY_PARTS.iter().any(|part| {
            SUFFIXES
                .iter()
                .any(|suffix| w.ends_with(&format!("{part}{suffix}")))
        })
    })
}

/// A URL userinfo password. The userinfo ends at the last `@` before the host, so a
/// password may contain `/` or `@`. A password that is exactly one `${VAR}` reference
/// is kept; anything else starting with `$` is redacted.
fn url_userinfo(s: &str) -> String {
    regex!(
        r#"([A-Za-z][A-Za-z0-9+.\-]*://)([^\s/?#@:'"]*):([^\s'"]*?)@([^\s/?#@'"]*)([/?#\s'"]|$)"#
    )
    .replace_all(s, |c: &Captures| {
        let pass = &c[3];
        if pass.is_empty()
            || pass == REDACTED
            || regex!(r"^\$\{[A-Za-z_][A-Za-z0-9_]*\}$").is_match(pass)
        {
            return c[0].to_string();
        }
        format!("{}{}:{REDACTED}@{}{}", &c[1], &c[2], &c[4], &c[5])
    })
    .into_owned()
}

/// Slack and Discord webhook URLs, whose path is the credential.
fn webhook_urls(s: &str) -> String {
    regex!(
        r"(https?://(?:hooks\.slack\.com/(?:services|workflows|triggers)/|(?:(?:ptb|canary)\.)?discord(?:app)?\.com/api/webhooks/))[A-Za-z0-9/_\-]+"
    )
    .replace_all(s, format!("${{1}}{REDACTED}"))
    .into_owned()
}

/// URL query parameters whose name names a secret (`?token=…&access_token=…`).
fn query_parameters(s: &str) -> String {
    regex!(r#"([?&])([A-Za-z0-9_.\-\[\]%]+)=([^&#\s'"]+)"#)
        .replace_all(s, |c: &Captures| {
            if is_secret_key(&c[2]) {
                format!("{}{}={REDACTED}", &c[1], &c[2])
            } else {
                c[0].to_string()
            }
        })
        .into_owned()
}

/// Credential-bearing HTTP header values (`Authorization: Basic …`, `Cookie: …`).
///
/// When a quote opens right before the header name (JSON keys, `curl -H "…"`), the
/// value ends at the matching unescaped quote. Otherwise the value is the rest of the
/// line, since header values may themselves contain quotes (`session="abc"`,
/// `Digest nonce="…"`).
fn headers(s: &str) -> String {
    let header = regex!(
        r#"(?i)(["']?)\b((?:proxy-)?authorization|set-cookie|cookie|x-api-key|x-auth-token|x-amz-security-token)(["']?[ \t]*:[ \t]*)"#
    );
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    while let Some(c) = header.captures_at(s, pos) {
        let sep_end = c.get(3).expect("group 3 always participates").end();
        out.push_str(&s[pos..sep_end]);
        let mut rest = &s[sep_end..];
        let mut start = sep_end;
        let quote = c[1].chars().next();
        let key_quoted = quote.is_some_and(|q| c[3].starts_with(q));
        let value_quote = rest.chars().next().filter(|ch| matches!(ch, '"' | '\''));
        // An earlier `<redacted>` is measured like any other text, so whatever follows
        // it on the line is still covered.
        let len = match (quote, value_quote) {
            // `curl -H "Name: value"`: the quote before the name closes the value.
            (Some(q), _) if !key_quoted => quoted_len(rest, q),
            // `"Name": "value"`, `'Name': "value"`: the value's own quotes bound it.
            (Some(_), Some(vq)) => {
                out.push(vq);
                rest = &rest[1..];
                start += 1;
                quoted_len(rest, vq)
            }
            // Unquoted value (which may itself contain quotes): the rest of the line.
            _ => rest.find(['\n', '\r']).unwrap_or(rest.len()),
        };
        if len > 0 {
            out.push_str(REDACTED);
        }
        pos = start + len;
    }
    out.push_str(&s[pos..]);
    out
}

/// The length of the body of a string opened by `q`, up to (not including) the closing
/// quote or the end of the line. `"` honours `\` escapes; `'` treats `''` as a literal
/// quote (YAML, and shell concatenation).
fn quoted_len(body: &str, q: char) -> usize {
    quoted_len_until(body, q, Span::Line)
}

/// How far a quoted value may run past the end of its line.
#[derive(Clone, Copy, PartialEq)]
enum Span {
    /// Never.
    Line,
    /// Onto following indented or blank lines (YAML multi-line flow scalars). An
    /// unclosed string ends at the first unindented line.
    Indented,
    /// Onto any following lines (dotenv and shell `KEY="…"`), up to
    /// [`MULTILINE_QUOTE_LINES`] lines, which are all covered when no quote closes
    /// within them, as is the rest of the text when it ends first.
    Any,
}

/// How many lines a dotenv/shell quoted value may span.
const MULTILINE_QUOTE_LINES: usize = 64;

/// [`quoted_len`] with the given [`Span`].
fn quoted_len_until(body: &str, q: char, span: Span) -> usize {
    let mut newlines = 0;
    let mut chars = body.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        match ch {
            '\n' | '\r' if span == Span::Line => return i,
            '\n' if span == Span::Indented
                && !matches!(chars.peek(), Some((_, ' ' | '\t' | '\n' | '\r'))) =>
            {
                return if body[..i].ends_with('\r') { i - 1 } else { i };
            }
            '\n' if span == Span::Any => {
                newlines += 1;
                if newlines > MULTILINE_QUOTE_LINES {
                    return if body[..i].ends_with('\r') { i - 1 } else { i };
                }
            }
            '\n' | '\r' => {}
            '\\' if q == '"' => {
                chars.next_if(|&(_, c)| c != '\n' && c != '\r');
            }
            '\'' if q == '\'' => {
                if chars.next_if(|&(_, c)| c == '\'').is_none() {
                    return i;
                }
            }
            c if c == q => return i,
            _ => {}
        }
    }
    body.len()
}

/// `Bearer <token>`, anywhere in a line.
fn bearer_tokens(s: &str) -> String {
    regex!(r"(?i)\b(bearer[ \t]+)[A-Za-z0-9._~+/=\-]+")
        .replace_all(s, format!("${{1}}{REDACTED}"))
        .into_owned()
}

/// JWTs: three dot-separated base64url segments, the first starting with `eyJ`.
fn jwts(s: &str) -> String {
    regex!(r"\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]*")
        .replace_all(s, REDACTED)
        .into_owned()
}

/// The password glued to `-p` on a MySQL/MariaDB client command line (`mysql -pSECRET`).
fn mysql_password_flags(s: &str) -> String {
    regex!(
        r#"(\b(?:mysql|mysqldump|mysqladmin|mysqlimport|mysqlsh|mariadb|mariadb-dump)\b[^\n]*?[ \t]-p)("[^"\n]*"|'[^'\n]*'|[^\s'"]+)"#
    )
    .replace_all(s, format!("${{1}}{REDACTED}"))
    .into_owned()
}

/// The password after `-p` for clients that take it as the next argument:
/// any `<tool> login` (`docker`, `az`, `oc`, `helm registry`, …), `vsce`/`ovsx publish`,
/// `sshpass` (also glued: `-pX`) and the MongoDB tools,
/// when they run as a command (at line start, after `;&|(:`, `sudo`, `exec` or `run`)
/// and within that command (`docker run --name mongo -p 27017:27017` is a port).
fn command_password_flags(s: &str) -> String {
    regex!(
        r#"(?m)((?:^[ \t]*(?:-[ \t]+)?|[;&|(:][ \t]*|\b(?:sudo|exec|run)[ \t]+)(?:[A-Za-z0-9_.\-]+(?:[ \t]+registry)?[ \t]+login|(?:vsce|ovsx)[ \t]+publish|sshpass|mongo|mongosh|mongodump|mongorestore|mongoexport|mongoimport)\b[^\n;&|]*?[ \t]-p[ \t]*)("[^"\n]*"|'[^'\n]*'|[^\s'"]+)"#
    )
    .replace_all(s, format!("${{1}}{REDACTED}"))
    .into_owned()
}

/// The password after `sqlcmd -P`.
fn sqlcmd_passwords(s: &str) -> String {
    regex!(r#"(\bsqlcmd\b[^\n;&|]*?[ \t]-P[ \t]*)("[^"\n]*"|'[^'\n]*'|[^\s'"]+)"#)
        .replace_all(s, format!("${{1}}{REDACTED}"))
        .into_owned()
}

/// The password in `cf auth <user> <password>`.
fn cf_auth_passwords(s: &str) -> String {
    regex!(
        r#"(\bcf[ \t]+auth[ \t]+(?:"[^"\n]*"|'[^'\n]*'|\S+)[ \t]+)("[^"\n]*"|'[^'\n]*'|[^\s'"]+)"#
    )
    .replace_all(s, format!("${{1}}{REDACTED}"))
    .into_owned()
}

/// The password in `htpasswd -b [options] <file> <user> <password>`, or
/// `htpasswd -nb [options] <user> <password>` (no file). `-C <cost>` takes a value.
fn htpasswd_passwords(s: &str) -> String {
    regex!(r"\bhtpasswd([ \t]+[^\n]*)")
        .replace_all(s, |c: &Captures| {
            let args = &c[1];
            let mut flags = String::new();
            let mut positional = Vec::new();
            let mut takes_value = false;
            for (start, word) in shell_words(args) {
                if takes_value {
                    takes_value = false;
                } else if positional.is_empty() && word.starts_with('-') {
                    flags.push_str(&word[1..]);
                    takes_value = word.ends_with('C');
                } else {
                    positional.push((start, word.len()));
                }
            }
            let index = if flags.contains('n') { 1 } else { 2 };
            match positional.get(index) {
                Some(&(start, len)) if flags.contains('b') => format!(
                    "htpasswd{}{REDACTED}{}",
                    &args[..start],
                    &args[start + len..]
                ),
                _ => c[0].to_string(),
            }
        })
        .into_owned()
}

/// The shell words of a command line, as (byte offset, word) with quotes kept, up to an
/// unquoted `|`, `;`, `&` or `)`.
fn shell_words(s: &str) -> Vec<(usize, &str)> {
    let mut words = Vec::new();
    let mut start = None;
    let mut quote = None;
    for (i, ch) in s.char_indices() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => {
                quote = Some(ch);
                start.get_or_insert(i);
            }
            (None, c) if c.is_whitespace() || "|;&)".contains(c) => {
                if let Some(st) = start.take() {
                    words.push((st, &s[st..i]));
                }
                if !c.is_whitespace() {
                    return words;
                }
            }
            (None, _) => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(st) = start {
        words.push((st, &s[st..]));
    }
    words
}

/// The password after `redis-cli -a` / `--pass`.
fn redis_cli_passwords(s: &str) -> String {
    regex!(r#"(\bredis-cli\b[^\n]*?[ \t](?:-a|--pass)[ \t]+)("[^"\n]*"|'[^'\n]*'|[^\s'"]+)"#)
        .replace_all(s, format!("${{1}}{REDACTED}"))
        .into_owned()
}

/// The password in a `curl -u user:password` / `--user user:password` argument. A quoted
/// argument (`-u 'bob:pa ss'`) is covered up to its closing quote.
fn curl_users(s: &str) -> String {
    let quoted =
        regex!(r#"(\bcurl\b[^\n]*?[ \t](?:-u|--user)(?:[ \t]+|=)?["'][^\s:'"]*:)[^'"\n]+"#)
            .replace_all(s, format!("${{1}}{REDACTED}"));
    regex!(r#"(\bcurl\b[^\n]*?[ \t](?:-u|--user)(?:[ \t]+|=)?["']?[^\s:'"]*:)([^\s'"]+)"#)
        .replace_all(&quoted, format!("${{1}}{REDACTED}"))
        .into_owned()
}

/// The text of an XML element whose name names a secret (`<password>x</password>`).
fn xml_elements(s: &str) -> String {
    regex!(format!(
        r#"(<{}(?:[ \t][^>\n]*)?>)(?:(?s:<!\[CDATA\[.*?\]\]>)|[^<\n]+)(</)"#,
        secret_key_pattern()
    ))
    .replace_all(s, format!("${{1}}{REDACTED}${{2}}"))
    .into_owned()
}

/// The value of a `key=value`, `key: value`, `"key": "value"` or `key value` assignment
/// anywhere in a line, when the key names a secret. Covers `.env`, YAML, JSON, `.npmrc`
/// (`//host/:_authToken=…`), `.netrc` (`password z`) and docker `"auth": "…"`.
fn key_assignments(s: &str) -> String {
    key_assignments_with(s, true)
}

/// Whether `key<sep>value` with a whitespace-only separator is an assignment. Prose words
/// that merely contain a secret part (`ssh-keygen -t`, `the monkey business`,
/// `<auth x=…>`) are not. Command-line flags (`--password value`) always count; otherwise, when
/// `spaced` is set, a tab separator, an env-style key (`DB_PASSWORD value`), the
/// `.netrc` keywords `password`/`passwd`, or a key ending in a secret part that forms
/// a two-word config directive (`alone`, as in redis.conf `requirepass x`) do.
fn spaced_assignment(key: &str, sep: &str, spaced: bool, alone: bool) -> bool {
    if key.starts_with('-') {
        return true;
    }
    let ends_in_part = || {
        let upper = key.to_ascii_uppercase();
        SECRET_KEY_PARTS.iter().any(|part| upper.ends_with(part))
    };
    spaced
        && (sep.contains('\t')
            || (key.bytes().any(|b| b.is_ascii_uppercase())
                && !key.bytes().any(|b| b.is_ascii_lowercase()))
            || key.eq_ignore_ascii_case("password")
            || key.eq_ignore_ascii_case("passwd")
            // A directive at the start of its line (`requirepass x`, `masterauth x`).
            || (alone && ends_in_part()))
}

/// [`key_assignments`], optionally ignoring the space-separated `key value` form for
/// bare-word keys (see [`spaced_assignment`]).
fn key_assignments_with(s: &str, spaced: bool) -> String {
    let assignment = regex!(format!(
        r#"(?:^|[^A-Za-z0-9_.\-])["']?({})["']?([ \t]*(?:[?+!]|::?)?=[ \t]*|[ \t]*:[ \t]*|[ \t]+)"#,
        secret_key_pattern()
    ));
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    // Start of the line holding the current key, tracked incrementally so long lines
    // with many matches stay linear.
    let mut line_start = 0;
    let mut scanned = 0;
    // First non-blank byte of the line at `line_start`, computed once per line.
    let mut content_start: Option<(usize, usize)> = None;
    while let Some(c) = assignment.captures_at(s, pos) {
        let key = c.get(1).expect("group 1 always participates");
        let sep = c.get(2).expect("group 2 always participates");
        let value_start = sep.end();
        if let Some(i) = s[scanned..key.start()].rfind('\n') {
            line_start = scanned + i + 1;
        }
        scanned = key.start();
        let is_spaced = !sep.as_str().contains(['=', ':']);
        let content = match content_start {
            Some((line, content)) if line == line_start => content,
            _ => {
                let rest = &s[line_start..];
                let content = line_start + rest.len() - rest.trim_start_matches([' ', '\t']).len();
                content_start = Some((line_start, content));
                content
            }
        };
        // A config directive (`requirepass x`): an unindented lower-case key and one
        // value word that is not a flag, alone on the line. At most one per line, so
        // scanning to the line end here stays linear.
        let alone = is_spaced
            && key.start() == line_start
            && !key.as_str().bytes().any(|b| b.is_ascii_uppercase())
            && !s[value_start..].starts_with('-')
            && {
                let rest = &s[value_start..];
                let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let after = &rest[word..];
                let after = &after[..after.find('\n').unwrap_or(after.len())];
                let after = after.trim();
                after.is_empty() || after.starts_with('#')
            };
        // `name:tag` and `host:port` (no space after an unquoted key's colon, unquoted
        // value) are not assignments when the key does not name a secret
        // (`keycloak:24`) or the value is a bare port (`redis-session:6379`).
        // `password:hunter2`, `DD-API-KEY:x`, JSON (quoted key) and shell
        // `${KEY:-default}` (and `:=`, `:?`, `:+`) still are.
        let key_quoted = sep.start() != key.end()
            || matches!(s[..key.start()].chars().next_back(), Some('"' | '\''));
        let glued_colon = sep.as_str() == ":"
            && !key_quoted
            && !s[value_start..].starts_with(['"', '\'', '[', '{', '=', '-', '?', '+'])
            && (!names_a_secret(key.as_str()) || {
                let rest = &s[value_start..];
                let digits = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                digits > 0
                    && rest[digits..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_alphanumeric())
            });
        if glued_colon
            || (is_spaced && !spaced_assignment(key.as_str(), sep.as_str(), spaced, alone))
        {
            // Resume at the separator, so it can precede the next key (`auth token=x`).
            // The key is non-empty, so this still moves past the match start.
            out.push_str(&s[pos..sep.start()]);
            pos = sep.start();
            continue;
        }
        out.push_str(&s[pos..value_start]);
        // Only a short prefix (`- `, `export `, a quote) can precede a line-start key, so
        // long ones are rejected without scanning them.
        let prefix = &s[content.min(key.start())..key.start()];
        let form = if is_spaced {
            ValueForm::Spaced
        } else if prefix.len() <= 32 && is_line_start(prefix) {
            ValueForm::RestOfLine
        } else {
            ValueForm::Token
        };
        let colon = sep.as_str().contains(':') && !sep.as_str().contains('=');
        let (replacement, consumed) = redact_value(&s[value_start..], form, colon);
        out.push_str(&replacement);
        pos = value_start + consumed;
    }
    out.push_str(&s[pos..]);
    out
}

/// How far an unquoted value extends.
#[derive(Clone, Copy)]
enum ValueForm {
    /// `key value`: one whitespace-delimited word.
    Spaced,
    /// `KEY=value` or `key: value` at the start of a line: the rest of the line.
    RestOfLine,
    /// `key=value` mid-line: up to the next whitespace or quote.
    Token,
}

/// Whether only indentation, `- `, `export ` or an opening quote precede a key.
fn is_line_start(prefix: &str) -> bool {
    let p = prefix.trim_start();
    let p = p.strip_prefix("- ").unwrap_or(p).trim_start();
    let p = p.strip_prefix("export ").unwrap_or(p).trim_start();
    matches!(p, "" | "\"" | "'")
}

/// Redacts the value at the start of `rest`, returning its replacement and how many
/// bytes of `rest` it covers. YAML block-scalar indicators (`|`/`>`) are left alone.
fn redact_value(rest: &str, form: ValueForm, colon: bool) -> (String, usize) {
    // `key => value` (Ruby, PHP): one `>` belongs to the separator. Any further `>` is
    // part of the value, so this never recurses.
    if !colon && let Some(after) = rest.strip_prefix('>') {
        let ws = after.len() - after.trim_start_matches([' ', '\t']).len();
        let (r, n) = redact_plain_value(&after[ws..], form, colon);
        return (format!("{}{r}", &rest[..1 + ws]), 1 + ws + n);
    }
    redact_plain_value(rest, form, colon)
}

/// [`redact_value`] without the `=>` separator handling.
fn redact_plain_value(rest: &str, form: ValueForm, colon: bool) -> (String, usize) {
    // A rest-of-line value already starting with `<redacted>` is measured like any
    // other, so text an earlier rule left after it on the same line is still covered.
    // Mid-line, an earlier redaction (such as `?token=<redacted>&page=2`) is kept as is.
    // Text glued to it (`<redacted>~tail`) is still covered.
    if let Some(after) = rest.strip_prefix(REDACTED)
        && !matches!(form, ValueForm::RestOfLine)
    {
        let tail = glued_len(after, "&\"',;)]}#");
        return (REDACTED.to_string(), REDACTED.len() + tail);
    }
    let Some(first) = rest.chars().next() else {
        return (String::new(), 0);
    };
    match first {
        '\n' | '\r' => return (String::new(), 0),
        // `a::b` paths and YAML block-scalar indicators.
        ':' | '|' | '>' if colon => return (String::new(), 0),
        '[' | '{' => return flow_collection(rest),
        '"' | '\'' if rest.starts_with("\"\"\"") || rest.starts_with("\'\'\'") => {
            return triple_quoted(rest);
        }
        '"' | '\'' => {
            let body = &rest[1..];
            let span = match form {
                ValueForm::RestOfLine if colon => Span::Indented,
                ValueForm::RestOfLine => Span::Any,
                ValueForm::Spaced | ValueForm::Token => Span::Line,
            };
            let len = quoted_len_until(body, first, span);
            if !body[len..].starts_with(first) {
                return (format!("{first}{REDACTED}"), len + 1);
            }
            // Shell concatenation (`'it'\''s a secret'`) continues the value past the quote.
            let tail = shell_word_tail(&body[len + 1..]);
            return (format!("{first}{REDACTED}{first}"), len + 2 + tail);
        }
        _ => {}
    }
    let len = match form {
        ValueForm::RestOfLine => {
            let line = &rest[..rest.find(['\n', '\r']).unwrap_or(rest.len())];
            let value = line.trim_end();
            value.strip_suffix(',').unwrap_or(value).len()
        }
        // A quote inside the value (`password=ab"cd`) does not end it.
        ValueForm::Spaced | ValueForm::Token => {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        }
    };
    if len == 0 {
        (String::new(), 0)
    } else {
        (REDACTED.to_string(), len)
    }
}

/// A TOML multi-line string (`"""…"""` or `\'\'\'…\'\'\'`) at the start of `rest`, up to its
/// closing delimiter within [`MULTILINE_QUOTE_LINES`] lines; otherwise everything scanned.
fn triple_quoted(rest: &str) -> (String, usize) {
    let delim = &rest[..3];
    let body = &rest[3..];
    let mut newlines = 0;
    let mut stop = body.len();
    let mut escaped = false;
    for (i, ch) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && delim == "\"\"\"" {
            escaped = true;
            continue;
        }
        if body[i..].starts_with(delim) {
            if i == 0 {
                return (String::new(), 0);
            }
            return (format!("{delim}{REDACTED}{delim}"), 3 + i + 3);
        }
        if ch == '\n' {
            newlines += 1;
            if newlines > MULTILINE_QUOTE_LINES {
                stop = i;
                break;
            }
        }
    }
    (format!("{delim}{REDACTED}"), 3 + stop)
}

/// The length of the rest of a shell word glued to a closing quote: further quoted
/// segments (which may contain spaces), `\`-escaped characters and plain characters, up
/// to unquoted whitespace or structure (`,;)]}&#<>` and `/>`).
fn shell_word_tail(s: &str) -> usize {
    let mut i = 0;
    while let Some(ch) = s[i..].chars().next() {
        match ch {
            '\'' | '"' => {
                let body = &s[i + 1..];
                let len = quoted_len(body, ch);
                if !body[len..].starts_with(ch) {
                    return i + 1 + len;
                }
                i += len + 2;
            }
            '\\' => {
                i += 1;
                match s[i..].chars().next() {
                    Some(c) if c != '\n' && c != '\r' => i += c.len_utf8(),
                    _ => {}
                }
            }
            _ if ch.is_whitespace() || ",;)]}&#<>".contains(ch) || s[i..].starts_with("/>") => {
                return i;
            }
            _ => i += ch.len_utf8(),
        }
    }
    i
}

/// The length of the text at the start of `s` up to whitespace or one of `stops`.
fn glued_len(s: &str, stops: &str) -> usize {
    s.find(|ch: char| ch.is_whitespace() || stops.contains(ch))
        .unwrap_or(s.len())
}

/// A `[ … ]` or `{ … }` value under a secret key: everything up to the matching close,
/// which may be on a later line. An empty `[]`/`{}` is kept. When nothing closes it
/// before a blank line, the scanned text is redacted, so nothing is scanned twice.
fn flow_collection(rest: &str) -> (String, usize) {
    let open = if rest.starts_with('{') { '{' } else { '[' };
    let close = if open == '{' { '}' } else { ']' };
    let mut depth = 0usize;
    let mut line_blank = false;
    // Where an unclosed sequence stops: the blank line, or the end of the text.
    let mut stop = rest.len();
    // Whether the next non-blank character starts an element (after the opener, `,` or
    // `:`): only there does a quote open a string, so `[it's, b]` is not a quote.
    let mut at_element_start = true;
    let mut i = 0;
    while let Some(ch) = rest[i..].chars().next() {
        match ch {
            // A close inside a quoted element does not close the collection.
            '"' | '\'' if at_element_start && i > 0 => {
                let body = &rest[i + 1..];
                let len = quoted_len(body, ch);
                i += 1 + len + usize::from(body[len..].starts_with(ch));
                line_blank = false;
                at_element_start = false;
                continue;
            }
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return if rest[1..i].trim().is_empty() {
                        (String::new(), 0)
                    } else {
                        (format!("{open}{REDACTED}{close}"), i + 1)
                    };
                }
            }
            '\n' if line_blank => {
                stop = i;
                break;
            }
            _ => {}
        }
        match ch {
            '\n' => line_blank = true,
            '\r' | ' ' | '\t' => {}
            _ => line_blank = false,
        }
        match ch {
            ',' | ':' | '[' | '{' => at_element_start = true,
            ' ' | '\t' | '\r' | '\n' => {}
            _ => at_element_start = false,
        }
        i += ch.len_utf8();
    }
    let stop = rest[..stop].trim_end().len();
    (format!("{open}{REDACTED}"), stop)
}

/// Tokens with a well-known credential prefix, not glued to a preceding word. Prefixes
/// that also start ordinary identifiers (`hf_`, `pypi-`, `shpat_`, …) require the
/// token's own length and alphabet, so `hf_hub_download` is kept.
fn token_prefixes(s: &str) -> String {
    regex!(concat!(
        r"(^|[^A-Za-z0-9_\-])(",
        r"(?:sk-|sk_live_|sk_test_|rk_live_|rk_test_|ghp_|gho_|ghu_|ghs_|ghr_|github_pat_|glpat-|npm_|xox[a-z]-)[A-Za-z0-9_\-]{8,}",
        r"|(?:AKIA|ASIA)[A-Z0-9]{16}[A-Za-z0-9_\-]*",
        r"|AIza[A-Za-z0-9_\-]{30,}",
        // Hugging Face, Slack app-level, Google OAuth client secret and access token.
        r"|hf_[A-Za-z0-9]{30,}",
        r"|xapp-[A-Za-z0-9\-]{20,}",
        r"|GOCSPX-[A-Za-z0-9_\-]{20,}",
        r"|ya29\.[A-Za-z0-9_\-.]{20,}",
        // SendGrid `SG.<id>.<secret>`.
        r"|SG\.[A-Za-z0-9_\-]{16,}\.[A-Za-z0-9_\-]{16,}",
        // Stripe/Svix webhook secrets, PyPI, Vault service tokens, Shopify, DigitalOcean,
        // Linear.
        r"|whsec_[A-Za-z0-9+/=]{20,}",
        r"|pypi-[A-Za-z0-9_\-]{40,}",
        r"|hvs\.[A-Za-z0-9_\-]{20,}",
        r"|shpat_[A-Fa-f0-9]{32,}",
        r"|dop_v1_[A-Fa-f0-9]{40,}",
        r"|lin_api_[A-Za-z0-9]{30,}",
        // age secret keys and Telegram bot tokens (`<bot id>:AA<35 chars>`).
        r"|AGE-SECRET-KEY-1[A-Z0-9]{40,}",
        r"|[0-9]{6,}:AA[A-Za-z0-9_\-]{30,}",
        r")"
    ))
    .replace_all(s, format!("${{1}}{REDACTED}"))
    .into_owned()
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
            let out = text(&format!("found {token} here\n"));
            assert_eq!(out, "found <redacted> here\n", "{token}");
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

    /// Every leak found by the security audit: (case, input, secret that must not survive).
    const LEAK_FIXTURES: &[(&str, &str, &str)] = &[
        (
            "bearer jwt in log",
            "2026-10-02 GET /api Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig\n",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig",
        ),
        (
            "bare bearer",
            "retrying with bearer abc123def456ghi\n",
            "abc123def456ghi",
        ),
        (
            "bare jwt",
            "decoded eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl ok\n",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
        ),
        (
            "mid-line assignment",
            "2026-10-02 INFO connecting with password=hunter2\n",
            "hunter2",
        ),
        (
            "mid-line assignment, more after",
            "level=info api_key=abc987 path=/x\n",
            "abc987",
        ),
        (
            "minified json",
            "{\"user\":\"a\",\"password\":\"x9\"}\n",
            "x9",
        ),
        (
            "json with spaces",
            "  \"client_secret\": \"s3cr3t\",\n",
            "s3cr3t",
        ),
        ("tab separated", "DB_PASSWORD\thunter3\n", "hunter3"),
        ("tab around equals", "db_pass\t=\thunter4\n", "hunter4"),
        (
            "npmrc auth token",
            "//registry.npmjs.org/:_authToken=npmsecret0001\n",
            "npmsecret0001",
        ),
        (
            "npmrc legacy auth",
            "_auth = dXNlcjpwYXNz\n",
            "dXNlcjpwYXNz",
        ),
        (
            "netrc one line",
            "machine github.com login bob password n3tr0c\n",
            "n3tr0c",
        ),
        (
            "netrc multi line",
            "machine example.com\n  login bob\n  password n3tr0d\n",
            "n3tr0d",
        ),
        (
            "docker config auth",
            "{\"auths\":{\"ghcr.io\":{\"auth\":\"Ym9iOmRvY2tlcg==\"}}}\n",
            "Ym9iOmRvY2tlcg==",
        ),
        (
            "url password with slash",
            "postgres://u:hun/ter2@db/app\n",
            "hun/ter2",
        ),
        (
            "url password with at",
            "redis://default:p@ss@cache:6379/0\n",
            "p@ss",
        ),
        (
            "query token",
            "GET https://api.example.com/v1?token=qtok111&access_token=qtok222&page=2\n",
            "qtok111",
        ),
        (
            "query access_token",
            "GET https://api.example.com/v1?token=qtok111&access_token=qtok222&page=2\n",
            "qtok222",
        ),
        (
            "mysql -p",
            "mysql -uroot -pMYSQLSECRET -h 127.0.0.1 app\n",
            "MYSQLSECRET",
        ),
        (
            "basic auth header",
            "curl -H \"Authorization: Basic dXNlcjpodW50ZXI=\" x\n",
            "dXNlcjpodW50ZXI=",
        ),
        (
            "cookie header",
            "Cookie: sid=abcdef0123456789\n",
            "abcdef0123456789",
        ),
        (
            "slack webhook",
            "notify https://hooks.slack.com/services/T000/B000/XXXXXXXXXXXX\n",
            "XXXXXXXXXXXX",
        ),
        (
            "yaml block scalar",
            "tls:\n  private_key: |\n    MIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n    bm90IGEgcmVhbCBrZXk=\n  port: 443\n",
            "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC",
        ),
        (
            "yaml folded scalar second line",
            "creds:\n  client_secret: >-\n    first\n    s3condl1ne\nnext: 1\n",
            "s3condl1ne",
        ),
        (
            "yaml multi-word plain value",
            "POSTGRES_PASSWORD: correct horse battery\n",
            "horse battery",
        ),
        (
            "sentry dsn",
            "SENTRY_DSN=https://abc123@o1.ingest.sentry.io/1\n",
            "abc123",
        ),
        (
            "session cookie key",
            "SESSION_SECRET=keyboardcat\n",
            "keyboardcat",
        ),
        (
            "quoted set-cookie value",
            "Set-Cookie: session=\"abc123\"; Path=/\n",
            "abc123",
        ),
        (
            "digest authorization",
            "Authorization: Digest username=\"bob\", nonce=\"dcd98b\", response=\"6629fae4\"\n",
            "6629fae4",
        ),
        (
            "json header with escaped quotes",
            "{\"Cookie\": \"a=\\\"x\\\"; sid=cookiesid1\"}\n",
            "cookiesid1",
        ),
        (
            "yaml single-quoted with escaped quote",
            "password: 'abc''def'\n",
            "def",
        ),
        ("fat arrow", "'password' => 'arrowpw1',\n", "arrowpw1"),
        (
            "url password starting with dollar",
            "postgres://u:$ecr3t@h/db\n",
            "$ecr3t",
        ),
        ("crlf env", "DB_PASSWORD=crlfpw\r\nPORT=1\r\n", "crlfpw"),
        (
            "non-ascii value",
            "password=h\u{e9}llo w\u{f6}rld\n",
            "h\u{e9}llo",
        ),
    ];

    /// One sample of every well-known token prefix.
    const PREFIXED_TOKENS: &[&str] = &[
        "sk-ant-api03-abcdefgh",
        "sk-proj-abcdefgh12",
        "sk_live_abcdefgh1234",
        "sk_test_abcdefgh1234",
        "rk_live_abcdefgh1234",
        "ghp_abcdefghijklmnop",
        "gho_abcdefghijklmnop",
        "ghu_abcdefghijklmnop",
        "ghs_abcdefghijklmnop",
        "ghr_abcdefghijklmnop",
        "github_pat_11ABCDEFG0123",
        "glpat-abcdefghij123456",
        "xoxb-123456789-abc",
        "xoxp-123456789-abc",
        "AKIAIOSFODNN7EXAMPLE",
        "ASIAIOSFODNN7EXAMPLE",
        "AIzaSyA-abcdefghijklmnopqrstuvwxyz12345",
        "npm_abcdefghijklmnopqrstuvwxyz0123456789",
        "hf_AbCdEfGhIjKlMnOpQrStUvWxYz01234567",
        "xapp-1-A0123456789-1234567890123-abcdef0123456789",
        "GOCSPX-AbCdEfGhIjKlMnOpQrStUvWx",
        "ya29.a0AfH6SMBxYz-abcdefghijklmnopqrstu",
        // Built with concat! so secret scanners don't flag these fake tokens.
        concat!(
            "SG",
            ".AbCdEfGhIjKlMnOpQrStUv.AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefg"
        ),
        "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
        "pypi-AgEIcHlwaS5vcmcCJGFiY2RlZmdoLWlqa2wtbW5vcC1xcnN0",
        "hvs.CAESIAbCdEfGhIjKlMnOpQrStUvWxYz0123",
        concat!("shpat", "_0123456789abcdef0123456789abcdef"),
        concat!(
            "dop_v1",
            "_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ),
        "lin_api_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789",
        "AGE-SECRET-KEY-1QQPQZRFR7ZZ2WCV7RXHYYHMQY4JNPXRLNMNJDTEGSWQKFYEXAMPLE",
        "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw",
    ];

    #[test]
    fn audit_leak_fixtures_are_redacted() {
        for (case, input, secret) in LEAK_FIXTURES {
            let out = text(input);
            assert!(
                !out.contains(secret),
                "{case}: must not contain {secret:?}: {out}"
            );
            assert!(out.contains(REDACTED), "{case}: {out}");
        }
    }

    #[test]
    fn identifiers_sharing_a_token_prefix_are_kept() {
        for s in [
            "hf_hub_download",
            "from huggingface_hub import hf_hub_url",
            "pypi-simple",
            "SG.config.value",
            "version 1:AA",
        ] {
            assert_eq!(text(s), s);
        }
    }

    #[test]
    fn signature_salt_and_pat_keys_are_secret_but_path_is_not() {
        for key in [
            "sig",
            "X-Amz-Signature",
            "X-Goog-Signature",
            "PASSWORD_SALT",
            "GITHUB_PAT",
            "azureDevopsPat",
            "webhook.signatures",
        ] {
            assert!(is_secret_key(key), "{key}");
            assert_eq!(value(key, "abc"), REDACTED, "{key}");
        }
        for key in [
            "PATH",
            "LD_LIBRARY_PATH",
            "PATTERN",
            "SIGNAL",
            "DESIGN",
            "SALTY",
            "COMPAT",
        ] {
            assert!(!is_secret_key(key), "{key}");
        }
        assert_eq!(
            text(
                "https://acct.blob.core.windows.net/c/b?sv=2022-11-02&se=2026&sig=AbCd%2Fef%3D&sp=r"
            ),
            "https://acct.blob.core.windows.net/c/b?sv=2022-11-02&se=2026&sig=<redacted>&sp=r"
        );
        let s3 = text("https://b.s3.amazonaws.com/k?X-Amz-Date=1&X-Amz-Signature=deadbeef01&x=1");
        assert!(!s3.contains("deadbeef01"), "{s3}");
        let gcs = text("https://storage.googleapis.com/b/o?X-Goog-Signature=cafe0123&y=2");
        assert!(!gcs.contains("cafe0123"), "{gcs}");
        let env = text("GITHUB_PAT=abc123secret\nPASSWORD_SALT: pepper99\nPATH=/usr/bin\n");
        assert!(
            !env.contains("abc123secret") && !env.contains("pepper99"),
            "{env}"
        );
        assert!(env.contains("PATH=/usr/bin"), "{env}");
        let json = text("{\"X-Amz-Signature\": \"feedface99\"}");
        assert!(!json.contains("feedface99"), "{json}");
        // camelCase keys in free text, but not words that merely start with one.
        let camel =
            text("azureDevopsPat: abc123\ngithubPat=def456\napiSig=ghi789\nmyPath=/usr/bin\n");
        assert!(
            !camel.contains("abc123") && !camel.contains("def456") && !camel.contains("ghi789"),
            "{camel}"
        );
        assert!(camel.contains("myPath=/usr/bin"), "{camel}");
        let pw = text("JAVA_TOOL_OPTIONS=-Dpw=hunter2 -Dx=1\n");
        assert!(!pw.contains("hunter2"), "{pw}");
    }

    #[test]
    fn every_token_prefix_redacted_anywhere_in_a_line() {
        for token in PREFIXED_TOKENS {
            for input in [
                format!("{token}\n"),
                format!("found {token} here\n"),
                format!("(\"{token}\")"),
            ] {
                let out = text(&input);
                assert!(!out.contains(token), "must not contain {token}: {out}");
            }
        }
    }

    #[test]
    fn rules_keep_surrounding_structure() {
        assert_eq!(
            text("2026-10-02 INFO connecting with password=hunter2\n"),
            "2026-10-02 INFO connecting with password=<redacted>\n"
        );
        assert_eq!(
            text("{\"user\":\"a\",\"password\":\"x9\"}"),
            "{\"user\":\"a\",\"password\":\"<redacted>\"}"
        );
        assert_eq!(
            text("GET /v1?token=a1&access_token=b2&page=2"),
            "GET /v1?token=<redacted>&access_token=<redacted>&page=2"
        );
        assert_eq!(
            text("//registry.npmjs.org/:_authToken=abc\n"),
            "//registry.npmjs.org/:_authToken=<redacted>\n"
        );
        assert_eq!(
            text("machine x login y password z\n"),
            "machine x login y password <redacted>\n"
        );
        assert_eq!(
            text("postgres://u:hun/ter2@db/app"),
            "postgres://u:<redacted>@db/app"
        );
        assert_eq!(
            text("mysql -uroot -pSECRET app"),
            "mysql -uroot -p<redacted> app"
        );
        assert_eq!(
            text("tls:\n  private_key: |\n    AAA\n    BBB\n\n  port: 443\n"),
            "tls:\n  private_key: |\n    <redacted>\n\n  port: 443\n"
        );
        assert_eq!(
            text("level=info api_key=abc path=/x\n"),
            "level=info api_key=<redacted> path=/x\n"
        );
    }

    #[test]
    fn non_secret_structure_kept() {
        for s in [
            "mkdir -p build\n",
            "use crate::session::Store;\n",
            "POSTGRES_PASSWORD:\n",
            "https://example.com/search?q=rust&page=2\n",
            "the monkey business and an author note\n",
            "run ssh-keygen -t ed25519\n",
        ] {
            assert_eq!(text(s), s);
        }
    }

    #[test]
    fn rule_order_starts_with_multiline_shapes() {
        let names: Vec<&str> = RULES.iter().map(|(name, _)| *name).collect();
        assert_eq!(names[..2], ["pem-block", "yaml-block-scalar"]);
        assert_eq!(names.last(), Some(&"token-prefix"));
        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(pos("url-userinfo") < pos("key-assignment"));
        assert!(pos("jwt") < pos("key-assignment"));
    }

    #[test]
    fn many_angle_brackets_do_not_overflow_the_stack() {
        let input = format!("password={}", ">".repeat(200_000));
        assert_eq!(text(&input), "password=><redacted>");
    }

    #[test]
    fn long_minified_line_with_many_matches_is_redacted() {
        let line = "\"token\":\"x\",".repeat(50_000);
        let out = text(&line);
        assert!(!out.contains("\"x\""));
    }

    #[test]
    fn header_value_ends_at_enclosing_quote() {
        assert_eq!(
            text("curl -H \"Authorization: Basic dXNlcg==\" -H 'Accept: x' url\n"),
            "curl -H \"Authorization: <redacted>\" -H 'Accept: x' url\n"
        );
        assert_eq!(
            text("{\"Authorization\": \"Bearer abc\", \"Accept\": \"x\"}"),
            "{\"Authorization\": \"<redacted>\", \"Accept\": \"x\"}"
        );
        assert_eq!(
            text("Set-Cookie: session=\"abc\"; Path=/\nnext\n"),
            "Set-Cookie: <redacted>\nnext\n"
        );
    }

    #[test]
    fn crlf_line_endings_are_kept() {
        assert_eq!(
            text("tls:\r\n  private_key: |\r\n    AAA\r\n  port: 443\r\n"),
            "tls:\r\n  private_key: |\r\n    <redacted>\r\n  port: 443\r\n"
        );
        assert_eq!(
            text("DB_PASSWORD=x\r\nPORT=1\r\n"),
            "DB_PASSWORD=<redacted>\r\nPORT=1\r\n"
        );
    }

    #[test]
    fn yaml_single_quote_escape_is_one_value() {
        assert_eq!(
            text("password: 'abc''def'\nport: 1\n"),
            "password: '<redacted>'\nport: 1\n"
        );
    }

    #[test]
    fn plain_values_keep_prose_with_secret_words() {
        assert_eq!(value("APP_NAME", "My Auth Service"), "My Auth Service");
        assert!(!is_secret("APP_NAME", "My Auth Service"));
        assert_eq!(value("OPTS", "--password=x"), "--password=<redacted>");
        assert_eq!(value("ARGS", "--password hunter2"), "--password <redacted>");
        assert!(is_secret("DB_ARGS", "mysql --password hunter2"));
    }

    #[test]
    fn quoted_header_name_with_unquoted_value_redacts_rest_of_line() {
        for (input, secret) in [
            ("\"Set-Cookie\": session=\"abc123\"; Path=/\n", "abc123"),
            (
                "\"Authorization\": Digest username=\"bob\", response=\"6629fae4\"\n",
                "6629fae4",
            ),
            ("{'Authorization': \"Basic abc\", 'x': 1}\n", "abc"),
        ] {
            let out = text(input);
            assert!(!out.contains(secret), "{input:?} -> {out}");
        }
        assert_eq!(
            text("{'Authorization': \"Basic abc\", 'x': 1}"),
            "{'Authorization': \"<redacted>\", 'x': 1}"
        );
    }

    #[test]
    fn security_review_leaks_are_redacted() {
        for (case, input, secret) in [
            (
                "tagged block",
                "password: !!binary |\n  aHVudGVyMg==\n",
                "aHVudGVyMg==",
            ),
            (
                "anchored block",
                "api_key: &k |\n  anchoredsecret\n",
                "anchoredsecret",
            ),
            (
                "quoted key with space",
                "\"api key\": |\n  hunter2\n",
                "hunter2",
            ),
            (
                "yaml list",
                "api_keys:\n  - customtok1\n  - customtok2\n",
                "customtok2",
            ),
            ("next-line scalar", "password:\n  hunter2\n", "hunter2"),
            ("flow sequence", "TOKEN=[abc123]\n", "abc123"),
            ("json array", "\"credentials\": [\"abc123\"]\n", "abc123"),
            (
                "multi-line json array",
                "\"tokens\": [\n  \"t0k1\"\n]\n",
                "t0k1",
            ),
            ("quote inside token", "INFO login password=ab\"cd\n", "cd"),
            ("shell concatenation", "x password='it'\\''s' y\n", "s' y"),
            (
                "multi-line quoted",
                "password: \"line1\n  line2\"\n",
                "line2",
            ),
            (
                "glued after jwt",
                "x password=eyJa.eyJb.sig~tail y\n",
                "tail",
            ),
            ("curl -u", "curl -u bob:curlpw1 https://x\n", "curlpw1"),
            (
                "curl --user",
                "curl --user 'bob:curlpw2' https://x\n",
                "curlpw2",
            ),
            ("xml element", "<password>xmlpw1</password>\n", "xmlpw1"),
        ] {
            let out = text(input);
            assert!(!out.contains(secret), "{case}: {input:?} -> {out:?}");
        }
        assert_eq!(
            text("api_keys:\n  - a1\n  # note\n  - name: x\nport: 1\n"),
            "api_keys:\n  - <redacted>\n  # note\n  - name: <redacted>\nport: 1\n"
        );
        assert_eq!(text("secrets: []\n"), "secrets: []\n");
        assert_eq!(
            text("x password='it'\\''s' y\n"),
            "x password='<redacted>' y\n"
        );
        assert_eq!(
            text("password: \"abc\nport: 1\n"),
            "password: \"<redacted>\nport: 1\n"
        );
        assert_eq!(
            text("<password>x</password>\n"),
            "<password><redacted></password>\n"
        );
    }

    #[test]
    fn third_review_cases() {
        for (case, input, secret) in [
            (
                "shell word with space",
                "x password='it'\\''s a secret' y\n",
                "secret",
            ),
            (
                "same-indent list",
                "api_keys:\n- abc111\n- def222\n",
                "def222",
            ),
            (
                "bracket in quoted element",
                "TOKEN=[\"ab]cd-secret\"]\n",
                "cd-secret",
            ),
        ] {
            let out = text(input);
            assert!(!out.contains(secret), "{case}: {input:?} -> {out:?}");
        }
        assert_eq!(
            text("x password='it'\\''s a secret' y\n"),
            "x password='<redacted>' y\n"
        );
        assert_eq!(
            text("api_keys:\n- a\n- b\nport: 1\n"),
            "api_keys:\n- <redacted>\n- <redacted>\nport: 1\n"
        );
        // A list item key's siblings at its own indent are not its values.
        assert_eq!(text("- secrets:\n- name: x\n"), "- secrets:\n- name: x\n");
        assert_eq!(
            text("<server password=\"x\">text</server>\n"),
            "<server password=\"<redacted>\">text</server>\n"
        );
        assert_eq!(
            text("<auth token=\"x\"/>\n"),
            "<auth token=\"<redacted>\"/>\n"
        );
        assert_eq!(
            text("password: \"abc\r\nport: 1\r\n"),
            "password: \"<redacted>\r\nport: 1\r\n"
        );
    }

    #[test]
    fn fourth_review_cases() {
        for (case, input, secret) in [
            ("nested mapping", "secrets:\n  db: hunter2\n", "hunter2"),
            (
                "list of mappings",
                "api_keys:\n  - name: prod\n    value: hunter3\n",
                "hunter3",
            ),
            (
                "json object",
                "\"secrets\": {\"db\": \"hunter4\"}\n",
                "hunter4",
            ),
            ("yaml flow mapping", "password: {v: hunter5}\n", "hunter5"),
            (
                "flow list item",
                "api_keys:\n  - [hunter6]\n  - {k: hunter7}\n",
                "hunter7",
            ),
            ("anchored header", "password: &pw\n  hunter8\n", "hunter8"),
            ("tagged header", "password: !!str\n  hunter9\n", "hunter9"),
            (
                "dotenv multi-line",
                "SECRET=\"line1\nline2\"\nPORT=1\n",
                "line2",
            ),
            (
                "yaml quoted with blank line",
                "password: \"a\n\n  bsecret\"\n",
                "bsecret",
            ),
            (
                "xml cdata",
                "<password><![CDATA[cdatapw]]></password>\n",
                "cdatapw",
            ),
            (
                "curl -u quoted with space",
                "curl -u 'bob:pa ss' https://x\n",
                "ss",
            ),
            (
                "discord webhook",
                "post https://discord.com/api/webhooks/123/abcDEF_token\n",
                "abcDEF_token",
            ),
            (
                "redis-cli -a",
                "redis-cli -h db -a r3dispw ping\n",
                "r3dispw",
            ),
        ] {
            let out = text(input);
            assert!(!out.contains(secret), "{case}: {input:?} -> {out:?}");
        }
        assert_eq!(
            text("secrets:\n  db:\n    file: ./db.txt\n  # c\nport: 1\n"),
            "secrets:\n  db:\n    file: <redacted>\n  # c\nport: 1\n"
        );
        assert_eq!(
            text("SECRET=\"a\nb\"\nPORT=1\n"),
            "SECRET=\"<redacted>\"\nPORT=1\n"
        );
        // An unclosed dotenv quote covers the rest of the text (here, under the bound).
        assert_eq!(text("SECRET=\"abc\nPORT=1\n"), "SECRET=\"<redacted>");
        assert_eq!(text("secrets: {}\n"), "secrets: {}\n");
    }

    #[test]
    fn fifth_review_cases() {
        // A service whose name merely contains a secret part keeps its subtree.
        let compose = "services:\n  keycloak:\n    image: quay.io/keycloak/keycloak:24\n    ports:\n      - \"8080:8080\"\n    environment:\n      KC_DB: postgres\n";
        assert_eq!(text(compose), compose);
        for header in [
            "api_keys",
            "secrets",
            "apiKeys",
            "DB_PASSWORD",
            "credentials",
            "auth",
        ] {
            assert!(names_a_secret(header), "{header}");
        }
        for header in ["keycloak", "authelia", "certbot", "passport", "keydb"] {
            assert!(!names_a_secret(header), "{header}");
        }
        // Mid-line quoted values stay on their line.
        let makefile = "check:\n\t@test -n \"$$API_KEY\" || (echo \"API_KEY=\"; exit 1)\nbuild:\n\tcargo build\ndeploy:\n\t./deploy --name \"x\"\n";
        let out = text(makefile);
        assert!(
            out.contains("build:\n\tcargo build\ndeploy:\n\t./deploy --name \"x\"\n"),
            "{out}"
        );
        assert_eq!(
            text("password: [it's, b]\nport: 1\nname: x\n"),
            "password: [<redacted>]\nport: 1\nname: x\n"
        );
        assert_eq!(text("TOKEN={\"a\": \"x}y\"}\n"), "TOKEN={<redacted>}\n");
        assert_eq!(
            text("password: {a: 1,\n  b: 2\n\nport: 1\n"),
            "password: {<redacted>\n\nport: 1\n"
        );
        for (input, secret) in [
            ("https://ptb.discord.com/api/webhooks/1/tok_a\n", "tok_a"),
            (
                "https://canary.discordapp.com/api/webhooks/1/tok_b\n",
                "tok_b",
            ),
            ("redis-cli --pass r3dis2 ping\n", "r3dis2"),
            ("curl --user=\"a:b c\" https://x\n", "b c"),
        ] {
            let out = text(input);
            assert!(!out.contains(secret), "{input:?} -> {out:?}");
        }
    }

    #[test]
    fn sixth_review_cases() {
        for (input, secret) in [
            ("requirepass hunter2\n", "hunter2"),
            ("masterauth hunter3\n", "hunter3"),
            ("docker login -u bob -p hunter4 ghcr.io\n", "hunter4"),
            ("sshpass -p hunter5 ssh host\n", "hunter5"),
            ("sshpass -phunter6 ssh host\n", "hunter6"),
            ("mongosh -u u -p hunter7 db\n", "hunter7"),
            ("htpasswd -bc .htpasswd bob hunter8\n", "hunter8"),
            ("htpasswd -b -B f bob hunter9\n", "hunter9"),
            ("password = \"\"\"\nhunter10\n\"\"\"\n", "hunter10"),
            ("password = \'\'\'\nhunter11\n\'\'\'\n", "hunter11"),
        ] {
            let out = text(input);
            assert!(!out.contains(secret), "{input:?} -> {out:?}");
        }
        assert_eq!(
            text("password = \"\"\"\nhunter\n\"\"\"\nport = 1\n"),
            "password = \"\"\"<redacted>\"\"\"\nport = 1\n"
        );
        // docker run -p publishes ports; not a password.
        assert_eq!(
            text("docker run -p 8080:80 nginx\n"),
            "docker run -p 8080:80 nginx\n"
        );
        assert_eq!(text("htpasswd -c f bob\n"), "htpasswd -c f bob\n");
        // A dotenv value longer than the bound is covered through the bound.
        let long = format!(
            "SECRET=\"a\n{}\"\n",
            "s3cr3t\n".repeat(MULTILINE_QUOTE_LINES + 5)
        );
        let out = text(&long);
        assert_eq!(
            out.matches("s3cr3t").count(),
            5,
            "only lines past the bound survive: {out}"
        );
    }

    #[test]
    fn seventh_review_cases() {
        for (input, secret) in [
            ("SECRET_KEY:=dev-secret-123\n", "dev-secret-123"),
            ("command: sh -c \"echo ${API_TOKEN:-abc123}\"\n", "abc123"),
            ("{\"pin_token\":123456}\n", "123456"),
            ("API_TOKEN ?= abc124\n", "abc124"),
            ("API_TOKEN += abc125\n", "abc125"),
            ("htpasswd -nbB admin hunter2\n", "hunter2"),
            ("htpasswd -nb u hunter3 >> .htpasswd\n", "hunter3"),
            ("htpasswd -bB -C 10 f bob hunter4\n", "hunter4"),
            ("authentication:\n  value: hunter5\n", "hunter5"),
            ("passphrase:\n  - hunter6\n", "hunter6"),
            ("oauth2:\n  data: hunter7\n", "hunter7"),
            ("run: docker login -u bob -p hunter8 ghcr.io\n", "hunter8"),
        ] {
            let out = text(input);
            assert!(!out.contains(secret), "{input:?} -> {out:?}");
        }
        assert_eq!(
            text("htpasswd -nb u p >> f\n"),
            "htpasswd -nb u <redacted> >> f\n"
        );
        for kept in [
            "Token refresh fails after up\n",
            "\tmkcert localhost\n",
            "docker run -d --name mongo -p 27017:27017 mongo:6\n",
            "docker login ghcr.io && docker run -p 8080:80 x\n",
            "image: keycloak:24\n",
            "password = \"\"\"\"\"\"\n",
        ] {
            assert_eq!(text(kept), kept);
        }
        assert_eq!(text("requirepass hunter9\n"), "requirepass <redacted>\n");
    }

    #[test]
    fn eighth_review_cases() {
        for (input, secret) in [
            ("curl -H \"DD-API-KEY:abc123\" https://x\n", "abc123"),
            ("curl -H \"X-Vault-Token:hvs.x1\" https://x\n", "hvs.x1"),
            ("{\"password\":123456}\n", "123456"),
            ("password:hunter2 user=x\n", "hunter2"),
            ("echo $(htpasswd -nbB admin hunter3) | sed x\n", "hunter3"),
            ("htpasswd -b f bob 'a b'\n", "b'"),
            ("authorization:\n  basic: dXNlcjpw\n", "dXNlcjpw"),
            ("keystore:\n  value: ks1\n", "ks1"),
            ("az login -u x -p azpw1\n", "azpw1"),
            ("helm registry login -u x -p helmpw1 r\n", "helmpw1"),
            ("run: vsce publish -p vscetok1\n", "vscetok1"),
            ("sqlcmd -S db -U sa -P sqlpw1\n", "sqlpw1"),
            ("cf auth bob cfpw1\n", "cfpw1"),
            ("password = \"\"\"ab\\\"\"\"cd\"\"\"\n", "cd"),
        ] {
            let out = text(input);
            assert!(!out.contains(secret), "{input:?} -> {out:?}");
        }
        for kept in [
            "image: keycloak:24\n",
            "host: redis-session:6379\n",
            "docker run -d --name mongo -p 27017:27017 mongo:6\n",
        ] {
            assert_eq!(text(kept), kept);
        }
        assert!(names_a_secret("oauth2-proxy"));
    }

    #[test]
    fn many_assignments_after_long_indentation_stay_fast() {
        let line = format!("{}{}\n", " ".repeat(200_000), "KEY=1 ".repeat(50_000));
        let start = std::time::Instant::now();
        let out = text(&line);
        assert!(!out.contains("KEY=1"));
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn makefile_recipes_are_not_redacted() {
        let makefile = "keys:\n\tssh-keygen -t ed25519 -f id\n\nrotate-secrets: |\n\t./rotate.sh\n";
        assert_eq!(text(makefile), makefile);
    }

    #[test]
    fn earlier_redaction_does_not_shield_rest_of_line() {
        for input in [
            "Cookie: -----BEGIN X-----a-----END X-----; sid=SECRET1\n",
            "password=-----BEGIN X-----a-----END X----- SECRET1\n",
        ] {
            let out = text(input);
            assert!(!out.contains("SECRET1"), "{input:?} -> {out}");
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
