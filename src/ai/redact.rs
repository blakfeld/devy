//! Redaction and size caps applied to everything sent to an AI provider.
//!
//! Redaction is deliberately over-eager: a key that merely contains `KEY` loses its value.
//! Losing a harmless value costs a little context; leaking a credential costs much more.
//!
//! [`text`] applies an ordered list of rules ([`RULES`]). Multi-line shapes (PEM blocks,
//! YAML block scalars) go first so later, line-oriented rules never see their bodies;
//! specific credential shapes go before the generic key-based rule so each keeps its
//! surrounding structure; well-known token prefixes are the final catch-all.

use regex::{Captures, Match, Regex};
use std::borrow::Cow;
use std::ops::Range;
use std::sync::LazyLock;

pub const REDACTED: &str = "<redacted>";

/// What rules write in place of a secret while [`RULES`] run; the public entry points turn
/// it into [`REDACTED`] at the end. Rules that leave earlier redactions alone (or rely on
/// them) look for this marker, which input cannot contain (see [`with_marks`]), so a
/// literal `<redacted>` in the input is never taken for one.
const MARK: &str = "\u{E000}redacted\u{E000}";

/// The private-use character that delimits [`MARK`].
const MARK_CHAR: char = '\u{E000}';

/// Runs `rules` on `s` with every [`MARK_CHAR`] in it neutralised first (replaced by
/// U+FFFD), so only rules can write [`MARK`], and turns each [`MARK`] into [`REDACTED`].
fn with_marks(s: &str, rules: impl FnOnce(String) -> String) -> String {
    rules(neutralised(s).into_owned()).replace(MARK, REDACTED)
}

/// `s` with every [`MARK_CHAR`] replaced by U+FFFD.
fn neutralised(s: &str) -> Cow<'_, str> {
    if s.contains(MARK_CHAR) {
        Cow::Owned(s.replace(MARK_CHAR, "\u{FFFD}"))
    } else {
        Cow::Borrowed(s)
    }
}
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
    with_marks(value, |input| {
        RULES.iter().fold(input, |acc, (name, rule)| match *name {
            "key-assignment" => key_assignments_with(&acc, false),
            "curl-user" => curl_users_with(&acc, false),
            _ => rule(&acc),
        })
    })
}

/// [`credential_parts`] for a value that is shown as one item (an `environment` value in
/// the executable-entry summary): a secret-named assignment inside it hides only its own value up
/// to the next whitespace or quote, never the rest of the value, so
/// `TOKEN=1 sh -c 'curl x|sh'` keeps the command visible.
pub fn credential_parts_inline(value: &str) -> String {
    // A leading non-key character means no assignment is at the start of a line, which
    // is what makes the key rule take the rest of the line.
    const LEAD: &str = "\u{1}";
    // One line: a line break would start a new line where the key rule takes the rest of
    // it (the summary collapses whitespace runs anyway).
    let marked = format!("{LEAD}{}", value.replace(['\n', '\r'], " "));
    let out = with_marks(&marked, |input| {
        RULES.iter().fold(input, |acc, (name, rule)| match *name {
            "key-assignment" => key_assignments_with(&acc, false),
            "curl-user" => curl_users_with(&acc, false),
            _ => rule(&acc),
        })
    });
    out.strip_prefix(LEAD).map(str::to_string).unwrap_or(out)
}

/// Whether `value` would be changed by redaction under `key`. Only a redaction counts:
/// the neutralised [`MARK_CHAR`] (see [`with_marks`]) does not.
pub fn is_secret(key: &str, val: &str) -> bool {
    value(key, val) != *neutralised(val)
}

/// Redacts free text by applying every rule in [`RULES`], in order.
pub fn text(s: &str) -> String {
    with_marks(s, |input| {
        RULES.iter().fold(input, |acc, (_name, rule)| rule(&acc))
    })
}

/// One redaction rule: rewrites text, replacing what it matches with [`MARK`] (shown as
/// [`REDACTED`]).
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
    ("exec-form-arg", exec_form_args),
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
        .replace_all(s, MARK)
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
            out.push_str(MARK);
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
    let mapping = yaml_mapping_item();
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
                        out.push_str(&format!("{lead}{} {MARK}{ending}", &m[1]));
                    }
                    _ => out.push_str(raw),
                }
            } else if let Some(item) = trimmed.strip_prefix("- ") {
                // A `- |` item's body lines are redacted as plain scalars below it.
                if item.trim().is_empty() || item.starts_with(['|', '>']) {
                    out.push_str(raw);
                } else {
                    out.push_str(&format!("{lead}- {MARK}{ending}"));
                }
            } else if trimmed == "-" {
                out.push_str(raw);
            } else {
                out.push_str(&format!("{lead}{MARK}{ending}"));
            }
            i += 1;
        }
    }
    out
}

/// A YAML mapping entry, optionally a list item: `key:`, `- key: value`, `"k": v`. Group 1
/// is the key with its colon (and any `- `), group 2 the value.
fn yaml_mapping_item() -> &'static Regex {
    regex!(r#"^((?:- )?(?:"[^"\n]*"|'[^'\n]*'|[^\s:"'#][^:\n]*?)[ \t]*:)(?:[ \t]+(.*))?$"#)
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
            || pass == MARK
            || regex!(r"^\$\{[A-Za-z_][A-Za-z0-9_]*\}$").is_match(pass)
        {
            return c[0].to_string();
        }
        format!("{}{}:{MARK}@{}{}", &c[1], &c[2], &c[4], &c[5])
    })
    .into_owned()
}

/// Slack and Discord webhook URLs, whose path is the credential.
fn webhook_urls(s: &str) -> String {
    regex!(
        r"(https?://(?:hooks\.slack\.com/(?:services|workflows|triggers)/|(?:(?:ptb|canary)\.)?discord(?:app)?\.com/api/webhooks/))[A-Za-z0-9/_\-]+"
    )
    .replace_all(s, format!("${{1}}{MARK}"))
    .into_owned()
}

/// URL query parameters whose name names a secret (`?token=…&access_token=…`).
fn query_parameters(s: &str) -> String {
    regex!(r#"([?&])([A-Za-z0-9_.\-\[\]%]+)=([^&#\s'"]+)"#)
        .replace_all(s, |c: &Captures| {
            if is_secret_key(&c[2]) {
                format!("{}{}={MARK}", &c[1], &c[2])
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
            out.push_str(MARK);
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
        .replace_all(s, format!("${{1}}{MARK}"))
        .into_owned()
}

/// JWTs: three dot-separated base64url segments, the first starting with `eyJ`.
fn jwts(s: &str) -> String {
    regex!(r"\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]*")
        .replace_all(s, MARK)
        .into_owned()
}

/// The password glued to `-p` on a MySQL/MariaDB client command line (`mysql -pSECRET`).
fn mysql_password_flags(s: &str) -> String {
    redact_flag_values(
        s,
        regex!(
            r#"(\b(?:mysql|mysqldump|mysqladmin|mysqlimport|mysqlsh|mariadb|mariadb-dump)\b[^\n]*?[ \t]-p)("[^"\n]*"|'[^'\n]*'|[^\s'"]+)"#
        ),
    )
}

/// Applies a command-line rule whose group 1 is the text before the value and group 2
/// the value: each value becomes [`MARK`], except for the bytes that are the structure of
/// an exec-form flow sequence ([`FlowScan::gaps`]).
///
/// So in a plain flow sequence the sequence's separators survive and the value of a flag
/// with no value of its own is left to [`exec_form_args`]: `[mysql, -ppw]` →
/// `[mysql, -p<redacted>]`, and `[sqlcmd, -P, pw]`, whose "value" is the `,`, is left
/// alone (the sequence's walker redacts `pw`). Anywhere else the value is redacted whole,
/// `,` and `]` included. The structure is computed only for a text with a match.
fn redact_flag_values(s: &str, rule: &Regex) -> String {
    let mut gaps: Option<Vec<Range<usize>>> = None;
    rule.replace_all(s, |c: &Captures| {
        let gaps = gaps.get_or_insert_with(|| flow_scan(s).gaps);
        let value = c.get(2).expect("group 2 always participates");
        let mut out = c[1].to_string();
        push_value_keeping_structure(s, value.range(), gaps, &mut out);
        out
    })
    .into_owned()
}

/// Writes `s[value]` to `out` as [`MARK`], keeping only the bytes of an exec-form flow
/// sequence's structure (`gaps`, see [`FlowScan::gaps`]) that are structure themselves
/// (see [`is_structure_byte`]). Each run of other bytes becomes one marker.
fn push_value_keeping_structure(
    s: &str,
    value: Range<usize>,
    gaps: &[Range<usize>],
    out: &mut String,
) {
    let bytes = s.as_bytes();
    // Whether the last byte was hidden, so its run's marker is already written.
    let mut hiding = false;
    let mut gap = gaps.partition_point(|gap| gap.end <= value.start);
    for i in value {
        while gaps.get(gap).is_some_and(|g| g.end <= i) {
            gap += 1;
        }
        let in_gap = gaps.get(gap).is_some_and(|g| g.start <= i);
        // Only structure and blank bytes of a gap stay visible, whatever else a gap
        // might hold. They are ASCII, so pushing them as chars keeps the text intact.
        if in_gap && is_structure_byte(bytes[i]) {
            out.push(char::from(bytes[i]));
            hiding = false;
        } else if !hiding {
            out.push_str(MARK);
            hiding = true;
        }
    }
}

/// Whether `b` is a byte of flow-sequence structure that a command-line value may leave
/// visible: a bracket, brace, `,`, quote or blank. A mapping's `:` is not one, since a
/// password may hold a `:` (`mysql -pab:cd` in a flow mapping).
fn is_structure_byte(b: u8) -> bool {
    matches!(
        b,
        b'[' | b']' | b'{' | b'}' | b',' | b'"' | b'\'' | b' ' | b'\t' | b'\r' | b'\n'
    )
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
    .replace_all(s, format!("${{1}}{MARK}"))
    .into_owned()
}

/// The password after `sqlcmd -P`.
fn sqlcmd_passwords(s: &str) -> String {
    redact_flag_values(
        s,
        regex!(r#"(\bsqlcmd\b[^\n;&|]*?[ \t]-P[ \t]*)("[^"\n]*"|'[^'\n]*'|[^\s'"]+)"#),
    )
}

/// The password in `cf auth <user> <password>`.
fn cf_auth_passwords(s: &str) -> String {
    regex!(
        r#"(\bcf[ \t]+auth[ \t]+(?:"[^"\n]*"|'[^'\n]*'|\S+)[ \t]+)("[^"\n]*"|'[^'\n]*'|[^\s'"]+)"#
    )
    .replace_all(s, format!("${{1}}{MARK}"))
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
                Some(&(start, len)) if flags.contains('b') => {
                    format!("htpasswd{}{MARK}{}", &args[..start], &args[start + len..])
                }
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
        .replace_all(s, format!("${{1}}{MARK}"))
        .into_owned()
}

/// The password in each `curl -u user:password` / `--user user:password` argument, in one
/// left-to-right pass over the text.
///
/// The pass keeps the shell's quote state (single and double quotes, `\` escapes outside
/// single quotes, an unclosed quote lasting to the line end) only to tell where a command
/// ends: at an unquoted `;`, `&`, `|` or the line end. A `curl` word anywhere, even in a
/// quoted span, starts a curl command, which only ever hides more. Inside one, every
/// `-u`/`--user` at a word start (after a blank, `=`, a quote or the line start) is a
/// flag, and [`curl_user_argument`] redacts its argument (curl uses the last one).
///
/// Flags are not looked for again in text an argument covered, though the pass still
/// walks it for quotes, command ends and `curl` words (`a:pw|curl -u b:x`). An argument
/// that is itself a flag (`-u -u c:x`, `-u=-u=…`) is left for the pass to reach. Each
/// byte is visited by the pass once and by at most one argument, so the work is linear.
fn curl_users(s: &str) -> String {
    curl_users_with(s, true)
}

/// [`curl_users`], where a key that ends a password's hidden tail, or takes the next
/// word as its value, must be one the key rule takes as an assignment with the same
/// `spaced` setting (see [`key_assignments_with`]): that rule redacts what is left.
fn curl_users_with(s: &str, spaced: bool) -> String {
    let bytes = s.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut gaps = None;
    let mut out = String::with_capacity(s.len());
    // Where the text not yet written starts, and up to where an argument was parsed.
    let mut pos = 0;
    let mut covered = 0;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut in_curl = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let prev = i.checked_sub(1).map(|p| bytes[p]);
        // Only ASCII bytes are looked at as positions, so slicing there is safe.
        if in_curl
            && b == b'-'
            && i >= covered
            && prev.is_none_or(|p| matches!(p, b' ' | b'\t' | b'\n' | b'=' | b'"' | b'\''))
            // (A continued line's `-u` follows its indentation or the line break.)
            && let Some(argument) = curl_user_flag(s, i)
        {
            // An argument that is itself a flag at a word start is left for the pass.
            let flag_argument = curl_user_flag(s, argument).is_some()
                && matches!(bytes[argument - 1], b' ' | b'\t' | b'=');
            if !flag_argument {
                out.push_str(&s[pos..argument]);
                let (end, scanned) = curl_user_argument(s, argument, &mut out, &mut gaps, spaced);
                pos = end;
                covered = scanned.max(end);
            }
        }
        if !prev.is_some_and(is_word)
            && bytes[i..].starts_with(b"curl")
            && !bytes.get(i + 4).is_some_and(|&n| is_word(n))
        {
            in_curl = true;
        }
        let literal = std::mem::take(&mut escaped);
        match (quote, b) {
            // `\` then a line break continues the command on the next line, `\r\n`
            // included.
            (_, b'\r') if literal && bytes.get(i + 1) == Some(&b'\n') => escaped = true,
            (_, b'\n') if !literal => {
                quote = None;
                in_curl = false;
            }
            _ if literal => {}
            (Some(b'\''), b'\'') | (Some(b'"'), b'"') => quote = None,
            (Some(b'"'), b'\\') | (None, b'\\') => escaped = true,
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b';' | b'&' | b'|') => in_curl = false,
            _ => {}
        }
        i += 1;
    }
    out.push_str(&s[pos..]);
    out
}

/// Where the argument of a `-u` or `--user` flag at `i` starts (after the flag and its
/// blanks or `=`), if one is there. `--user` followed by a letter or `-`
/// (`--user-agent`) is another option; `-u` may have its argument glued (`-ua:b`).
fn curl_user_flag(s: &str, i: usize) -> Option<usize> {
    let rest = &s[i..];
    let after = if rest.starts_with("--user")
        && !rest[6..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '-')
    {
        i + 6
    } else if rest.starts_with("-u") {
        i + 2
    } else {
        return None;
    };
    let rest = &s[after..];
    let sep = if rest.starts_with('=') {
        1
    } else {
        rest.len() - rest.trim_start_matches([' ', '\t']).len()
    };
    Some(after + sep)
}

/// Redacts the password of the `user:password` argument at `start` into `out`, which
/// holds `s` up to `start`. Returns where the text not yet written starts, and how far
/// the argument was looked at (at least that far): for a quoted password, the whole
/// shell word it ends, though a key glued in it is left to later rules.
///
/// - A quote before the user (`'bob:pa ss'`) or right after its `:` (`bob:"p w"`) bounds
///   the password up to the matching closing quote (`\"` escapes a `"`). Its quotes are
///   kept. An empty password (`"bob:"`) is kept.
/// - Text glued to the closing quote is part of the password's shell word, so it is
///   redacted too, up to a key that starts an assignment of its own (`"pw"--password x`),
///   which later rules redact.
/// - An unquoted password runs to a blank or quote, as on HEAD, so every character of a
///   word such as `Sup3r(Secret)` or `pa&zq` is hidden. It keeps the structure of an
///   exec-form flow sequence around it (see [`push_value_keeping_structure`]); `gaps`
///   caches that structure. A key glued to its end takes the next word as its value
///   (see [`redact_glued_key_values`]).
/// - Without a `:` there is no password.
fn curl_user_argument(
    s: &str,
    start: usize,
    out: &mut String,
    gaps: &mut Option<Vec<Range<usize>>>,
    spaced: bool,
) -> (usize, usize) {
    let is_quote = |c: char| matches!(c, '"' | '\'');
    let open = s[start..].chars().next().filter(|&c| is_quote(c));
    let user = start + open.map_or(0, char::len_utf8);
    let colon = user
        + s[user..]
            .find(|c: char| c.is_whitespace() || c == ':' || is_quote(c))
            .unwrap_or(s.len() - user);
    if !s[colon..].starts_with(':') {
        return (start, colon);
    }
    let mut value = colon + 1;
    let quote = open.or_else(|| {
        let q = s[value..].chars().next().filter(|&c| is_quote(c))?;
        value += q.len_utf8();
        Some(q)
    });
    let Some(q) = quote else {
        let len = s[value..]
            .find(|c: char| c.is_whitespace() || is_quote(c))
            .unwrap_or(s.len() - value);
        out.push_str(&s[start..value]);
        let gaps = gaps.get_or_insert_with(|| flow_scan(s).gaps);
        push_value_keeping_structure(s, value..value + len, gaps, out);
        let end = redact_glued_key_values(s, value..value + len, out, spaced);
        return (end, end);
    };
    let len = quoted_len(&s[value..], q);
    if len == 0 {
        return (start, value);
    }
    out.push_str(&s[start..value]);
    out.push_str(MARK);
    let mut end = value + len;
    if s[end..].starts_with(q) {
        out.push(q);
        end += q.len_utf8();
        let tail = command_word_tail(&s[end..]);
        let word_end = end + tail;
        if tail > 0 {
            let stop = assignment_in(s, end, word_end, spaced).unwrap_or(word_end);
            if stop > end {
                out.push_str(MARK);
            }
            // The text after `stop` is left to later rules, but it was looked at: the
            // whole word is covered, so it is never searched for flags again.
            return (stop, word_end);
        }
    }
    (end, end)
}

/// Where the first assignment that [`key_assignments_with`] would take as one, with the
/// same `spaced` setting, starts in `s[start..end]` (the part of its match from `start`
/// on), if any. The search sees one character before `start` and one past `end`.
fn assignment_in(s: &str, start: usize, end: usize, spaced: bool) -> Option<usize> {
    let before = s[..start].chars().next_back().map_or(0, char::len_utf8);
    let haystack = through_next_char(s, end);
    let mut at = start - before;
    while let Some(c) = assignment_regex().captures_at(haystack, at) {
        let key = c.get(1).expect("group 1 always participates");
        let sep = c.get(2).expect("group 2 always participates");
        if key.start() >= end {
            return None;
        }
        if !not_an_assignment(s, key, sep, spaced, false) {
            // A match that starts at a marker's closing character ends the hidden text
            // after it, so the marker stays whole.
            let at = c.get(0).expect("group 0 always participates").start();
            let at = if s[at..].starts_with(MARK_CHAR) {
                at + MARK_CHAR.len_utf8()
            } else {
                at
            };
            return Some(at.max(start));
        }
        at = key.end();
    }
    None
}

/// Exec-form command arguments, one sequence element per argument: the element after a
/// secret-valued flag in a flow sequence (`["redis-server", "--requirepass", "x"]`, quoted
/// or plain, possibly over several lines or nested) or a block sequence
/// (`- --requirepass` / `- x`), and a value glued inside one element (`--password=x`,
/// `-px` after a MySQL client). Each element's quotes and the separators between elements
/// are kept.
fn exec_form_args(s: &str) -> String {
    let flow = redact_spans(s, &flow_scan(s).redactions);
    let block = block_sequence_redactions(&flow);
    redact_spans(&flow, &block)
}

/// `s` with each of `spans` (sorted, non-overlapping byte ranges) replaced by [`MARK`].
fn redact_spans(s: &str, spans: &[Range<usize>]) -> String {
    if spans.is_empty() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    for span in spans {
        out.push_str(&s[pos..span.start]);
        out.push_str(MARK);
        pos = span.end;
    }
    out.push_str(&s[pos..]);
    out
}

/// Which tools an exec-form command has named so far, for flags whose meaning depends
/// on the tool (`-a` is a password only for `redis-cli`).
#[derive(Default)]
struct ExecTools {
    /// `redis-cli`: `-a`/`--pass` take the password.
    redis_cli: bool,
    /// `sqlcmd`: `-P` takes the password.
    sqlcmd: bool,
    /// `curl`: `-u`/`--user` take `user:password`.
    curl: bool,
    /// `<tool> login`, `sshpass` or a MongoDB tool: `-p` takes the password.
    spaced_p: bool,
    /// A MySQL/MariaDB client or `sshpass`: the password is glued to `-p` (`-px`).
    glued_p: bool,
}

impl ExecTools {
    /// Notes the tool named by a non-flag element (matched by basename, so
    /// `/usr/local/bin/redis-cli` counts). `index` is the element's position: `login`
    /// counts only after the tool it belongs to.
    fn see(&mut self, arg: &str, index: usize) {
        let base = arg.rsplit(['/', '\\']).next().unwrap_or(arg);
        let base = base.strip_suffix(".exe").unwrap_or(base);
        match base {
            "redis-cli" => self.redis_cli = true,
            "sqlcmd" => self.sqlcmd = true,
            "curl" => self.curl = true,
            "login" if index > 0 => self.spaced_p = true,
            "sshpass" => {
                self.spaced_p = true;
                self.glued_p = true;
            }
            "mongo" | "mongosh" | "mongodump" | "mongorestore" | "mongoexport" | "mongoimport" => {
                self.spaced_p = true
            }
            // `mysql`, `mysqladmin`, `mysqlcheck`, …, `mariadb`, `mariadb-admin`, ….
            _ if base.starts_with("mysql") || base == "mariadb" || base.starts_with("mariadb-") => {
                self.glued_p = true;
            }
            _ => {}
        }
    }

    /// What the element after a flag (no `=`) is under these tools.
    fn next_after(&self, flag: &str) -> NextArg {
        match flag {
            "-a" | "--pass" if self.redis_cli => NextArg::Password,
            "-P" if self.sqlcmd => NextArg::Password,
            "-p" if self.spaced_p => NextArg::Password,
            "-u" | "--user" if self.curl => NextArg::UserInfo,
            _ => NextArg::Plain,
        }
    }
}

/// What the next element of an exec-form command holds, given the flag before it.
#[derive(Clone, Copy, PartialEq)]
enum NextArg {
    /// An ordinary argument.
    Plain,
    /// A password after a tool's password flag (`redis-cli -a`): redacted unless it is
    /// itself a flag.
    Password,
    /// A value after a secret-named flag (`--requirepass`): redacted even when it starts
    /// with `-`, unless it is a long flag (`--port`) or `--`, since a password may well
    /// start with a dash and a short `-x` is as likely a value as a flag.
    Secret,
    /// `user:password` after `curl -u`: redacted after its first `:`.
    UserInfo,
}

/// Whether `arg` is unmistakably a flag: `--` or a long `--name`.
fn is_long_flag(arg: &str) -> bool {
    arg == "--"
        || arg
            .strip_prefix("--")
            .is_some_and(|name| name.starts_with(|c: char| c.is_ascii_alphabetic()))
}

/// Whether a command-line flag (`--requirepass`, `--api-key`, `-Dpw`) names a secret, by
/// the same key rule as [`key_assignments`]. The rule sees the whole flag, dashes
/// included, since `-Dpw` is a secret only because of its `-D` prefix.
fn is_secret_flag(flag: &str) -> bool {
    regex!(format!("^(?:{})$", secret_key_pattern())).is_match(flag)
}

/// `span` of `s`, unless it is empty, already redacted, or a literal `<redacted>` from the
/// input. That one is left as written: it shows nothing, and turning it into [`MARK`]
/// would let a later rule take it for a rule's redaction (see
/// [`starts_with_redacted_element`]).
fn secret_span(s: &str, span: Range<usize>) -> Option<Range<usize>> {
    (!matches!(&s[span.clone()], "" | MARK | REDACTED)).then_some(span)
}

/// The password part of a `user:password` element `span`: everything after its first
/// `:`. Without a `:` there is no password, as in [`curl_users`].
fn userinfo_password(s: &str, span: Range<usize>) -> Option<Range<usize>> {
    let colon = s[span.clone()].find(':')?;
    secret_span(s, span.start + colon + 1..span.end)
}

/// The parts of one exec-form command to redact. `elements` are the byte ranges in `s`
/// of the command's elements, in order, each without its quotes; the result is sorted.
///
/// The element after a secret-named flag or a tool's password flag is redacted (see
/// [`NextArg`] for when a dashed element still counts as the value). A value glued inside
/// an element (`--password=x`, `-px` for MySQL clients and `sshpass`, curl's
/// `-uuser:x`) is redacted within it. A MySQL `-p` on its own prompts, so its next
/// element (the database) is kept.
fn exec_form_redactions(s: &str, elements: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut tools = ExecTools::default();
    let mut next = NextArg::Plain;
    for (index, element) in elements.iter().enumerate() {
        let arg = &s[element.clone()];
        let is_flag = arg.len() > 1 && arg.starts_with('-');
        let value = match std::mem::replace(&mut next, NextArg::Plain) {
            NextArg::Secret if !is_long_flag(arg) => secret_span(s, element.clone()),
            NextArg::Password if !is_flag => secret_span(s, element.clone()),
            NextArg::UserInfo if !is_flag => userinfo_password(s, element.clone()),
            NextArg::Plain if !is_flag => {
                tools.see(arg, index);
                continue;
            }
            _ => None,
        };
        if value.is_some() || !is_flag {
            out.extend(value);
            continue;
        }
        let glued = |offset: usize| element.start + offset..element.end;
        // A flag followed only by blanks (`"--password "`) is a bare flag.
        let flag_arg = arg.trim_end_matches([' ', '\t']);
        // A flag word with its value in the same element, after blanks (`"-P pw"`).
        let word = flag_arg.find([' ', '\t']).unwrap_or(flag_arg.len());
        let redaction = if flag_arg.len() > 2
            && ((tools.glued_p && arg.starts_with("-p")) || (tools.sqlcmd && arg.starts_with("-P")))
        {
            // An element is one argument, so `-pX` (MySQL clients, `sshpass`) and `-PX`
            // (`sqlcmd`) hold a glued password, blanks included (`"-pmy pass"`).
            secret_span(s, glued(2))
        } else if flag_arg.len() > 2 && tools.curl && arg.starts_with("-u") {
            userinfo_password(s, glued(2))
        } else if let Some(eq) = flag_arg[..word].find('=') {
            let flag = &arg[..eq];
            if is_secret_flag(flag) {
                secret_span(s, glued(eq + 1))
            } else if tools.curl && flag == "--user" {
                userinfo_password(s, glued(eq + 1))
            } else {
                None
            }
        } else if word < flag_arg.len() {
            let flag = &flag_arg[..word];
            let value = word + (arg.len() - word - arg[word..].trim_start().len());
            match tools.next_after(flag) {
                _ if is_secret_flag(flag) => secret_span(s, glued(value)),
                NextArg::Password => secret_span(s, glued(value)),
                NextArg::UserInfo => userinfo_password(s, glued(value)),
                _ => None,
            }
        } else {
            next = if is_secret_flag(flag_arg) {
                NextArg::Secret
            } else {
                tools.next_after(flag_arg)
            };
            None
        };
        out.extend(redaction);
    }
    out
}

/// What [`flow_scan`] found in a text's exec-form flow sequences.
#[derive(Default)]
struct FlowScan {
    /// The exec-form redactions (see [`exec_form_redactions`]), sorted.
    redactions: Vec<Range<usize>>,
    /// The structure of every fully scanned sequence: the bytes of its span that are
    /// neither in one of its scalars nor in a comment (brackets, separators, quotes,
    /// blanks), sorted.
    gaps: Vec<Range<usize>>,
}

/// Scans every exec-form flow sequence (`[ … ]`) of `s`. A sequence opens at a `[` in a
/// value position (see [`opens_exec_form`]) and is scanned by [`scan_flow`]. Scanning
/// resumes where each sequence ended or was given up, so every byte is scanned once.
fn flow_scan(s: &str) -> FlowScan {
    let mut scan = FlowScan::default();
    let mut pos = 0;
    while let Some(offset) = s[pos..].find('[') {
        let open = pos + offset;
        pos = if opens_exec_form(s, open) {
            scan_flow(s, open, &mut scan)
        } else {
            open + 1
        };
    }
    // An inner sequence closes, and adds its redactions, before its parent does. The
    // spans never overlap (a nested collection is an empty element of its parent).
    scan.redactions.sort_unstable_by_key(|span| span.start);
    scan
}

/// Adds to `gaps` the parts of `span` outside `scalars` (sorted, inside `span`).
fn push_gaps(gaps: &mut Vec<Range<usize>>, span: Range<usize>, scalars: &[Range<usize>]) {
    let mut pos = span.start;
    for scalar in scalars {
        if scalar.start > pos {
            gaps.push(pos..scalar.start);
        }
        pos = scalar.end;
    }
    if span.end > pos {
        gaps.push(pos..span.end);
    }
}

/// The longest word [`last_word`] looks back over for YAML node properties, which keeps
/// that look-back bounded.
const MAX_WORD: usize = 256;

/// The longest Dockerfile `--flag[=value]` option [`after_dockerfile_instruction`] looks
/// back over. `--mount=type=secret,…` and `--mount=type=bind,source=…,target=…` options
/// with long paths stay well under it, and the bound keeps each look-back constant-time.
const MAX_DOCKERFILE_FLAG: usize = 4096;

/// How many `--flag` options may sit between a Dockerfile instruction and its `[`.
const MAX_DOCKERFILE_FLAGS: usize = 8;

/// `s` without trailing spaces and tabs, and whether there were any.
fn trim_blank_end(s: &str) -> (&str, bool) {
    let trimmed = s.trim_end_matches([' ', '\t']);
    (trimmed, trimmed.len() < s.len())
}

/// The last whitespace-delimited word of `s` and the text before it, when `s` ends in a
/// word of at most `max` bytes.
fn last_word(s: &str, max: usize) -> Option<(&str, &str)> {
    match s
        .bytes()
        .rev()
        .take(max + 1)
        .position(|b| b.is_ascii_whitespace())
    {
        Some(0) => None,
        Some(n) => Some((&s[s.len() - n..], &s[..s.len() - n])),
        None if !s.is_empty() && s.len() <= max => Some((s, "")),
        None => None,
    }
}

/// Whether the `[` at `open` can start an exec-form sequence: at the start of a line,
/// after `:`, `=` or a `- ` list marker, inside another sequence (after `[` or `,`), or
/// after a Dockerfile `CMD`, `ENTRYPOINT`, `RUN` or `HEALTHCHECK` word and its `--flag`
/// options. YAML node properties (`&anchor`, `!!seq`) may come before the `[`.
fn opens_exec_form(s: &str, open: usize) -> bool {
    let (mut before, mut spaced) = trim_blank_end(&s[..open]);
    for _ in 0..2 {
        match last_word(before, MAX_WORD) {
            Some((word, lead)) if spaced && word.starts_with(['&', '!']) => {
                (before, spaced) = trim_blank_end(lead);
            }
            _ => break,
        }
    }
    match before.chars().next_back() {
        None | Some('\n' | '\r' | ':' | '=' | '[' | ',') => true,
        Some('-') => {
            let lead = &before[..before.len() - 1];
            spaced && (lead.is_empty() || lead.ends_with(char::is_whitespace))
        }
        Some(_) => spaced && after_dockerfile_instruction(before),
    }
}

/// Whether `before` ends in a Dockerfile exec-form instruction word (in any case, as
/// Dockerfile instructions are), possibly followed by up to [`MAX_DOCKERFILE_FLAGS`]
/// `--flag[=value]` options of at most [`MAX_DOCKERFILE_FLAG`] bytes each
/// (`RUN --mount=type=secret,id=x`).
///
/// The look-back stops at the first word that is neither, so it never passes another
/// spaced `[` (a word of its own), and the look-backs of different `[`s cover disjoint
/// text: the scan stays linear.
fn after_dockerfile_instruction(before: &str) -> bool {
    let mut rest = before;
    for _ in 0..=MAX_DOCKERFILE_FLAGS {
        let Some((word, lead)) = last_word(rest, MAX_DOCKERFILE_FLAG) else {
            return false;
        };
        if ["CMD", "ENTRYPOINT", "RUN", "HEALTHCHECK", "SHELL"]
            .iter()
            .any(|instruction| word.eq_ignore_ascii_case(instruction))
        {
            return true;
        }
        let (trimmed, spaced) = trim_blank_end(lead);
        if !word.starts_with("--") || !spaced {
            return false;
        }
        rest = trimmed;
    }
    false
}

/// One open collection while [`scan_flow`] scans a flow sequence.
enum FlowFrame {
    /// A `[ … ]` sequence: one command, with its elements so far.
    Sequence(Vec<Range<usize>>),
    /// A `{ … }` mapping, which is one opaque element of its parent.
    Mapping,
}

/// Scans the flow sequence opened by the `[` at `open`, adding the redactions of each
/// sequence in it to `out` (and, once the outermost `]` is reached, its structure), and
/// returns where scanning should resume: just past the outermost `]`, or where the scan
/// was given up.
///
/// Quoted elements may contain `,` and `]`; plain ones run to the next separator, the end
/// of the line, or a ` #` comment. The sequence may span lines and hold comments. Each
/// `[ … ]` inside it is a command of its own, with its own tools; in its parent, it and
/// any `{ … }` mapping are one opaque element (never redacted), and the parent's
/// remaining elements continue its command (`[x, [y], --password, z]`).
///
/// The scan is given up at a blank line (as in [`flow_collection`]), an unclosed quote, a
/// mismatched close or anything else out of place. Sequences already closed by then keep
/// their redactions. Each byte is scanned once, however deep the nesting.
fn scan_flow(s: &str, open: usize, out: &mut FlowScan) -> usize {
    let mut stack = vec![FlowFrame::Sequence(Vec::new())];
    // Every scalar and comment so far, in order, for the structure of the whole
    // collection. Comments are not elements: they never reach the walker.
    let mut scalars: Vec<Range<usize>> = Vec::new();
    let mut i = open + 1;
    // Whether the last thing scanned was an element, so a separator or a close is next.
    let mut after_element = false;
    loop {
        i = match skip_flow_space(s, i, &mut scalars) {
            Ok(i) => i,
            Err(stop) => return stop,
        };
        let Some(ch) = s[i..].chars().next() else {
            return i;
        };
        let in_mapping = matches!(stack.last(), Some(FlowFrame::Mapping));
        if std::mem::take(&mut after_element) {
            match ch {
                ',' => {
                    i += 1;
                    continue;
                }
                ':' if in_mapping => {
                    i += 1;
                    continue;
                }
                ']' | '}' => {}
                _ => return i,
            }
        }
        let element = match ch {
            '[' => {
                stack.push(FlowFrame::Sequence(Vec::new()));
                i += 1;
                continue;
            }
            '{' => {
                stack.push(FlowFrame::Mapping);
                i += 1;
                continue;
            }
            // A close, also of an empty collection or after a trailing comma.
            ']' | '}' => {
                match (stack.pop(), ch) {
                    (Some(FlowFrame::Sequence(elements)), ']') => {
                        out.redactions.extend(exec_form_redactions(s, &elements));
                    }
                    (Some(FlowFrame::Mapping), '}') => {}
                    _ => return i,
                }
                i += 1;
                if stack.is_empty() {
                    push_gaps(&mut out.gaps, open..i, &scalars);
                    return i;
                }
                i..i
            }
            '"' | '\'' => {
                // A quoted element may continue onto indented lines, as YAML allows.
                let body = &s[i + 1..];
                let len = quoted_len_until(body, ch, Span::Indented);
                let first_line = body[..len].find('\n');
                // An unclosed quote, or one spanning lines that a separator or close does
                // not follow (so it is likely not a string at all), gives up the sequence
                // on its first line, so sequences on the next lines are still scanned.
                let closes = body[len..].starts_with(ch)
                    && (first_line.is_none()
                        || skip_flow_space(s, i + len + 2, &mut Vec::new()).is_ok_and(|j| {
                            matches!(s.as_bytes().get(j), Some(b',' | b']' | b'}'))
                                || (in_mapping && s.as_bytes().get(j) == Some(&b':'))
                        }));
                if !closes {
                    let line_end = first_line
                        .or_else(|| body[len..].find('\n').map(|n| len + n))
                        .unwrap_or(body.len());
                    return i + 1 + len.min(line_end);
                }
                let element = i + 1..i + 1 + len;
                i += len + 2;
                element
            }
            _ => {
                let len = plain_flow_len(&s[i..], in_mapping);
                if len == 0 {
                    return i;
                }
                let element = i..i + s[i..i + len].trim_end().len();
                i += len;
                element
            }
        };
        if !element.is_empty() {
            scalars.push(element.clone());
        }
        if let Some(FlowFrame::Sequence(elements)) = stack.last_mut() {
            elements.push(element);
        }
        after_element = true;
    }
}

/// The length of a plain flow scalar at the start of `rest`: up to a `,`, `]`, `[`, `{`,
/// the end of the line, or a `#` after whitespace (a comment); in a mapping, also up to a
/// `}`, or a `:` that is a mapping indicator (followed by a blank, the end of the line or
/// one of `,[]{}`, as YAML has it), so `ab:cd` is one scalar.
fn plain_flow_len(rest: &str, in_mapping: bool) -> usize {
    let bytes = rest.as_bytes();
    bytes
        .iter()
        .enumerate()
        .position(|(i, &b)| match b {
            b',' | b']' | b'[' | b'{' | b'\n' | b'\r' => true,
            b'}' => in_mapping,
            b':' => {
                in_mapping
                    && bytes.get(i + 1).is_none_or(|next| {
                        matches!(
                            next,
                            b' ' | b'\t' | b'\r' | b'\n' | b',' | b'[' | b']' | b'{' | b'}'
                        )
                    })
            }
            b'#' => i > 0 && matches!(bytes[i - 1], b' ' | b'\t'),
            _ => false,
        })
        .unwrap_or(rest.len())
}

/// The position of the next character at or after `i` that is neither whitespace nor part
/// of a comment (`#` at the start of a line or after whitespace, to the end of the line),
/// or `Err` with the position of a blank line's end, which ends a flow sequence. A comment
/// line is not blank. Each comment's range is added to `comments`, so a comment is never
/// taken for sequence structure (see [`FlowScan::gaps`]).
fn skip_flow_space(
    s: &str,
    mut i: usize,
    comments: &mut Vec<Range<usize>>,
) -> Result<usize, usize> {
    let bytes = s.as_bytes();
    let mut newline = false;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\n' if newline => return Err(i),
            b'\n' => newline = true,
            b' ' | b'\t' | b'\r' => {}
            b'#' if i == 0 || bytes[i - 1].is_ascii_whitespace() => {
                newline = false;
                let end = i + s[i..].find('\n').unwrap_or(s.len() - i);
                comments.push(i..end);
                i = end;
                continue;
            }
            _ => break,
        }
        i += 1;
    }
    Ok(i)
}

/// The redactions in every exec-form block sequence of `s`: each maximal run of `- item`
/// lines at one indent, where the item is a scalar (see [`block_item`]). Blank and comment
/// lines inside a run are skipped; any other line ends it. The parent key does not
/// matter, so `command:`, `args:` and nested lists are all covered.
fn block_sequence_redactions(s: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for raw in s.split_inclusive('\n') {
        lines.push((offset, raw.trim_end_matches(['\n', '\r'])));
        offset += raw.len();
    }
    let mut out = Vec::new();
    let mut run: Vec<Range<usize>> = Vec::new();
    let mut run_indent = None;
    let mut i = 0;
    while i < lines.len() {
        let (_, line) = lines[i];
        let indent = indent_of(line);
        if line[indent..].is_empty() || line[indent..].starts_with('#') {
            i += 1;
            continue;
        }
        // YAML forbids tabs in indentation, so a tab-indented line is not an item.
        let item = (!line[..indent].contains('\t'))
            .then(|| block_item(s, &lines, i))
            .flatten();
        if run_indent != Some(indent) || item.is_none() {
            out.extend(exec_form_redactions(s, &run));
            run.clear();
            run_indent = None;
        }
        match item {
            Some((element, next)) => {
                run.push(element);
                run_indent = Some(indent);
                i = next;
            }
            None => i += 1,
        }
    }
    out.extend(exec_form_redactions(s, &run));
    out
}

/// The length of a line's indentation (spaces and tabs).
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

/// The scalar of the `- item` line `lines[index]` (each line is its offset in `s` and its
/// text without the line ending), as a byte range of `s`, with the index of the first
/// line after it. The scalar is:
/// - for a quoted item, its body, which may continue onto indented lines;
/// - for a `|`/`>` block scalar, its indented body lines;
/// - for a plain item, the rest of the line up to a ` #` comment, plus any deeper
///   continuation lines.
///
/// Node properties (`&anchor`, `!tag`) before the scalar are skipped. Mappings
/// (`- key: value`), nested collections and aliases are not scalars.
fn block_item(s: &str, lines: &[(usize, &str)], index: usize) -> Option<(Range<usize>, usize)> {
    let (line_start, line) = lines[index];
    let indent = indent_of(line);
    let marker = &line[indent..];
    let after = marker
        .strip_prefix("- ")
        .or_else(|| marker.strip_prefix("-\t"))?;
    let mut start = line.len() - after.trim_start_matches([' ', '\t']).len();
    for _ in 0..2 {
        let rest = &line[start..];
        if !rest.starts_with(['&', '!']) {
            break;
        }
        // A property with no scalar after it on the line is not a scalar item.
        let end = rest.find([' ', '\t'])?;
        start += end + indent_of(&rest[end..]);
    }
    let item = line[start..].trim_end();
    if item.is_empty()
        || item == "-"
        || item.starts_with("- ")
        || item.starts_with("-\t")
        || item.starts_with(['[', '{', '#', '*', '&', '!'])
    {
        return None;
    }
    let at = line_start + start;
    let first = item.chars().next()?;
    if matches!(first, '|' | '>') {
        let header = &item[1..];
        let header =
            header.trim_start_matches(|c: char| c.is_ascii_digit() || c == '+' || c == '-');
        let header = header.trim_start();
        if !(header.is_empty() || header.starts_with('#')) {
            return None;
        }
        return Some(block_scalar_body(lines, index, indent));
    }
    if yaml_mapping_item().is_match(item) {
        return None;
    }
    if matches!(first, '"' | '\'') {
        let body = &s[at + 1..];
        let len = quoted_len_until(body, first, Span::Indented);
        let end = at + 1 + len;
        let next = lines[index..]
            .iter()
            .position(|&(offset, _)| offset > end)
            .map_or(lines.len(), |n| index + n);
        if body[len..].starts_with(first) {
            // Only a comment may follow the closing quote.
            let (last_start, last) = lines[next - 1];
            let after = last[end + 1 - last_start..].trim_start();
            if !(after.is_empty() || after.starts_with('#')) {
                return None;
            }
        }
        return Some((at + 1..end, next));
    }
    let comment = [" #", "\t#"].iter().filter_map(|c| item.find(c)).min();
    let first_len = comment.map_or(item.len(), |c| item[..c].trim_end().len());
    if comment.is_some() {
        return Some((at..at + first_len, index + 1));
    }
    // A plain scalar continues onto deeper lines (blank lines between them included)
    // that are not comments or list items of their own.
    let mut end = at + first_len;
    let mut next = index + 1;
    let mut j = index + 1;
    while let Some(&(offset, l)) = lines.get(j) {
        let lead = indent_of(l);
        let text = &l[lead..];
        j += 1;
        if text.is_empty() {
            continue;
        }
        if lead <= indent
            || l[..lead].contains('\t')
            || text.starts_with('#')
            || text == "-"
            || text.starts_with("- ")
        {
            break;
        }
        let comment = [" #", "\t#"].iter().filter_map(|c| text.find(c)).min();
        let text = comment.map_or(text, |c| text[..c].trim_end());
        end = offset + lead + text.len();
        next = j;
        if comment.is_some() {
            break;
        }
    }
    Some((at..end, next))
}

/// The body of the `- |` / `- >` block scalar item at `lines[index]` (whose `-` is at
/// `indent`): the lines after it that are blank or indented deeper, from the first
/// body line's text to the last one's end, with the index of the line after the body.
/// An empty body is an empty range at the header's end.
fn block_scalar_body(
    lines: &[(usize, &str)],
    index: usize,
    indent: usize,
) -> (Range<usize>, usize) {
    let (header_start, header) = lines[index];
    let mut body: Option<Range<usize>> = None;
    let mut next = index + 1;
    let mut j = index + 1;
    while let Some(&(offset, l)) = lines.get(j) {
        let lead = indent_of(l);
        j += 1;
        if l[lead..].is_empty() {
            continue;
        }
        // YAML forbids tabs in indentation, so a tab-indented line ends the body.
        if lead <= indent || l[..lead].contains('\t') {
            break;
        }
        let text = offset + lead..offset + l.trim_end().len();
        body = Some(body.map_or(text.clone(), |b| b.start..text.end));
        next = j;
    }
    let end = header_start + header.len();
    (body.unwrap_or(end..end), next)
}

/// The text of an XML element whose name names a secret (`<password>x</password>`).
fn xml_elements(s: &str) -> String {
    regex!(format!(
        r#"(<{}(?:[ \t][^>\n]*)?>)(?:(?s:<!\[CDATA\[.*?\]\]>)|[^<\n]+)(</)"#,
        secret_key_pattern()
    ))
    .replace_all(s, format!("${{1}}{MARK}${{2}}"))
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

/// The regex of a `key<sep>value` assignment whose key names a secret: group 1 is the key,
/// group 2 the separator (`=` forms, `:`, or blanks).
fn assignment_regex() -> &'static Regex {
    regex!(format!(
        r#"(?:^|[^A-Za-z0-9_.\-])["']?({})["']?([ \t]*(?:[?+!]|::?)?=[ \t]*|[ \t]*:[ \t]*|[ \t]+)"#,
        secret_key_pattern()
    ))
}

/// [`key_assignments`], optionally ignoring the space-separated `key value` form for
/// bare-word keys (see [`spaced_assignment`]).
fn key_assignments_with(s: &str, spaced: bool) -> String {
    let assignment = assignment_regex();
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
        if not_an_assignment(s, key, sep, spaced, alone) {
            // Resume at the separator, so it can precede the next key (`auth token=x`).
            // The key is non-empty, so this still moves past the match start.
            out.push_str(&s[pos..sep.start()]);
            pos = sep.start();
            continue;
        }
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
        out.push_str(&s[pos..value_start]);
        let colon = sep.as_str().contains(':') && !sep.as_str().contains('=');
        // A key opening a quoted sequence element (`["PASSWORD=x y", "z"]`) ends with the
        // element: its value is the rest of the element (see [`element_value_len`]).
        let (replacement, consumed) = match element_quote(s, key.start())
            .filter(|_| sep.start() == key.end())
            .and_then(|q| element_value_len(&s[value_start..], q))
        {
            Some(len) => (MARK.to_string(), len),
            None => redact_value(&s[value_start..], form, colon),
        };
        out.push_str(&replacement);
        pos = value_start + consumed;
        if matches!(form, ValueForm::Token | ValueForm::Spaced) && replacement == MARK {
            pos = redact_glued_key_values(s, value_start..pos, &mut out, spaced);
        }
    }
    out.push_str(&s[pos..]);
    out
}

/// After the redacted word `word`, already written to `out`: when the word ends in a key
/// glued to it whose own value is the next word (`token=abc;password: x`), that word is
/// redacted too, and so on while each redacted word ends in such a key (`password:
/// b;secret: x`). A next word that [`key_assignments_with`] takes as an assignment of its
/// own is left to it (`spaced` as there). Returns where the text not yet written starts.
fn redact_glued_key_values(
    s: &str,
    mut word: Range<usize>,
    out: &mut String,
    spaced: bool,
) -> usize {
    let assignment = assignment_regex();
    let mut pos = word.end;
    // Set when the last word redacted might have been a key with a long separator: the
    // word after the separator is then redacted too.
    let mut forced = None;
    while let Some(colon) = forced
        .take()
        .or_else(|| ends_in_glued_key(assignment, s, word.clone()))
    {
        let blanks = s[pos..].len() - s[pos..].trim_start_matches([' ', '\t']).len();
        let start = pos + blanks;
        // A comment (`# note`) is not a value.
        let comment = s[start..].starts_with('#')
            && s[start + 1..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace);
        if blanks == 0 || comment {
            break;
        }
        match next_word_kind(assignment, s, start, spaced) {
            NextWord::Assignment => break,
            NextWord::Unknown => forced = Some(false),
            NextWord::Value => {}
        }
        let (next, len) = redact_value(&s[start..], ValueForm::Spaced, colon);
        if len == 0 {
            break;
        }
        out.push_str(&s[pos..start]);
        out.push_str(&next);
        pos = start + len;
        if next != MARK {
            break;
        }
        word = start..pos;
    }
    pos
}

/// Whether [`key_assignments_with`] rejects a match of its regex as an assignment (`key`
/// and `sep` are its groups 1 and 2; `spaced` and `alone` as for [`spaced_assignment`]).
fn not_an_assignment(s: &str, key: Match, sep: Match, spaced: bool, alone: bool) -> bool {
    let value_start = sep.end();
    let is_spaced = !sep.as_str().contains(['=', ':']);
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
    // A spaced value is absent when a comment follows the key (`--password # c`),
    // or when a secret flag ends its sequence element (after the element's close, or
    // as a flag element with trailing blanks) and the next element is
    // [`exec_form_args`]'s (`"--password" , "<redacted>"`).
    let value_absent = is_spaced && {
        let rest = &s[value_start..];
        let secret_flag = key.as_str().starts_with('-') && is_secret_flag(key.as_str());
        (secret_flag
            && (rest
                .strip_prefix(',')
                .is_some_and(starts_with_redacted_element)
                // A flag element with only blanks after it (`["--password ", "x"]`)
                // ends at its own closing quote; its value is the next element,
                // which exec-form has redacted (or the next block item, or none).
                || element_quote(s, key.start())
                    .and_then(|q| rest.strip_prefix(q))
                    .map(|after| after.trim_start_matches([' ', '\t']))
                    .is_some_and(|after| {
                        after.is_empty()
                            || after.starts_with(['\n', '\r', ']'])
                            || after
                                .strip_prefix(',')
                                .is_some_and(starts_with_redacted_element)
                    })))
            || (rest.starts_with('#') && rest[1..].chars().next().is_none_or(char::is_whitespace))
    };
    glued_colon
        || value_absent
        || (is_spaced && !spaced_assignment(key.as_str(), sep.as_str(), spaced, alone))
}

/// `s` up to `end` and the character there, if any: enough for [`assignment_regex`] to
/// see the blank after a word, without searching past it.
fn through_next_char(s: &str, end: usize) -> &str {
    &s[..end + s[end..].chars().next().map_or(0, char::len_utf8)]
}

/// Whether the redacted word `word` ends in a key glued to it whose value is the next
/// word (`abc;password:`, `abc#--requirepass`), and if so, whether its separator is a
/// colon (for [`redact_value`]).
///
/// The key qualifies when its match (`assignment`) starts inside the word (or just
/// before it, for a key that is the whole word), and its `:` or `=` separator ends the
/// word, or it is a secret flag
/// (`--requirepass`) ending the word, followed by a blank. The search sees the word and
/// one character past it, so each word is searched once and the scan stays linear.
fn ends_in_glued_key(assignment: &Regex, s: &str, word: Range<usize>) -> Option<bool> {
    let haystack = through_next_char(s, word.end);
    // From the character before the word, so a key that is the whole word (`--token`,
    // `api_key:`) counts too.
    let mut at = word.start
        - s[..word.start]
            .chars()
            .next_back()
            .map_or(0, char::len_utf8);
    let mut glued = None;
    while let Some(c) = assignment.captures_at(haystack, at) {
        let whole = c.get(0).expect("group 0 always participates");
        let key = c.get(1).expect("group 1 always participates");
        let sep = c.get(2).expect("group 2 always participates");
        if whole.start() >= word.end {
            break;
        }
        let sep_text = sep.as_str();
        glued = if sep_text.contains([':', '=']) {
            (sep.start() + sep_text.trim_end().len() == word.end)
                .then(|| sep_text.contains(':') && !sep_text.contains('='))
        } else {
            (key.end() == word.end && key.as_str().starts_with('-') && is_secret_flag(key.as_str()))
                .then_some(false)
        };
        at = key.end();
    }
    glued
}

/// What the word after a glued key is (see [`next_word_kind`]).
#[derive(Clone, Copy, PartialEq)]
enum NextWord {
    /// An assignment the key rule takes as one, left to it.
    Assignment,
    /// A value.
    Value,
    /// A key-like word whose separator runs past the bound: both it and the word after
    /// the separator are redacted.
    Unknown,
}

/// Whether the word at `start` begins with an assignment that [`key_assignments_with`]
/// would take as one: a key match at the word's first key character (after any leading
/// punctuation, as in `;--password`) that [`not_an_assignment`] accepts.
///
/// The search sees at most a few characters of leading punctuation, the key, a quote and
/// a separator (bounded), never the rest of a long word, so calling this for every value
/// keeps the scan linear. A separator past the bound gives [`NextWord::Unknown`].
fn next_word_kind(assignment: &Regex, s: &str, start: usize, spaced: bool) -> NextWord {
    match next_word_is_assignment(assignment, s, start, spaced) {
        Some(true) => NextWord::Assignment,
        Some(false) => NextWord::Value,
        None => NextWord::Unknown,
    }
}

/// [`next_word_kind`] as `Some(is an assignment)`, or `None` past the separator bound.
fn next_word_is_assignment(
    assignment: &Regex,
    s: &str,
    start: usize,
    spaced: bool,
) -> Option<bool> {
    const MAX_LEAD: usize = 8;
    const MAX_SEP: usize = 64;
    let is_key_byte = |b: &u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-');
    let bytes = s.as_bytes();
    let Some(lead) = bytes[start..].iter().take(MAX_LEAD).position(is_key_byte) else {
        return Some(false);
    };
    if bytes[start..start + lead]
        .iter()
        .any(u8::is_ascii_whitespace)
    {
        return Some(false);
    }
    let first_key_char = start + lead;
    let key_end = first_key_char
        + bytes[first_key_char..]
            .iter()
            .position(|b| !is_key_byte(b))
            .unwrap_or(bytes.len() - first_key_char);
    let mut sep_end = key_end + usize::from(matches!(bytes.get(key_end), Some(b'"' | b'\'')));
    let sep_len = bytes[sep_end..]
        .iter()
        .take(MAX_SEP + 1)
        .take_while(|b| matches!(b, b' ' | b'\t' | b'?' | b'+' | b'!' | b':' | b'='))
        .count();
    // A longer separator is not looked at: the word and the one after it are then
    // redacted, which may hide too much but never too little.
    if sep_len > MAX_SEP {
        return None;
    }
    sep_end += sep_len;
    let before = s[..start].chars().next_back().map_or(0, char::len_utf8);
    let Some(c) = assignment.captures_at(through_next_char(s, sep_end), start - before) else {
        return Some(false);
    };
    let key = c.get(1).expect("group 1 always participates");
    let sep = c.get(2).expect("group 2 always participates");
    Some(key.start() == first_key_char && !not_an_assignment(s, key, sep, spaced, false))
}

/// Whether `rest` starts (after blanks and an optional quote) with a whole redacted
/// element: [`MARK`] (never a literal `<redacted>`) followed by a quote, a separator,
/// whitespace or the end.
fn starts_with_redacted_element(rest: &str) -> bool {
    let rest = rest.trim_start_matches([' ', '\t']);
    let rest = rest.strip_prefix(['"', '\'']).unwrap_or(rest);
    rest.strip_prefix(MARK).is_some_and(|after| {
        after
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ']' | '}'))
    })
}

/// The quote that opens the sequence element a key starts, when the key at `key_start`
/// is the start of a quoted flow or block element (after `[`, `,` or a `- ` list
/// marker). A caller that bounds the value requires the quote to be still open past the
/// key, since a JSON key (`"key": "value"`) closes its quote before the separator.
fn element_quote(s: &str, key_start: usize) -> Option<char> {
    let before = &s[..key_start];
    let q = before
        .chars()
        .next_back()
        .filter(|c| matches!(c, '"' | '\''))?;
    let outer = &before[..before.len() - 1];
    let lead = outer.trim_end_matches([' ', '\t', '\r', '\n']);
    let opens = match lead.chars().next_back() {
        Some('[' | ',') => true,
        Some('-') => {
            let marker_lead = &lead[..lead.len() - 1];
            lead.len() < outer.len()
                && (marker_lead.is_empty() || marker_lead.ends_with(char::is_whitespace))
        }
        _ => false,
    };
    opens.then_some(q)
}

/// The length of the value at the start of `rest` that ends a quoted sequence element
/// opened by `q`: up to the closing quote, when that closes the element (only
/// whitespace, then `,`, `]`, `}`, a comment or the line end, may follow it). `None` when
/// the value is empty or the quote does not close the element (`"TOKEN="abc"`,
/// `"TOKEN=ab"cd"`), so the value is measured as if no element bounded it.
fn element_value_len(rest: &str, q: char) -> Option<usize> {
    let len = quoted_len(rest, q);
    let after = rest[len..].strip_prefix(q)?.trim_start_matches([' ', '\t']);
    let closes = after.is_empty() || after.starts_with([',', ']', '}', '#', '\n', '\r']);
    (len > 0 && closes).then_some(len)
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
    if let Some(after) = rest.strip_prefix(MARK)
        && !matches!(form, ValueForm::RestOfLine)
    {
        let tail = glued_len(after, "&\"',;)]}#");
        return (MARK.to_string(), MARK.len() + tail);
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
                return (format!("{first}{MARK}"), len + 1);
            }
            // Shell concatenation (`'it'\''s a secret'`) continues the value past the quote.
            let tail = shell_word_tail(&body[len + 1..]);
            return (format!("{first}{MARK}{first}"), len + 2 + tail);
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
        (MARK.to_string(), len)
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
            return (format!("{delim}{MARK}{delim}"), 3 + i + 3);
        }
        if ch == '\n' {
            newlines += 1;
            if newlines > MULTILINE_QUOTE_LINES {
                stop = i;
                break;
            }
        }
    }
    (format!("{delim}{MARK}"), 3 + stop)
}

/// The length of the rest of a shell word glued to a closing quote: further quoted
/// segments (which may contain spaces), `\`-escaped characters and plain characters, up
/// to unquoted whitespace or structure (`,;)]}&#<>` and `/>`). A `|` is part of the word,
/// as it is of a YAML, dotenv or JSON value (`TOKEN="ab"|x`).
fn shell_word_tail(s: &str) -> usize {
    shell_word_tail_until(s, ",;)]}&#<>")
}

/// [`shell_word_tail`] for a shell command line (curl's `-u`), where a `|` also ends the
/// word.
fn command_word_tail(s: &str) -> usize {
    shell_word_tail_until(s, ",;)]}&|#<>")
}

/// [`shell_word_tail`] up to unquoted whitespace, `/>` or one of `stops`.
fn shell_word_tail_until(s: &str, stops: &str) -> usize {
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
            _ if ch.is_whitespace() || stops.contains(ch) || s[i..].starts_with("/>") => {
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
                        (format!("{open}{MARK}{close}"), i + 1)
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
    (format!("{open}{MARK}"), stop)
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
    .replace_all(s, format!("${{1}}{MARK}"))
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
        // Exec-form arguments go after the command-line rules, so those still see every
        // tool name (and keep exec-form separators, see `redact_flag_values`), and before
        // the key rule (so glued values keep their element's bounds).
        assert_eq!(pos("curl-user") + 1, pos("exec-form-arg"));
        for rule in [
            "mysql-password-flag",
            "redis-cli-password",
            "command-password-flag",
            "htpasswd",
            "sqlcmd-password",
            "cf-auth",
            "curl-user",
        ] {
            assert!(pos(rule) < pos("exec-form-arg"), "{rule}");
        }
        assert!(pos("exec-form-arg") < pos("xml-element"));
        assert!(pos("exec-form-arg") < pos("key-assignment"));
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
    fn exec_form_secret_args_redacted() {
        for (input, expected) in [
            (
                "command: [\"redis-server\", \"--requirepass\", \"hunter2\"]\n",
                "command: [\"redis-server\", \"--requirepass\", \"<redacted>\"]\n",
            ),
            (
                "test: [\"CMD\", \"redis-cli\", \"-a\", \"hunter2\", \"ping\"]\n",
                "test: [\"CMD\", \"redis-cli\", \"-a\", \"<redacted>\", \"ping\"]\n",
            ),
            (
                "command:\n  - redis-server\n  - --requirepass\n  - hunter2\n",
                "command:\n  - redis-server\n  - --requirepass\n  - <redacted>\n",
            ),
            (
                "command: [redis-server, --requirepass, hunter2]\n",
                "command: [redis-server, --requirepass, <redacted>]\n",
            ),
            (
                "[\"mongod\", \"--password\", \"hunter2\"]\n",
                "[\"mongod\", \"--password\", \"<redacted>\"]\n",
            ),
            (
                "[\"sqlcmd\", \"-P\", \"hunter2\"]\n",
                "[\"sqlcmd\", \"-P\", \"<redacted>\"]\n",
            ),
            (
                "test: [\"CMD\", \"mysqladmin\", \"ping\", \"-phunter2\"]\n",
                "test: [\"CMD\", \"mysqladmin\", \"ping\", \"-p<redacted>\"]\n",
            ),
            (
                "command: [\n  \"redis-server\",\n  \"--requirepass\",\n  \"hunter2\"\n]\n",
                "command: [\n  \"redis-server\",\n  \"--requirepass\",\n  \"<redacted>\"\n]\n",
            ),
        ] {
            assert_eq!(text(input), expected, "{input:?}");
        }
    }

    #[test]
    fn exec_form_glued_value_keeps_structure() {
        assert_eq!(
            text("args: [\"--api-key=abc123\", \"--port\", \"80\"]\n"),
            "args: [\"--api-key=<redacted>\", \"--port\", \"80\"]\n"
        );
        assert_eq!(
            text("command: [\"mysqld\", \"--password=hunter2\"]\n"),
            "command: [\"mysqld\", \"--password=<redacted>\"]\n"
        );
    }

    #[test]
    fn exec_form_non_secret_args_kept() {
        for kept in [
            "command: [\"redis-server\", \"--port\", \"6380\", \"--appendonly\", \"yes\"]\n",
            // A separate MySQL `-p` prompts; the next argument is the database (D4).
            "command: [\"mysql\", \"-p\", \"appdb\"]\n",
            "command: [\"x\", \"--no-auth\", \"--port\", \"1\"]\n",
            "packages:\n  - git\n  - curl\n  - jq\n  - ripgrep\n",
        ] {
            assert_eq!(text(kept), kept);
        }
    }

    /// The parts [`exec_form_redactions`] redacts from `args`, one element each.
    fn exec_form_walk(args: &[&str]) -> Vec<String> {
        let joined = args.join("\n");
        let mut elements = Vec::new();
        let mut start = 0;
        for arg in args {
            elements.push(start..start + arg.len());
            start += arg.len() + 1;
        }
        exec_form_redactions(&joined, &elements)
            .into_iter()
            .map(|span| joined[span].to_string())
            .collect()
    }

    #[test]
    fn exec_form_walker_redacts_flag_values() {
        for (args, expected) in [
            (
                &["/usr/local/bin/redis-cli", "-a", "pw", "ping"][..],
                &["pw"][..],
            ),
            (&["C:\\redis\\redis-cli.exe", "-a", "pw"], &["pw"]),
            (&["redis-cli", "--pass", "pw"], &["pw"]),
            (&["redis-server", "--requirepass", "pw"], &["pw"]),
            (&["redis-server", "--masterauth", "pw"], &["pw"]),
            (&["app", "--token", "t", "--api-key", "k"], &["t", "k"]),
            (&["sqlcmd", "-S", "db", "-P", "pw"], &["pw"]),
            (&["docker", "login", "-u", "bob", "-p", "pw"], &["pw"]),
            (&["sshpass", "-p", "pw", "ssh"], &["pw"]),
            (&["sshpass", "-ppw", "ssh"], &["pw"]),
            (&["mongosh", "-u", "u", "-p", "pw"], &["pw"]),
            (&["mysql", "-uroot", "-ppw", "app"], &["pw"]),
            (&["mariadb-dump", "-ppw"], &["pw"]),
            (&["x", "--password=pw", "--api-key=k"], &["pw", "k"]),
            (&["java", "-Dpw=pw"], &["pw"]),
            // After a secret-named flag, a short dashed element is the value.
            (&["x", "--token", "-v"], &["-v"]),
            (&["x", "--password", "-"], &["-"]),
            (&["mariadb-admin", "-ppw"], &["pw"]),
            (&["mysqlcheck", "-ppw"], &["pw"]),
            (&["curl", "-u", "a:b:c"], &["b:c"]),
            (&["curl", "--user=a:b", "-ua:c"], &["b", "c"]),
        ] {
            assert_eq!(exec_form_walk(args), expected, "{args:?}");
        }
    }

    #[test]
    fn exec_form_walker_keeps_other_args() {
        for args in [
            // Tool-scoped flags mean something else without their tool.
            &["redis-server", "-a", "x"][..],
            &["x", "-P", "8080"],
            &["docker", "run", "-p", "8080:80", "nginx"],
            &["login", "-p", "x"],
            // A separate MySQL `-p` prompts; the next element is the database.
            &["mysql", "-p", "appdb"],
            // A flag after a secret-named flag is not its value.
            &["x", "--no-auth", "--port", "1"],
            &["x", "--token", "--verbose"],
            &["x", "--token", "--"],
            &["redis-cli", "-a", "-h"],
            &["curl", "-u", "admin"],
            &["wget", "--user=a:b"],
            &["x", "--port=80", "--password="],
            // Already redacted by an earlier rule, or a literal `<redacted>` (left as
            // written, so no later rule takes it for a rule's redaction).
            &["x", "--password", MARK],
            &["x", "--password", "<redacted>"],
            &["x", "--password=\u{E000}redacted\u{E000}"],
            &["redis-server", "--port", "6380", "--appendonly", "yes"],
        ] {
            assert_eq!(exec_form_walk(args), Vec::<String>::new(), "{args:?}");
        }
    }

    #[test]
    fn exec_form_sequences_keep_structure() {
        for (input, expected) in [
            // Quoted block items keep their quotes; trailing comments stay.
            (
                "command:\n  - \"--password\"\n  - \"hunter2\"\n",
                "command:\n  - \"--password\"\n  - \"<redacted>\"\n",
            ),
            (
                "args:\n- --token\n# note\n\n- abc  # c\n- --api-key\n- 'k'  # c2\n- --port\n",
                "args:\n- --token\n# note\n\n- <redacted>  # c\n- --api-key\n- '<redacted>'  # c2\n- --port\n",
            ),
            // Block items with a glued value.
            (
                "  - mysqladmin\n  - -phunter2\n  - --api-key=abc\n",
                "  - mysqladmin\n  - -p<redacted>\n  - --api-key=<redacted>\n",
            ),
            // A key that is not flag-shaped keeps the elements after it (D3).
            (
                "env: [\"PASSWORD=x\", \"y\"]\n",
                "env: [\"PASSWORD=<redacted>\", \"y\"]\n",
            ),
            (
                "env:\n  - \"PASSWORD=x\"\n  - 'TOKEN=y z'\n",
                "env:\n  - \"PASSWORD=<redacted>\"\n  - 'TOKEN=<redacted>'\n",
            ),
            // CMD-SHELL strings are still handled by the command-line rules.
            (
                "test: [\"CMD-SHELL\", \"redis-cli -a hunter2 ping\"]\n",
                "test: [\"CMD-SHELL\", \"redis-cli -a <redacted> ping\"]\n",
            ),
            // Dockerfile exec form.
            (
                "CMD [\"redis-server\", \"--requirepass\", \"hunter2\"]\nHEALTHCHECK CMD [\"redis-cli\", \"-a\", \"x\"]\n",
                "CMD [\"redis-server\", \"--requirepass\", \"<redacted>\"]\nHEALTHCHECK CMD [\"redis-cli\", \"-a\", \"<redacted>\"]\n",
            ),
            // JSON, with a trailing comma, and a `- [ … ]` item.
            (
                "{\"args\": [\"--token\", \"t1\",], \"n\": 1}\n- [sshpass, -p, pw, ssh]\n",
                "{\"args\": [\"--token\", \"<redacted>\",], \"n\": 1}\n- [sshpass, -p, <redacted>, ssh]\n",
            ),
            // A blank line ends an unclosed sequence; the next one is still scanned.
            (
                "a: [x,\n\nb: [\"--token\", \"t\"]\n",
                "a: [x,\n\nb: [\"--token\", \"<redacted>\"]\n",
            ),
            // A JSON key closes its quote before the separator, so D3 leaves it alone.
            (
                "{\"a\": 1, \"password\": \"x\", \"b\": 2}\n",
                "{\"a\": 1, \"password\": \"<redacted>\", \"b\": 2}\n",
            ),
        ] {
            assert_eq!(text(input), expected, "{input:?}");
        }
        for kept in [
            // In nested sequences, each inner sequence is its own command, and a flag
            // at the end of one takes no value from the next.
            "matrix: [[a, --token], [b, c]]\n",
            "- [ ] write docs\n[section]\nhost: [::1]\n",
            // Mapping items are not arguments.
            "- name: --token\n- value\n",
        ] {
            assert_eq!(text(kept), kept);
        }
    }

    /// Asserts `text(input) == expected` for each pair.
    fn assert_redacts(cases: &[(&str, &str)]) {
        for (input, expected) in cases {
            assert_eq!(text(input), *expected, "{input:?}");
        }
    }

    #[test]
    fn line_rules_leave_plain_flow_values_to_exec_form() {
        assert_redacts(&[
            ("[sqlcmd, -P, pw]\n", "[sqlcmd, -P, <redacted>]\n"),
            (
                "command: [/opt/mssql-tools/bin/sqlcmd, -S, db, -U, sa, -P, pw]\n",
                "command: [/opt/mssql-tools/bin/sqlcmd, -S, db, -U, sa, -P, <redacted>]\n",
            ),
            ("[mysql, -ppw]\n", "[mysql, -p<redacted>]\n"),
            ("[mysql, -ppw, app]\n", "[mysql, -p<redacted>, app]\n"),
            // A separate MySQL `-p` prompts (D4); a flag with no value is left alone.
            ("[mysql, -p, appdb]\n", "[mysql, -p, appdb]\n"),
            ("[sqlcmd, -P]\n", "[sqlcmd, -P]\n"),
            ("[curl, -uadmin:pw]\n", "[curl, -uadmin:<redacted>]\n"),
            // Command lines keep the whole word, commas and brackets included.
            ("mysql -pab,cd app\n", "mysql -p<redacted> app\n"),
            ("mysql -pab]cd app\n", "mysql -p<redacted> app\n"),
            ("sqlcmd -S db -P pw,x\n", "sqlcmd -S db -P <redacted>\n"),
            (
                "curl -u bob:a,b] https://x\n",
                "curl -u bob:<redacted> https://x\n",
            ),
        ]);
    }

    #[test]
    fn exec_form_flow_comments() {
        assert_redacts(&[
            (
                "args: [\n  \"--token\", # auth\n  \"t\"\n]\n",
                "args: [\n  \"--token\", # auth\n  \"<redacted>\"\n]\n",
            ),
            (
                "args = [\n  \"--token\",  # c\n  \"t\",\n]\n",
                "args = [\n  \"--token\",  # c\n  \"<redacted>\",\n]\n",
            ),
            (
                "[\"--token\",\n  # comment\n  \"t\"]\n",
                "[\"--token\",\n  # comment\n  \"<redacted>\"]\n",
            ),
            (
                "args: [--token, # c\n  t]\n",
                "args: [--token, # c\n  <redacted>]\n",
            ),
            (
                "command: [ \"x\",\n  # c\n  \"--password\", \"hunter2\" ]\n",
                "command: [ \"x\",\n  # c\n  \"--password\", \"<redacted>\" ]\n",
            ),
            // A plain element ends at a comment.
            (
                "args: [--token # c\n  , t]\n",
                "args: [--token # c\n  , <redacted>]\n",
            ),
        ]);
    }

    #[test]
    fn key_assignment_value_is_the_rest_of_its_element() {
        assert_redacts(&[
            (
                "environment: [\"DB_PASSWORD=correct horse battery\", \"X=1\"]\n",
                "environment: [\"DB_PASSWORD=<redacted>\", \"X=1\"]\n",
            ),
            (
                "args: [\"-e\", \"PASSWORD=pw x\", \"y\"]\n",
                "args: [\"-e\", \"PASSWORD=<redacted>\", \"y\"]\n",
            ),
            (
                "env: [\"PASSWORD=x\" , \"y\"] # c\n",
                "env: [\"PASSWORD=<redacted>\" , \"y\"] # c\n",
            ),
        ]);
    }

    /// When a quote does not close the element right after the value, the value is
    /// measured as before elements were bounded (expected outputs are HEAD's).
    #[test]
    fn key_assignment_unclosed_element_falls_back() {
        assert_redacts(&[
            (
                "- \"PASSWORD=\"hunter2\"\"\n",
                "- \"PASSWORD=\"<redacted>\"\n",
            ),
            ("- \"TOKEN=\"abc\"\n", "- \"TOKEN=\"<redacted>\"\n"),
            ("[x, \"TOKEN=\"abc\"]\n", "[x, \"TOKEN=\"<redacted>\"]\n"),
            ("- \"TOKEN=ab\"cd\"\n", "- \"TOKEN=<redacted>\n"),
            (
                "args: [\"--token=ab\"cd\", \"y\"]\n",
                "args: [\"--token=<redacted> \"y\"]\n",
            ),
            ("- 'TOKEN=abc'xyz\n", "- 'TOKEN=<redacted>\n"),
            (
                "- \"TOKEN='''abc\ndef'''\"\n",
                "- \"TOKEN='''<redacted>'''\"\n",
            ),
        ]);
    }

    #[test]
    fn exec_form_mysql_family_glued_password() {
        assert_redacts(&[
            (
                "test: [\"CMD\", \"mariadb-admin\", \"ping\", \"-phunter2\"]\n",
                "test: [\"CMD\", \"mariadb-admin\", \"ping\", \"-p<redacted>\"]\n",
            ),
            (
                "[\"CMD\", \"mysqlcheck\", \"-phunter2\"]\n",
                "[\"CMD\", \"mysqlcheck\", \"-p<redacted>\"]\n",
            ),
        ]);
    }

    #[test]
    fn spaced_key_keeps_separator_and_comment() {
        assert_redacts(&[
            (
                "args: [\"--password\" , \"x\"]\n",
                "args: [\"--password\" , \"<redacted>\"]\n",
            ),
            (
                "command: [\"x\", \"--password\" , \"hunter2\" ]\n",
                "command: [\"x\", \"--password\" , \"<redacted>\" ]\n",
            ),
            (
                "- --password # c\n- pw\n",
                "- --password # c\n- <redacted>\n",
            ),
            (
                "- \"--password\" # c\n- pw\n",
                "- \"--password\" # c\n- <redacted>\n",
            ),
        ]);
    }

    #[test]
    fn exec_form_dash_prefixed_secret() {
        assert_redacts(&[
            (
                "command: [\"redis-server\", \"--requirepass\", \"-dashpw\"]\n",
                "command: [\"redis-server\", \"--requirepass\", \"<redacted>\"]\n",
            ),
            (
                "command:\n  - --requirepass\n  - -dashpw\n",
                "command:\n  - --requirepass\n  - <redacted>\n",
            ),
            (
                "- --password\n- '- hunter2'\n",
                "- --password\n- '<redacted>'\n",
            ),
        ]);
        for kept in [
            "command: [\"x\", \"--no-auth\", \"--port\", \"1\"]\n",
            "command: [\"x\", \"--token\", \"--\", \"y\"]\n",
            // A tool's password flag still takes no dashed value.
            "command: [\"redis-cli\", \"-a\", \"-h\"]\n",
        ] {
            assert_eq!(text(kept), kept);
        }
    }

    #[test]
    fn exec_form_properties_and_tabs() {
        assert_redacts(&[
            (
                "command: &a [\"x\", \"--password\", \"hunter2\"]\n",
                "command: &a [\"x\", \"--password\", \"<redacted>\"]\n",
            ),
            (
                "command: !!seq [\"x\", \"--password\", \"hunter2\"]\n",
                "command: !!seq [\"x\", \"--password\", \"<redacted>\"]\n",
            ),
            (
                "- --password\n- !!str hunter2\n",
                "- --password\n- !!str <redacted>\n",
            ),
            (
                "- --password\n- &p hunter2\n",
                "- --password\n- &p <redacted>\n",
            ),
            (
                "  -\t--password\n  -\thunter2\n",
                "  -\t--password\n  -\t<redacted>\n",
            ),
        ]);
    }

    #[test]
    fn exec_form_multi_line_block_values() {
        assert_redacts(&[
            (
                "- --password\n- |\n    hunter2\n    more\nnext: 1\n",
                "- --password\n- |\n    <redacted>\nnext: 1\n",
            ),
            (
                "- --password\n- >-\n    hunter2\n\n    more\n- --port\n",
                "- --password\n- >-\n    <redacted>\n- --port\n",
            ),
            (
                "- --password\n- \"hunter2\n    more\"\n- x\n",
                "- --password\n- \"<redacted>\"\n- x\n",
            ),
            (
                "- --password\n- hunter2\n    more\n- x\n",
                "- --password\n- <redacted>\n- x\n",
            ),
            // The body of a block scalar that is not a value stays.
            ("- --port\n- |\n    80\n", "- --port\n- |\n    80\n"),
        ]);
    }

    #[test]
    fn exec_form_nested_sequences() {
        assert_redacts(&[
            (
                "commands: [[\"redis-cli\", \"-a\", \"hunter2\"]]\n",
                "commands: [[\"redis-cli\", \"-a\", \"<redacted>\"]]\n",
            ),
            // Each inner sequence has its own tools.
            (
                "x: [[\"redis-cli\"], [\"y\", \"-a\", \"z\"], [redis-cli, -a, pw]]\n",
                "x: [[\"redis-cli\"], [\"y\", \"-a\", \"z\"], [redis-cli, -a, <redacted>]]\n",
            ),
            // The outer sequence continues after a nested one, with its own tools.
            (
                "cmd: [[\"x\"], \"--token\", \"t\"]\n",
                "cmd: [[\"x\"], \"--token\", \"<redacted>\"]\n",
            ),
            (
                "command: [x, [y], --password, hunter2]\n",
                "command: [x, [y], --password, <redacted>]\n",
            ),
            (
                "command: [x, {a: b}, --password, hunter2]\n",
                "command: [x, {a: b}, --password, <redacted>]\n",
            ),
            (
                "x: [redis-cli, [y], -a, pw, {k: [\"--token\", \"t\"]}]\n",
                "x: [redis-cli, [y], -a, <redacted>, {k: [\"--token\", \"<redacted>\"]}]\n",
            ),
            // Redactions before and after a nested sequence keep their order.
            (
                "[--password, a, [redis-cli, -a, b], --token, c]\n",
                "[--password, <redacted>, [redis-cli, -a, <redacted>], --token, <redacted>]\n",
            ),
            // A nested element takes the value slot of a flag before it.
            ("[--password, [x], y]\n", "[--password, [x], y]\n"),
            // An inner sequence keeps its redactions when its parent is given up.
            (
                "x: [[redis-cli, -a, pw], \"unclosed\n",
                "x: [[redis-cli, -a, <redacted>], \"unclosed\n",
            ),
        ]);
    }

    #[test]
    fn line_rules_need_a_flow_element_for_flow_handling() {
        // HEAD's output: outside a flow sequence, a value keeps its `,` and `]`.
        assert_redacts(&[
            (
                "echo mysql, then: mysql -u root -p,Pa55\n",
                "echo mysql, then: mysql -u root -p<redacted>\n",
            ),
            (
                "tools: mysql, then run mysql -p,secret app\n",
                "tools: mysql, then run mysql -p<redacted> app\n",
            ),
            ("mysql, -p,secret\n", "mysql, -p<redacted>\n"),
            (
                "mysql, -u root -p]secret\n",
                "mysql, -u root -p<redacted>\n",
            ),
            ("sqlcmd, -P ]secret\n", "sqlcmd, -P <redacted>\n"),
            (
                "curl, -u admin:]secret http://h\n",
                "curl, -u admin:<redacted> http://h\n",
            ),
            ("mysql -u root -ppw]\n", "mysql -u root -p<redacted>\n"),
            ("sqlcmd -P pw, -d x\n", "sqlcmd -P <redacted> -d x\n"),
            ("a, mysql -ppw]\n", "a, mysql -p<redacted>\n"),
            // A scanned sequence keeps its `]` (HEAD's output lost it).
            ("[mysql -ppw]\n", "[mysql -p<redacted>]\n"),
        ]);
    }

    #[test]
    fn closed_element_rule_needs_an_element() {
        assert_redacts(&[
            ("\"password\" ,hunter2\n", "\"password\" <redacted>\n"),
            (
                "[\"--password\" , \"x\"]\n",
                "[\"--password\" , \"<redacted>\"]\n",
            ),
            // Outside a scanned sequence, HEAD's spaced value stands.
            ("- \"--password\" ,\n", "- \"--password\" <redacted>\n"),
        ]);
    }

    /// Outside exec form the command-line rules are as before; expected outputs are
    /// HEAD's.
    #[test]
    fn line_rules_match_head_outside_exec_form() {
        assert_redacts(&[
            ("a,mysql, -p,hunter2\n", "a,mysql, -p<redacted>\n"),
            ("name, mysql, -p,hunter2\n", "name, mysql, -p<redacted>\n"),
            (
                "# deps: a, mysql, then mysql -u root -p,hunter2\n",
                "# deps: a, mysql, then mysql -u root -p<redacted>\n",
            ),
            (
                "echo [ok], mysql, -p,hunter2\n",
                "echo [ok], mysql, -p<redacted>\n",
            ),
            ("a,mysql, -p]hunter2\n", "a,mysql, -p<redacted>\n"),
            (
                "echo a, sqlcmd, -P ,hunter2 done\n",
                "echo a, sqlcmd, -P <redacted> done\n",
            ),
            ("x, sqlcmd, -P ]hunter2\n", "x, sqlcmd, -P <redacted>\n"),
            (
                "cmd: x, curl, -u a:,hunter2 http://h\n",
                "cmd: x, curl, -u a:<redacted> http://h\n",
            ),
            ("[x], curl, -u a:]hunter2\n", "[x], curl, -u a:<redacted>\n"),
            (
                "Install git, mysql, then run mysql -p,secret app\n",
                "Install git, mysql, then run mysql -p<redacted> app\n",
            ),
            (
                "git, sqlcmd, then sqlcmd -P ,pw\n",
                "git, sqlcmd, then sqlcmd -P <redacted>\n",
            ),
            (
                "Deps: git, curl, then curl -u bob:]pw https://x\n",
                "Deps: git, curl, then curl -u bob:<redacted> https://x\n",
            ),
        ]);
    }

    #[test]
    fn line_rules_keep_exec_form_separators() {
        assert_redacts(&[
            (
                "[\"sqlcmd\", -P, hunter2]\n",
                "[\"sqlcmd\", -P, <redacted>]\n",
            ),
            ("['sqlcmd', -P, hunter2]\n", "['sqlcmd', -P, <redacted>]\n"),
            (
                "[ \"sqlcmd\", -P, hunter2]\n",
                "[ \"sqlcmd\", -P, <redacted>]\n",
            ),
            (
                "command: [\"/opt/mssql-tools/bin/sqlcmd\", -S, db, -P, hunter2]\n",
                "command: [\"/opt/mssql-tools/bin/sqlcmd\", -S, db, -P, <redacted>]\n",
            ),
            ("[sqlcmd , -P, hunter2]\n", "[sqlcmd , -P, <redacted>]\n"),
            (
                "[sqlcmd.exe, -S, db, -P, hunter2]\n",
                "[sqlcmd.exe, -S, db, -P, <redacted>]\n",
            ),
            (
                "[\n  sqlcmd, -P, hunter2]\n",
                "[\n  sqlcmd, -P, <redacted>]\n",
            ),
            // A flag glued to its separator still keeps it.
            ("[sqlcmd, -P ,hunter2]\n", "[sqlcmd, -P ,<redacted>]\n"),
            // A `CMD-SHELL` string is one element, left to the command-line rules.
            (
                "test: [CMD-SHELL, redis-cli -a hunter2 ping]\n",
                "test: [CMD-SHELL, redis-cli -a <redacted> ping]\n",
            ),
        ]);
    }

    /// Expected outputs are HEAD's, or more redacted.
    #[test]
    fn spaced_separator_is_absent_only_before_a_redacted_element() {
        assert_redacts(&[
            ("[\"password\" ,hunter2]\n", "[\"password\" <redacted>\n"),
            ("- \"password\" ,hunter2\n", "- \"password\" <redacted>\n"),
            ("x,\"password\" ,hunter2\n", "x,\"password\" <redacted>\n"),
            (
                "- \"--password\" ,hunter2\n",
                "- \"--password\" <redacted>\n",
            ),
            (
                "x,\"--password\" ,hunter2\n",
                "x,\"--password\" <redacted>\n",
            ),
            ("x, \"password\" ]hunter2\n", "x, \"password\" <redacted>\n"),
            ("[\"password\" ]hunter2\n", "[\"password\" <redacted>\n"),
            ("[\"password\" }hunter2\n", "[\"password\" <redacted>\n"),
            ("[\"--password\" ]hunter2\n", "[\"--password\" <redacted>\n"),
            (
                "env: [\"DB_PASSWORD\" ,hunter2]\n",
                "env: [\"DB_PASSWORD\" <redacted>\n",
            ),
            ("[x, \"api_key\" ,abc123]\n", "[x, \"api_key\" ,abc123]\n"),
            ("- \"token\" ,t0k\n", "- \"token\" ,t0k\n"),
            // The value was redacted as an element first, so the separator stays.
            (
                "[\"--password\" , \"x\"]\n",
                "[\"--password\" , \"<redacted>\"]\n",
            ),
            (
                "[\"--password\" ,hunter2]\n",
                "[\"--password\" ,<redacted>]\n",
            ),
            ("[--password ,hunter2]\n", "[--password ,<redacted>]\n"),
        ]);
    }

    /// Exec-form runs after the command-line rules, so redacting a tool name as a value
    /// never hides it from them (the `-p`/`-P`/`-u` values are redacted as on HEAD).
    #[test]
    fn line_rules_see_tool_names_exec_form_redacts() {
        assert_redacts(&[
            (
                "[--password, mysql] -phunter2\n",
                "[--password, <redacted>] -p<redacted>\n",
            ),
            (
                "[--password, sqlcmd] -P hunter2\n",
                "[--password, <redacted>] -P <redacted>\n",
            ),
            (
                "[--password, curl] -u a:hunter2\n",
                "[--password, <redacted>] -u a:<redacted>\n",
            ),
            (
                "x: [--token=mysql] -phunter2\n",
                "x: [--token=<redacted>] -p<redacted>\n",
            ),
            (
                "[--password=mysql] -u root -phunter2\n",
                "[--password=<redacted>] -u root -p<redacted>\n",
            ),
            // A value past a scanned sequence's close is redacted whole.
            ("[mysql, -ppw]xyz\n", "[mysql, -p<redacted>]<redacted>\n"),
        ]);
    }

    /// A literal `<redacted>` in the input never counts as a rule's redaction, so these
    /// are redacted as on HEAD.
    #[test]
    fn literal_redacted_is_not_trusted() {
        assert_redacts(&[
            (
                "\"--password\" ,<redacted>,hunter2\n",
                "\"--password\" <redacted>\n",
            ),
            (
                "[\"--password\" ,<redacted>,hunter2]\n",
                "[\"--password\" <redacted>\n",
            ),
            (
                "--password ,<redacted>,hunter2\n",
                "--password <redacted>\n",
            ),
            (
                "\"--password\" ,<redacted>]hunter2\n",
                "\"--password\" <redacted>\n",
            ),
            (
                "\"--password\" ,\"<redacted>\"hunter2\n",
                "\"--password\" <redacted>\n",
            ),
            // The private marker cannot be forged: its delimiter is neutralised first, so
            // it is ordinary text to the guards ...
            (
                "\"--password\" ,\u{E000}redacted\u{E000},hunter2\n",
                "\"--password\" <redacted>\n",
            ),
            // ... and, in a sequence, an ordinary value of the flag before it.
            (
                "[\"--password\" ,\u{E000}redacted\u{E000},hunter2]\n",
                "[\"--password\" ,<redacted>,hunter2]\n",
            ),
            (
                "a \u{E000}redacted\u{E000} b\n",
                "a \u{FFFD}redacted\u{FFFD} b\n",
            ),
        ]);
        // Rules still leave their own earlier redactions alone.
        assert_redacts(&[
            (
                "GET /v1?token=a1&access_token=b2&page=2\n",
                "GET /v1?token=<redacted>&access_token=<redacted>&page=2\n",
            ),
            ("password=<redacted>\n", "password=<redacted>\n"),
        ]);
    }

    /// Keys glued after an earlier rule's redaction (`#eyJa.b.c'password:`) pass their
    /// value on, and no marker is ever split.
    #[test]
    fn glued_key_after_an_earlier_redaction() {
        assert_redacts(&[
            (
                "x --password --password #eyJa.b.c'password: zq3\n",
                "x --password <redacted> <redacted> <redacted>\n",
            ),
            (
                "x --password --password #eyJa.b.c-'password: zq1\n",
                "x --password <redacted> <redacted> <redacted>\n",
            ),
        ]);
    }

    #[test]
    fn curl_scan_is_linear_and_escape_aware() {
        let n = 256 * 1024 / 4;
        let start = std::time::Instant::now();
        // Every quote escaped: no quote opens, and the flag at the end is found.
        let escaped = format!("curl x{} -u a:zq\n", " \\\"x".repeat(n));
        assert_eq!(
            text(&escaped),
            format!("curl x{} -u a:<redacted>\n", " \\\"x".repeat(n))
        );
        // One quote that never closes: later quotes of its kind on the line are lone.
        let unclosed = format!("curl x \"{} -u a:zq\n", "\\\"x".repeat(n));
        assert_eq!(
            text(&unclosed),
            format!("curl x \"{} -u a:<redacted>\n", "\\\"x".repeat(n))
        );
        let lone = format!("curl x{} -u a:zq\n", " 'x".repeat(n));
        assert_eq!(
            text(&lone),
            format!("curl x{} -u a:<redacted>\n", " 'x".repeat(n))
        );
        let doubled = format!("curl x '{} -u a:zq\n", "x''".repeat(n));
        assert_eq!(
            text(&doubled),
            format!("curl x '{} -u a:<redacted>\n", "x''".repeat(n))
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        // Outside quotes, `\"` is a literal quote.
        assert_redacts(&[
            (
                "curl -X POST -d {\\\"k\\\":1} -u admin:zq1 -H \"X: y\" http://h\n",
                "curl -X POST -d {\\\"k\\\":1} -u admin:<redacted> -H \"X: y\" http://h\n",
            ),
            (
                "curl -d \\\"a -u admin:zq2 -H \"b\" x\n",
                "curl -d \\\"a -u admin:<redacted> -H \"b\" x\n",
            ),
        ]);
    }

    /// A command that ends inside a redacted password word (`pw|curl`) lets the search
    /// for `curl` resume there.
    /// A `curl` word anywhere starts a curl command, even in quoted text; a flag at a word
    /// start inside one counts whatever the quoting. Only an unquoted `;`, `&`, `|` or the
    /// line end ends the command.
    #[test]
    fn curl_single_pass_finds_every_flag() {
        assert_redacts(&[
            (
                "Use curl \"for example: curl -u admin:zq2 http://h\" to test\n",
                "Use curl \"for example: curl -u admin:<redacted> http://h\" to test\n",
            ),
            (
                "curl -X POST http://h -d '{\"cmd\": \"curl -u admin:zq1 http://x\"}'\n",
                "curl -X POST http://h -d '{\"cmd\": \"curl -u admin:<redacted> http://x\"}'\n",
            ),
            (
                "curl -H \"X: curl -u a:zq3\" http://h\n",
                "curl -H \"X: curl -u a:<redacted>\" http://h\n",
            ),
            (
                "curl -u a:zq5\" #curl -u b:zq6 x\"\"\n",
                "curl -u a:<redacted>\" #curl -u b:<redacted> x\"\"\n",
            ),
            (
                "curl -d it's -u a:b -d 'x -u c:d' -u e:f\n",
                "curl -d it's -u a:<redacted> -d 'x -u c:<redacted>' -u e:<redacted>\n",
            ),
            (
                "curl -d it's -u a:b -d 'x' z\n",
                "curl -d it's -u a:<redacted> -d 'x' z\n",
            ),
            // Command bounds.
            (
                "curl -u a:b; echo -u c:d\n",
                "curl -u a:<redacted> echo -u c:d\n",
            ),
            ("curl x; echo -u a:b\n", "curl x; echo -u a:b\n"),
            (
                "curl -u a:pw|grep -u c:d\n",
                "curl -u a:<redacted> -u c:d\n",
            ),
            (
                "curl -d \"x;y\" -u b:zq\n",
                "curl -d \"x;y\" -u b:<redacted>\n",
            ),
        ]);
    }

    #[test]
    fn curl_single_pass_is_linear() {
        let n = 256 * 1024;
        let start = std::time::Instant::now();
        let flags = format!("curl {}\n", "-u=".repeat(n / 3));
        assert_eq!(text(&flags), flags);
        let escaped = format!("curl {}\n", "-u a:\\\" ".repeat(n / 8));
        assert_eq!(
            text(&escaped),
            format!("curl {}\n", "-u a:<redacted>\" ".repeat(n / 8))
        );
        let mixed = format!("curl {}\n", "--user a:\\\"''a:b ".repeat(n / 16));
        assert!(!text(&mixed).contains(MARK_CHAR));
        let odd = format!("curl {}\n", "\\\"-u\u{e9}  --user :".repeat(n / 16));
        assert!(!text(&odd).contains(MARK_CHAR));
        // A quoted password's glued tail chaining quoted segments over the line is looked
        // at once, even when a key in it ends the hidden part early.
        let chained = format!("curl {}\n", "token=\u{e9}curla:\"p'curl\"-u".repeat(n / 20));
        assert!(!text(&chained).contains(MARK_CHAR));
        let keyed = format!("curl {}\n", "password: \"-u\"".repeat(n / 15));
        assert!(!text(&keyed).contains(MARK_CHAR));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    /// A `|` glued to a quoted value is part of it outside a command line (as on HEAD);
    /// on curl's command line it ends the word.
    #[test]
    fn pipe_after_a_quoted_value() {
        assert_redacts(&[
            ("TOKEN=\"ab\"|zq28\n", "TOKEN=\"<redacted>\"\n"),
            ("TOKEN='ab'|zq29\n", "TOKEN='<redacted>'\n"),
            (
                "x password=\"ab\"|zq30 y\n",
                "x password=\"<redacted>\" y\n",
            ),
            (
                "x --password \"ab\"|zq31 y\n",
                "x --password \"<redacted>\" y\n",
            ),
            ("password: \"ab\"|zq32\n", "password: \"<redacted>\"\n"),
            (
                "export SECRET=\"a\"|\"zq33\"\n",
                "export SECRET=\"<redacted>\"\n",
            ),
            (
                "x token=\"a b\"|tr -d x zq34\n",
                "x token=\"<redacted>\" -d x zq34\n",
            ),
            ("KEY=\"ab\"zq35|zq36\n", "KEY=\"<redacted>\"\n"),
            (
                "curl -u \"a:pw\"|curl -u b:x\n",
                "curl -u \"a:<redacted>\"|curl -u b:<redacted>\n",
            ),
        ]);
    }

    /// A `\` at the end of a line continues the curl command on the next one.
    #[test]
    fn curl_continuation_lines() {
        assert_redacts(&[
            (
                "curl -X POST \\\n  -u admin:hunter2 \\\n  https://h\n",
                "curl -X POST \\\n  -u admin:<redacted> \\\n  https://h\n",
            ),
            (
                "curl -X POST \\\r\n  -u admin:hunter2 \\\r\n  https://h\r\n",
                "curl -X POST \\\r\n  -u admin:<redacted> \\\r\n  https://h\r\n",
            ),
            (
                "curl -X POST \\\n-u admin:hunter2\n",
                "curl -X POST \\\n-u admin:<redacted>\n",
            ),
            // Without the `\`, the line break ends the command.
            ("curl x\n -u a:b\n", "curl x\n -u a:b\n"),
        ]);
    }

    /// A curl password's hidden tail stops only at a key that the key rule, run with the
    /// same `spaced` setting, redacts; so nothing of it shows in any entry point.
    #[test]
    fn curl_tail_follows_the_runs_key_rule() {
        let fuzz =
            "' #  #curl -u 'b:sqlcmd<:'\"-p \\'curl -u 'b:MYTOKEN zq36438-P}curl -u 'b:\r\n]\n";
        // (input, text, credential_parts, credential_parts_inline); `value` under a key
        // that names no secret is `credential_parts`.
        for (input, text_out, parts, inline) in [
            (
                "curl -u a:\"x\"\"-p MYTOKEN zq3\n",
                "curl -u a:\"<redacted>\"<redacted> MYTOKEN <redacted>\n",
                "curl -u a:\"<redacted>\"<redacted>\n",
                "curl -u a:\"<redacted>\"<redacted>",
            ),
            (
                "curl -u a:\"x\"y\" MYTOKEN zq6\n",
                "curl -u a:\"<redacted>\"<redacted> MYTOKEN <redacted>\n",
                "curl -u a:\"<redacted>\"<redacted>\n",
                "curl -u a:\"<redacted>\"<redacted>",
            ),
            (
                "curl -u a:\"x\"y\" SECRET_KEY zq9 q\n",
                "curl -u a:\"<redacted>\"<redacted> SECRET_KEY <redacted> q\n",
                "curl -u a:\"<redacted>\"<redacted>\n",
                "curl -u a:\"<redacted>\"<redacted>",
            ),
            (
                "curl -u a:\"pw\"\"x MYTOKEN zq10\" http://h\n",
                "curl -u a:\"<redacted>\"<redacted> MYTOKEN <redacted> http://h\n",
                "curl -u a:\"<redacted>\"<redacted> http://h\n",
                "curl -u a:\"<redacted>\"<redacted> http://h ",
            ),
            (
                fuzz,
                "' #  #curl -u 'b:<redacted>'<redacted>:MYTOKEN <redacted> -u 'b:\r\n]\n",
                "' #  #curl -u 'b:<redacted>'<redacted>\r\n]\n",
                "' #  #curl -u 'b:<redacted>'<redacted>",
            ),
        ] {
            assert_eq!(text(input), text_out, "text {input:?}");
            assert_eq!(credential_parts(input), parts, "parts {input:?}");
            assert_eq!(credential_parts_inline(input), inline, "inline {input:?}");
            assert_eq!(value("PATH", input), parts, "value {input:?}");
        }
    }

    #[test]
    fn curl_glued_after_a_password() {
        assert_redacts(&[
            (
                "curl -u a:pw|curl -u b:zq4\n",
                "curl -u a:<redacted> -u b:<redacted>\n",
            ),
            (
                "curl -u a:pw;curl -u b:zq5\n",
                "curl -u a:<redacted> -u b:<redacted>\n",
            ),
            (
                "curl -u a:pw&&curl -u b:zq6\n",
                "curl -u a:<redacted> -u b:<redacted>\n",
            ),
        ]);
    }

    /// A key-like next word whose separator runs past the bound is redacted with the word
    /// after the separator.
    #[test]
    fn long_separator_hides_both_words() {
        let pad = " ".repeat(70);
        assert_eq!(
            text(&format!("x token=a;password: KEY{pad}value\n")),
            format!("x token=<redacted> <redacted>{pad}<redacted>\n")
        );
    }

    #[test]
    fn output_never_holds_the_internal_marker() {
        let inputs = LEAK_FIXTURES
            .iter()
            .map(|(_, input, _)| input.to_string())
            .chain(PREFIXED_TOKENS.iter().map(|t| format!("x {t} y\n")))
            .chain(
                [
                    "command: [\"redis-server\", \"--requirepass\", \"x\"]\n",
                    "- --password\n- |\n  x\n",
                    "[mysql, -ppw]\n",
                    "a \u{E000}redacted\u{E000} b\n",
                    "x --password --password #eyJa.b.c'password: zq3\n",
                    "x --password --password #eyJa.b.c-'password: zq1\n",
                    "curl -u a:\"q\"eyJa.b.c\"--password zq7\n",
                ]
                .map(str::to_string),
            );
        for input in inputs {
            for out in [
                text(&input),
                credential_parts(&input),
                credential_parts_inline(&input),
                value("TOKEN", &input),
            ] {
                assert!(!out.contains(MARK_CHAR), "{input:?} -> {out:?}");
            }
        }
    }

    /// A comment inside a scanned sequence is not structure, so a command line in it is
    /// redacted as on HEAD.
    #[test]
    fn line_rules_redact_in_flow_comments() {
        assert_redacts(&[
            (
                "command: [\n  \"mysqld\",  # connect with: mysql -u root -phunter2\n  \"--bind-address=0.0.0.0\",\n]\n",
                "command: [\n  \"mysqld\",  # connect with: mysql -u root -p<redacted>\n  \"--bind-address=0.0.0.0\",\n]\n",
            ),
            (
                "[x, # sqlcmd -S db -P hunter2\n  y]\n",
                "[x, # sqlcmd -S db -P <redacted>\n  y]\n",
            ),
            (
                "[x, # curl -u admin:hunter2 http://h\n  y]\n",
                "[x, # curl -u admin:<redacted> http://h\n  y]\n",
            ),
            (
                "command: [sqlcmd, -S, db, -P #Pa55word\n  , -Q, x]\n",
                "command: [sqlcmd, -S, db, -P<redacted>\n  , -Q, x]\n",
            ),
        ]);
    }

    #[test]
    fn only_structure_bytes_of_a_gap_stay_visible() {
        for b in *b"[]{},\"' \t\r\n" {
            assert!(is_structure_byte(b), "{:?}", char::from(b));
        }
        // A `:` may be part of a password (`mysql -pab:cd` in a flow mapping).
        for b in *b":#aZ09-_./=<>!@$%&*()+;|?~`" {
            assert!(!is_structure_byte(b), "{:?}", char::from(b));
        }
        // A comment's text is never a gap, so it is redacted like any value.
        let scan = flow_scan("[x, # c -pq\n  y]\n");
        assert_eq!(scan.gaps, vec![0..1, 2..4, 11..14, 15..16]);
    }

    #[test]
    fn line_rule_values_in_flow_mappings_hide_colons() {
        assert_redacts(&[
            ("[{a: mysql -pab:cd}]\n", "[{a: mysql -p<redacted>}]\n"),
            (
                "[{a: mysql -pab:cd:ef:gh}]\n",
                "[{a: mysql -p<redacted>}]\n",
            ),
            ("[{a: mysql -p:x}]\n", "[{a: mysql -p<redacted>}]\n"),
            ("[{a: mysql -p:}]\n", "[{a: mysql -p<redacted>}]\n"),
            ("[{a: sqlcmd -P ab:cd}]\n", "[{a: sqlcmd -P <redacted>}]\n"),
            ("[{a: mysql -p\"a:b\"}]\n", "[{a: mysql -p<redacted>}]\n"),
            // A `:` before a blank is still the mapping indicator.
            ("[{cmd: x, b: y}]\n", "[{cmd: x, b: y}]\n"),
        ]);
    }

    /// A plain word value is redacted whole, as on HEAD. When a key glued to its end
    /// (`;password:`, `#--requirepass`) takes the next word as its value, that word is
    /// redacted too.
    #[test]
    fn key_glued_to_a_value_hides_its_next_word() {
        assert_redacts(&[
            (
                "x token=abc;password: zq1 y\n",
                "x token=<redacted> <redacted> y\n",
            ),
            (
                "x token=<redacted>;password: zq1 y\n",
                "x token=<redacted> <redacted> y\n",
            ),
            (
                "x token=abc#--requirepass zq1\n",
                "x token=<redacted> <redacted>\n",
            ),
            // At line start the value is the rest of the line anyway.
            ("token=x]@hAuthorization: zq12398 y\n", "token=<redacted>\n"),
            (
                "x token=x]@hAuthorization: zq12398 y\n",
                "x token=<redacted> <redacted> y\n",
            ),
            (
                "x token=a;secret=b;password: zq1 y\n",
                "x token=<redacted> <redacted> y\n",
            ),
            // The whole word is redacted; nothing of it is shown again.
            (
                "docker run -e DB_PASSWORD=My@Password1 app\n",
                "docker run -e DB_PASSWORD=<redacted> app\n",
            ),
            ("x --password My!Token9 y\n", "x --password <redacted> y\n"),
            ("x api_key=Ab$Cookie7 y\n", "x api_key=<redacted> y\n"),
            (
                "x password=abc/tokenXYZ next\n",
                "x password=<redacted> next\n",
            ),
            ("x password=Xy9!pass yes\n", "x password=<redacted> yes\n"),
            (
                "x DB_PASSWORD=s3cr3t:apiKey value\n",
                "x DB_PASSWORD=<redacted> value\n",
            ),
            (
                "x password=hunter2#token here\n",
                "x password=<redacted> here\n",
            ),
            ("x password=p;sessionId 42\n", "x password=<redacted> 42\n"),
            ("x password=p@cookie jar\n", "x password=<redacted> jar\n"),
            ("x password=a,secret more\n", "x password=<redacted> more\n"),
            ("x password=p,auth=\n", "x password=<redacted>\n"),
            ("x token=s3cr3t,PASSWORD:1234 y\n", "x token=<redacted> y\n"),
            ("x token=a;password:\n", "x token=<redacted>\n"),
            ("x token=ab;cookie y\n", "x token=<redacted> y\n"),
            // A next word that is an assignment of its own is redacted as one.
            (
                "x token=a;password api_key: zq1\n",
                "x token=<redacted> api_key: <redacted>\n",
            ),
            (
                "env TOKEN=abc;Password secret: zq2 run\n",
                "env TOKEN=<redacted> secret: <redacted> run\n",
            ),
            (
                "x token=abc\"--password client_secret: zq3\n",
                "x token=<redacted> client_secret: <redacted>\n",
            ),
            (
                "x --password zq5zq6mongosh?token=zq7\"--password \n",
                "x --password <redacted> \n",
            ),
        ]);
        // Mid-line, so each value is a word (a line-start key takes the rest of the line).
        let many = format!("x {}\n", "token=a;password: b ".repeat(10_000));
        let start = std::time::Instant::now();
        assert_eq!(
            text(&many),
            format!("x {}\n", "token=<redacted> <redacted> ".repeat(10_000))
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn exec_form_flag_and_value_in_one_element() {
        assert_redacts(&[
            (
                "[\"sqlcmd\", \"-PPassw0rd\"]\n",
                "[\"sqlcmd\", \"-P<redacted>\"]\n",
            ),
            // An element is one argument: `-P…` holds a glued password, blanks included.
            ("[sqlcmd, \"-P pw\"]\n", "[sqlcmd, \"-P<redacted>\"]\n"),
            (
                "[\"sqlcmd\",\n \"-P pw\"]\n",
                "[\"sqlcmd\",\n \"-P<redacted>\"]\n",
            ),
            ("[redis-cli,\n -a pw]\n", "[redis-cli,\n -a <redacted>]\n"),
            (
                "[docker, login,\n -p pw]\n",
                "[docker, login,\n -p <redacted>]\n",
            ),
            ("- sqlcmd\n- -P pw\n", "- sqlcmd\n- -P<redacted>\n"),
            (
                "[\"x\", \"--requirepass  pw\"]\n",
                "[\"x\", \"--requirepass  <redacted>\"]\n",
            ),
            (
                "[curl, \"-u admin:pw\"]\n",
                "[curl, \"-u admin:<redacted>\"]\n",
            ),
        ]);
    }

    /// In exec form an element is one argument, so `-p<anything>` in one element is a
    /// glued password; only a bare `-p` element prompts (D4).
    /// Each redacted word that ends in a glued key passes the redaction on to the next
    /// word, unless that word is an assignment the key rule takes as one.
    #[test]
    fn key_glued_values_chain() {
        assert_redacts(&[
            (
                "x token=a;password: b;secret: zq5\n",
                "x token=<redacted> <redacted> <redacted>\n",
            ),
            (
                "x token=a;password: ;--password zq4\n",
                "x token=<redacted> ;--password <redacted>\n",
            ),
            (
                "x --token a;password: b;secret: zq46\n",
                "x --token <redacted> <redacted> <redacted>\n",
            ),
            (
                "x token=a;password: b;c;secret: zq6\n",
                "x token=<redacted> <redacted> <redacted>\n",
            ),
        ]);
        let long = format!("x token=a;password: {}zq\n", "b;secret: ".repeat(10_000));
        let start = std::time::Instant::now();
        assert_eq!(
            text(&long),
            format!(
                "x token=<redacted> {}<redacted>\n",
                "<redacted> ".repeat(10_000)
            )
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    /// A next word counts as an assignment only when the key rule would take it as one;
    /// otherwise it is the glued key's value.
    #[test]
    fn glued_key_next_word_needs_an_accepted_assignment() {
        assert_redacts(&[
            (
                "x token=abc;password: secretsauce more\n",
                "x token=<redacted> <redacted> more\n",
            ),
            (
                "x token=abc;password: MyToken1 more\n",
                "x token=<redacted> <redacted> more\n",
            ),
            (
                "x token=abc;password: apiKey more\n",
                "x token=<redacted> <redacted> more\n",
            ),
            (
                "x token=a;--password MySecretPw y\n",
                "x token=<redacted> <redacted> y\n",
            ),
            (
                "x token=a;password: api_key y\n",
                "x token=<redacted> <redacted> y\n",
            ),
            // The next word is measured as a value, quotes included.
            (
                "x token=abc;password: \"hunter 2\" y\n",
                "x token=<redacted> \"<redacted>\" y\n",
            ),
        ]);
    }

    #[test]
    fn curl_password_quotes_tails_and_every_user() {
        assert_redacts(&[
            // The closing quote matches the opening one; `\"` is escaped.
            (
                "curl -u a:\"it's secret\" x\n",
                "curl -u a:\"<redacted>\" x\n",
            ),
            (
                "curl -u bob:\"p\\\"w\" x\n",
                "curl -u bob:\"<redacted>\" x\n",
            ),
            // A glued tail is marked, up to a key that is an assignment of its own.
            (
                "curl -u bob:\"pw\"tail more\n",
                "curl -u bob:\"<redacted>\"<redacted> more\n",
            ),
            (
                "curl -u a:\"pw\"--password zq1 x\n",
                "curl -u a:\"<redacted>\"--password <redacted> x\n",
            ),
            // An empty password is kept; the user's closing quote opens nothing.
            ("curl -u \"bob:\" -H x\n", "curl -u \"bob:\" -H x\n"),
            // Every `-u` of the command (curl uses the last).
            (
                "curl --user \"a:pw\" -u b:\"q r\" x\n",
                "curl --user \"a:<redacted>\" -u b:\"<redacted>\" x\n",
            ),
            (
                "curl -u a:x -u b:y\n",
                "curl -u a:<redacted> -u b:<redacted>\n",
            ),
            // An unquoted password runs to a blank, `;` included (as on HEAD); the `;`
            // in it still ends the command, so `echo`'s `-u` stays ...
            (
                "curl -u a:x; echo -u b:y\n",
                "curl -u a:<redacted> echo -u b:y\n",
            ),
            // ... as does a separated `;`.
            (
                "curl -u a:x ; echo -u b:y\n",
                "curl -u a:<redacted> ; echo -u b:y\n",
            ),
            // ... also when curl had none (HEAD took the first `-u` on the line).
            ("curl x; echo -u b:y\n", "curl x; echo -u b:y\n"),
        ]);
        let many = format!("curl {}\n", "-u a:b ".repeat(20_000));
        let start = std::time::Instant::now();
        assert_eq!(
            text(&many),
            format!("curl {}\n", "-u a:<redacted> ".repeat(20_000))
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn curl_tail_never_splits_a_marker() {
        let out = text("curl -u a:\"q\"eyJa.b.c\"--password zq7\n");
        assert!(!out.contains(MARK_CHAR) && !out.contains("zq7"), "{out:?}");
        assert_eq!(
            out,
            "curl -u a:\"<redacted>\"<redacted>\"--password <redacted>\n"
        );
    }

    #[test]
    fn curl_user_flag_without_argument() {
        assert_redacts(&[
            ("curl -u -u c:zq4 x\n", "curl -u -u c:<redacted> x\n"),
            ("curl -u  -u c:zq5 x\n", "curl -u  -u c:<redacted> x\n"),
            (
                "curl --user -u c:zq6 x\n",
                "curl --user -u c:<redacted> x\n",
            ),
        ]);
    }

    /// An unquoted password is the whole shell word up to a blank or quote, as on HEAD; a
    /// `;`, `&` or `|` in it ends the command, and a key glued to its end takes the next
    /// word.
    #[test]
    fn curl_unquoted_password_is_the_whole_word() {
        assert_redacts(&[
            (
                "curl -u admin:Sup3r(Secret)zq4 http://h\n",
                "curl -u admin:<redacted> http://h\n",
            ),
            (
                "Run: curl -u admin:hunter2(zq9) to test\n",
                "Run: curl -u admin:<redacted> to test\n",
            ),
            (
                "curl -u admin:pa&zq6 http://h\n",
                "curl -u admin:<redacted> http://h\n",
            ),
            (
                "curl -u admin:Pa$$w0rd<zq5> http://h\n",
                "curl -u admin:<redacted> http://h\n",
            ),
            (
                "curl -u admin:(zq7) http://h\n",
                "curl -u admin:<redacted> http://h\n",
            ),
            (
                "curl -u admin:<zq8> http://h\n",
                "curl -u admin:<redacted> http://h\n",
            ),
            (
                "curl -v -u a:b -u c:Secret;--password zq2 x\n",
                "curl -v -u a:<redacted> -u c:<redacted> <redacted> x\n",
            ),
            (
                "curl -u c:Secret;--password zq1 x\n",
                "curl -u c:<redacted> <redacted> x\n",
            ),
            (
                "curl -u a:b; echo -u c:d\n",
                "curl -u a:<redacted> echo -u c:d\n",
            ),
            ("curl -u a:b;echo -u c:d\n", "curl -u a:<redacted> -u c:d\n"),
            (
                "curl -u a:b>out -u c:d\n",
                "curl -u a:<redacted> -u c:<redacted>\n",
            ),
            // `,` is not a metacharacter: a key glued to the end takes the next word.
            (
                "curl -u c:Secret,api_key: zq3\n",
                "curl -u c:<redacted> <redacted>\n",
            ),
            // Flow sequences keep their structure.
            ("[curl, -uadmin:pw]\n", "[curl, -uadmin:<redacted>]\n"),
            ("[curl, -u, a:b]\n", "[curl, -u, a:<redacted>]\n"),
            (
                "[\"curl\", \"-u\", \"admin:pw\", \"https://x\"]\n",
                "[\"curl\", \"-u\", \"admin:<redacted>\", \"https://x\"]\n",
            ),
        ]);
    }

    #[test]
    fn curl_first_user_flag_uses_the_command_scan() {
        assert_redacts(&[
            // A flag in quoted data counts too (it only hides more).
            (
                "curl -d \"x -u c:d\" -u a:b\n",
                "curl -d \"x -u c:<redacted>\" -u a:<redacted>\n",
            ),
            ("curl x; echo -u a:b\n", "curl x; echo -u a:b\n"),
            // An unclosed quote is a lone character; `--user-agent` is another option.
            (
                "curl --user-agent Bob's -u admin:zq1 http://h\n",
                "curl --user-agent Bob's -u admin:<redacted> http://h\n",
            ),
            (
                "curl -u admin -d it's -u admin:zq2\n",
                "curl -u admin -d it's -u admin:<redacted>\n",
            ),
            (
                "curl -A x --user-agent=v -d don't -u admin:zq3\n",
                "curl -A x --user-agent=v -d don't -u admin:<redacted>\n",
            ),
            ("curl --user-agent=M:5 x\n", "curl --user-agent=M:5 x\n"),
        ]);
    }

    /// A word value is always redacted whole. When it is itself a key whose value comes
    /// after a blank (`--token`, `api_key:`), that next word is redacted too; a value
    /// inside the word (`api_key=abc`) is hidden with it.
    #[test]
    fn value_word_is_redacted_whole_and_passes_on_its_key() {
        let pad = " ".repeat(70);
        assert_redacts(&[
            (
                "x password=SUPERSECRETPWD user=bob\n",
                "x password=<redacted> user=bob\n",
            ),
            (
                "connecting with password=MYSECRETPASS next\n",
                "connecting with password=<redacted> next\n",
            ),
            (
                "x DB_PASSWORD=ADMIN_PASS1 extra\n",
                "x DB_PASSWORD=<redacted> extra\n",
            ),
            ("x password=TOKEN more\n", "x password=<redacted> more\n"),
            ("x password=KEY more\n", "x password=<redacted> more\n"),
            (
                "x password: MYAPIKEY other\n",
                "x password: <redacted> other\n",
            ),
            (
                "x --password MYAPIKEY other\n",
                "x --password <redacted> other\n",
            ),
            ("x token=PASSWORD hunter2\n", "x token=<redacted> hunter2\n"),
            (
                "x password=--token rest\n",
                "x password=<redacted> <redacted>\n",
            ),
            (
                "x password: --token x\n",
                "x password: <redacted> <redacted>\n",
            ),
            (
                "x password=\"KEY more\" y\n",
                "x password=\"<redacted>\" y\n",
            ),
            (
                "x password=api_key=abc rest\n",
                "x password=<redacted> rest\n",
            ),
            ("x password: -Dpw=abc\n", "x password: <redacted>\n"),
            ("x secret=password:hunter2 y\n", "x secret=<redacted> y\n"),
            ("x secret=TOKEN:abc y\n", "x secret=<redacted> y\n"),
            (
                "docker run -e TOKEN=ABCDEFKEY12 image\n",
                "docker run -e TOKEN=<redacted> image\n",
            ),
            (
                "x --password SECRET_KEY_123 --port 5432\n",
                "x --password <redacted> --port 5432\n",
            ),
            (
                ";token=Secretzq1027zq1028zq1029--password=\n",
                ";token=<redacted>\n",
            ),
            ("--password=zq496--api-key=\n", "--password=<redacted>\n"),
            (
                ";token=zq222zq223sqlcmdpassword: --passcommand:\n",
                ";token=<redacted> <redacted>\n",
            ),
            // The value that was an assignment of its own is hidden too (round 11 left
            // it visible).
            (
                "y;password: --token zq1\n",
                "y;password: <redacted> <redacted>\n",
            ),
            (
                "x;password: --requirepass zq5\n",
                "x;password: <redacted> <redacted>\n",
            ),
            (
                "curl -u a:x;password: --token zq4\n",
                "curl -u a:<redacted> --token <redacted>\n",
            ),
            (
                "curl -u a:x;password: api_key: zq6\n",
                "curl -u a:<redacted> api_key: <redacted>\n",
            ),
        ]);
        // A long run of blanks never makes a value look like a key; a comment is no value.
        for (input, expected) in [
            (
                format!("x password=KEY{pad}# c\n"),
                format!("x password=<redacted>{pad}# c\n"),
            ),
            (
                format!("x token=a;password: KEY{pad}# c\n"),
                format!("x token=<redacted> <redacted>{pad}# c\n"),
            ),
            (
                format!("x password: --token{pad}# c\n"),
                format!("x password: <redacted>{pad}# c\n"),
            ),
        ] {
            assert_eq!(text(&input), expected, "{input:?}");
        }
    }

    #[test]
    fn curl_command_bounds_skip_quoted_text() {
        assert_redacts(&[
            (
                "curl -u a:pw -d \"x|y\" -u b:zq94\n",
                "curl -u a:<redacted> -d \"x|y\" -u b:<redacted>\n",
            ),
            (
                "curl -u a:pw -d \"x;y\" -u b:zq95\n",
                "curl -u a:<redacted> -d \"x;y\" -u b:<redacted>\n",
            ),
            (
                "curl -u a:pw -d 'x&y' -u b:zq96\n",
                "curl -u a:<redacted> -d 'x&y' -u b:<redacted>\n",
            ),
        ]);
    }

    #[test]
    fn exec_form_glued_password_with_blanks() {
        assert_redacts(&[
            ("[mysql, \"-pmy pass\"]\n", "[mysql, \"-p<redacted>\"]\n"),
            (
                "[\"sshpass\", \"-pmy pw\", ssh, h]\n",
                "[\"sshpass\", \"-p<redacted>\", ssh, h]\n",
            ),
            ("[sqlcmd, \"-PMy Pass\"]\n", "[sqlcmd, \"-P<redacted>\"]\n"),
            (
                "[sqlcmd, \"-Plain text\"]\n",
                "[sqlcmd, \"-P<redacted>\"]\n",
            ),
            ("[curl, \"-ubob:p w\"]\n", "[curl, \"-ubob:<redacted>\"]\n"),
            (
                "[\"mysql\", \"-p appdb\"]\n",
                "[\"mysql\", \"-p<redacted>\"]\n",
            ),
            ("[mysql, -p pw]\n", "[mysql, -p<redacted>]\n"),
            (
                "[\"mysql\", \"-p\\tpw\"]\n",
                "[\"mysql\", \"-p<redacted>\"]\n",
            ),
            (
                "[\"mariadb\", \"-p\", \"-p pw\"]\n",
                "[\"mariadb\", \"-p\", \"-p<redacted>\"]\n",
            ),
            // A bare `-p` prompts; the database stays.
            (
                "[\"mysql\", \"-p\", \"appdb\"]\n",
                "[\"mysql\", \"-p\", \"appdb\"]\n",
            ),
        ]);
    }

    /// A flag element with only blanks after it takes the next element, as a bare flag.
    #[test]
    fn exec_form_flag_with_trailing_blanks() {
        assert_redacts(&[
            (
                "[x, \"--password \", \"hunter2\"]\n",
                "[x, \"--password \", \"<redacted>\"]\n",
            ),
            (
                "[\"sqlcmd\", \"-P \", \"pw\"]\n",
                "[\"sqlcmd\", \"-P \", \"<redacted>\"]\n",
            ),
            (
                "[\"redis-cli\", \"-a \", \"pw\"]\n",
                "[\"redis-cli\", \"-a \", \"<redacted>\"]\n",
            ),
            (
                "[\"curl\", \"-u \", \"a:pw\"]\n",
                "[\"curl\", \"-u \", \"a:<redacted>\"]\n",
            ),
            (
                "- \"--password \"\n- hunter2\n",
                "- \"--password \"\n- <redacted>\n",
            ),
            // Outside a scanned sequence the value is measured as on HEAD.
            (
                "- \"--password \" hunter2\n",
                "- \"--password \"<redacted>\n",
            ),
            (
                "x, \"--password \", hunter2\n",
                "x, \"--password \"<redacted>\n",
            ),
        ]);
    }

    #[test]
    fn curl_quoted_password_keeps_its_quotes() {
        assert_redacts(&[
            (
                "curl -u a:\"pw\" http://x\n",
                "curl -u a:\"<redacted>\" http://x\n",
            ),
            (
                "curl -u a:'pw' http://x\n",
                "curl -u a:'<redacted>' http://x\n",
            ),
            (
                "curl --user=a:\"pw\" http://x\n",
                "curl --user=a:\"<redacted>\" http://x\n",
            ),
            (
                "curl -u a:'p w' http://x\n",
                "curl -u a:'<redacted>' http://x\n",
            ),
            // Text glued to the closing quote is part of the password's shell word.
            (
                "curl --user=a:\"pw\"x http://x\n",
                "curl --user=a:\"<redacted>\"<redacted> http://x\n",
            ),
        ]);
    }

    #[test]
    fn exec_form_multi_line_quote_needs_a_separator_after_it() {
        assert_redacts(&[(
            "x: [\"a, b]\n  y: [\"--token\", \"t\"]\n",
            "x: [\"a, b]\n  y: [\"--token\", \"<redacted>\"]\n",
        )]);
    }

    #[test]
    fn exec_form_multi_line_quoted_flow_element() {
        assert_redacts(&[
            (
                "args: [\"--token\", \"abc\n  def\"]\n",
                "args: [\"--token\", \"<redacted>\"]\n",
            ),
            (
                "args: [\"--token\", \"abc\n\n  def\", \"--port\", \"1\"]\n",
                "args: [\"--token\", \"<redacted>\", \"--port\", \"1\"]\n",
            ),
            // An unclosed quote gives up on its line; the next line is still scanned.
            (
                "x: [\"it\n  y: [--token, t]\n",
                "x: [\"it\n  y: [--token, <redacted>]\n",
            ),
        ]);
    }

    #[test]
    fn exec_form_several_sequences_on_a_line() {
        assert_redacts(&[
            (
                "a: [x, --token, t] b: [y, --password, p]\n",
                "a: [x, --token, <redacted>] b: [y, --password, <redacted>]\n",
            ),
            (
                "[--token, t], [--password, p]\n",
                "[--token, <redacted>], [--password, <redacted>]\n",
            ),
        ]);
    }

    #[test]
    fn exec_form_curl_user() {
        assert_redacts(&[
            (
                "[\"curl\", \"-u\", \"admin:pw\", \"https://x\"]\n",
                "[\"curl\", \"-u\", \"admin:<redacted>\", \"https://x\"]\n",
            ),
            (
                "[\"curl\", \"--user=admin:pw\"]\n",
                "[\"curl\", \"--user=admin:<redacted>\"]\n",
            ),
            (
                "[\"curl\", \"-uadmin:pw\"]\n",
                "[\"curl\", \"-uadmin:<redacted>\"]\n",
            ),
            (
                "- curl\n- --user\n- a:b:c\n",
                "- curl\n- --user\n- a:<redacted>\n",
            ),
        ]);
        for kept in [
            "[\"curl\", \"-u\", \"admin\", \"https://x\"]\n",
            "[\"curl\", \"-u\", \"admin:\"]\n",
            "[\"wget\", \"-u\", \"a:b\"]\n",
        ] {
            assert_eq!(text(kept), kept);
        }
    }

    #[test]
    fn exec_form_dockerfile_flags() {
        assert_redacts(&[
            (
                "RUN --mount=type=secret,id=x [\"x\", \"--token\", \"t0k\"]\n",
                "RUN --mount=type=secret,id=x [\"x\", \"--token\", \"<redacted>\"]\n",
            ),
            (
                "HEALTHCHECK --interval=5s --retries=3 CMD [\"redis-cli\", \"-a\", \"x\"]\n",
                "HEALTHCHECK --interval=5s --retries=3 CMD [\"redis-cli\", \"-a\", \"<redacted>\"]\n",
            ),
        ]);
        // Up to eight flags, each up to 4 KiB, and instructions in any case.
        let eight = format!(
            "RUN {}[\"--token\", \"t\"]\n",
            "--a=1 ".repeat(MAX_DOCKERFILE_FLAGS)
        );
        assert_eq!(text(&eight), eight.replace("\"t\"]", "\"<redacted>\"]"));
        let mount = format!(
            "RUN --mount={} [\"x\",\"--token\",\"t0k\"]\n",
            "m".repeat(300)
        );
        assert_eq!(text(&mount), mount.replace("t0k", REDACTED));
        assert_redacts(&[
            (
                "run [\"x\", \"--token\", \"t\"]\n",
                "run [\"x\", \"--token\", \"<redacted>\"]\n",
            ),
            (
                "Run --mount=x [\"x\", \"--token\", \"t\"]\n",
                "Run --mount=x [\"x\", \"--token\", \"<redacted>\"]\n",
            ),
            (
                "cmd [\"x\", \"--token\", \"t\"]\n",
                "cmd [\"x\", \"--token\", \"<redacted>\"]\n",
            ),
            (
                "SHELL [\"/bin/sh\", \"-c\", \"--token\", \"t\"]\n",
                "SHELL [\"/bin/sh\", \"-c\", \"--token\", \"<redacted>\"]\n",
            ),
        ]);
        for kept in [
            // Flags must follow an instruction word, which is a whole word.
            "echo --x [\"redis-cli\", \"-a\", \"x\"]\n",
            "xRUN [\"redis-cli\", \"-a\", \"x\"]\n",
            "RUNS [\"redis-cli\", \"-a\", \"x\"]\n",
        ] {
            assert_eq!(text(kept), kept);
        }
        let nine = format!(
            "RUN {}[\"redis-cli\", \"-a\", \"x\"]\n",
            "--a=1 ".repeat(MAX_DOCKERFILE_FLAGS + 1)
        );
        assert_eq!(text(&nine), nine);
    }

    #[test]
    fn exec_form_line_endings_and_non_ascii() {
        assert_redacts(&[
            (
                "command: [\r\n  \"--requirepass\",\r\n  \"x\"\r\n]\r\n",
                "command: [\r\n  \"--requirepass\",\r\n  \"<redacted>\"\r\n]\r\n",
            ),
            (
                "command:\r\n  - --requirepass\r\n  - x\r\n",
                "command:\r\n  - --requirepass\r\n  - <redacted>\r\n",
            ),
            (
                "[\"é\",\"--token\",\"ü\"]\n",
                "[\"é\",\"--token\",\"<redacted>\"]\n",
            ),
        ]);
        let kept = "ÉCMD [\"--token\",\"t\"]\n";
        assert_eq!(text(kept), kept);
    }

    #[test]
    fn exec_form_block_run_boundaries() {
        assert_redacts(&[("- --token\n- |\n  x\n", "- --token\n- |\n  <redacted>\n")]);
        for kept in ["- --token\n  - t\n", "- --token\n-\n- t\n"] {
            assert_eq!(text(kept), kept);
        }
    }

    #[test]
    fn exec_form_flow_element_shapes() {
        assert_redacts(&[
            (
                "[\"redis-cli\",\"-a\",\"p,w\"]\n",
                "[\"redis-cli\",\"-a\",\"<redacted>\"]\n",
            ),
            (
                "[\"redis-cli\",\"-a\",\"p]w\", \"ping\"]\n",
                "[\"redis-cli\",\"-a\",\"<redacted>\", \"ping\"]\n",
            ),
            (
                "[\"--token\", \"a\\\"b\", \"c\"]\n",
                "[\"--token\", \"<redacted>\", \"c\"]\n",
            ),
            (
                "['--token', 'it''s', 'c']\n",
                "['--token', '<redacted>', 'c']\n",
            ),
            // An unclosed quote gives up its sequence; the next line is scanned anew.
            (
                "x: [\"a\", \"abc\ny: [\"--token\", \"t\"]\n",
                "x: [\"a\", \"abc\ny: [\"--token\", \"<redacted>\"]\n",
            ),
            // An inner sequence is scanned as a command of its own.
            ("x: [a, [--token, t]]\n", "x: [a, [--token, <redacted>]]\n"),
        ]);
    }

    #[test]
    fn exec_form_tool_passwords_end_to_end() {
        assert_redacts(&[
            (
                "[\"redis-cli\", \"--pass\", \"pw\", \"ping\"]\n",
                "[\"redis-cli\", \"--pass\", \"<redacted>\", \"ping\"]\n",
            ),
            (
                "[\"docker\", \"login\", \"-u\", \"bob\", \"-p\", \"pw\"]\n",
                "[\"docker\", \"login\", \"-u\", \"bob\", \"-p\", \"<redacted>\"]\n",
            ),
            (
                "[\"sshpass\", \"-p\", \"pw\", \"ssh\", \"host\"]\n",
                "[\"sshpass\", \"-p\", \"<redacted>\", \"ssh\", \"host\"]\n",
            ),
            (
                "[\"sshpass\", \"-ppw\", \"ssh\", \"host\"]\n",
                "[\"sshpass\", \"-p<redacted>\", \"ssh\", \"host\"]\n",
            ),
            (
                "[\"mongosh\", \"-u\", \"root\", \"-p\", \"pw\"]\n",
                "[\"mongosh\", \"-u\", \"root\", \"-p\", \"<redacted>\"]\n",
            ),
        ]);
    }

    #[test]
    fn exec_form_adversarial_inputs_stay_fast() {
        let n = 100 * 1024;
        let start = std::time::Instant::now();
        // Deep nesting and many openers.
        let deep = "[".repeat(n);
        assert_eq!(text(&deep), deep);
        let chained = "a: [x, ".repeat(n / 7);
        assert_eq!(text(&chained), chained);
        let glued = "x[".repeat(n / 2);
        assert_eq!(text(&glued), glued);
        // Look-back over Dockerfile flags and tags stays bounded.
        let flags = format!("{}[a]", "--x ".repeat(n / 4));
        assert_eq!(text(&flags), flags);
        let tags = "!t [".repeat(n / 4);
        assert_eq!(text(&tags), tags);
        // Block scalars, continuations and comments.
        let scalars = "- --token\n- |\n  x\n".repeat(n / 20);
        assert_eq!(
            text(&scalars),
            "- --token\n- |\n  <redacted>\n".repeat(n / 20)
        );
        let continued = "- a\n  b\n".repeat(n / 8);
        assert_eq!(text(&continued), continued);
        let commented = format!("x: [{}\"--token\", \"t\"]\n", "a, # c\n".repeat(n / 7));
        assert_eq!(
            text(&commented),
            format!(
                "x: [{}\"--token\", \"<redacted>\"]\n",
                "a, # c\n".repeat(n / 7)
            )
        );
        // Many command-line matches in one long scanned sequence share one scan.
        let line_rule = format!("[{}]\n", "mysql -pa, ".repeat(n / 11));
        assert_eq!(
            text(&line_rule),
            format!("[{}]\n", "mysql -p<redacted>, ".repeat(n / 11))
        );
        let quoted = "- \"a\n".repeat(n / 4);
        assert_eq!(text(&quoted), quoted);
        let nested_tail = format!("x: [{}--token, t]\n", "[a], {b: c}, ".repeat(n / 13));
        assert_eq!(
            text(&nested_tail),
            format!(
                "x: [{}--token, <redacted>]\n",
                "[a], {b: c}, ".repeat(n / 13)
            )
        );
        // Each `[` looks back over one maximal flag word, then stops at the `[x]` before.
        let long_flags = format!("--{} [x] ", "m".repeat(MAX_DOCKERFILE_FLAG - 2)).repeat(n / 1024);
        assert_eq!(text(&long_flags), long_flags);
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn long_exec_form_line_stays_fast() {
        let pairs = 100 * 1024 / "\"--password\", \"x\", ".len();
        let line = format!(
            "command: [{}\"end\"]\n",
            "\"--password\", \"x\", ".repeat(pairs)
        );
        let expected = format!(
            "command: [{}\"end\"]\n",
            "\"--password\", \"<redacted>\", ".repeat(pairs)
        );
        assert!(line.len() >= 100 * 1024 - 32);
        let start = std::time::Instant::now();
        assert_eq!(text(&line), expected);
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
    fn is_secret_ignores_the_neutralised_marker_char() {
        assert!(!is_secret("NAME", "a\u{E000}b"));
        assert!(!is_secret("NAME", "\u{E000}redacted\u{E000}"));
        assert!(is_secret("URL", "redis://:pw@localhost/\u{E000}"));
        assert!(is_secret("API_TOKEN", "\u{E000}"));
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
