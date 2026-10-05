//! Guarded YAML parsing for files a cloned repository controls.
//!
//! serde_norway's only alias guard counts alias jumps, so a flat document (one anchored
//! list of `k` scalars plus `m` aliases to it) expands to `m * k` nodes: a few KB of
//! input can cost hundreds of MB. Neither devy.yml nor devy.lock needs anchors, so both
//! are pre-scanned with libyaml's event parser (the same one serde_norway uses, so the
//! two always agree on what is an alias) and rejected if they contain any. Third-party
//! files that legitimately use anchors (docker compose) get an expansion budget instead.

use anyhow::{Result, bail};
use serde::de::DeserializeOwned;
use serde_norway as yaml;
use std::collections::HashMap;
use std::io::Read;
use std::mem::MaybeUninit;
use std::path::Path;
use unsafe_libyaml_norway as unsafe_sys;

/// The largest YAML input devy parses: far beyond any real devy.yml or devy.lock.
pub const MAX_YAML_BYTES: usize = 1024 * 1024;

/// How the pre-scan treats anchors and aliases.
#[derive(Clone, Copy, Debug)]
pub enum Aliases {
    /// Any anchor (`&name`) or alias (`*name`) is an error.
    Reject,
    /// Aliases are allowed while the fully expanded document stays within `nodes` nodes
    /// and `bytes` bytes of scalar text, and no alias refers to a node that contains it.
    Budget { nodes: u64, bytes: u64 },
}

/// Reads `path` as UTF-8, failing with `InvalidData` instead of reading past
/// [`MAX_YAML_BYTES`]. A missing file keeps its `NotFound` kind. Anything but a regular
/// file (after following symlinks) is refused before it is opened, so a committed
/// symlink to a FIFO or device can neither block nor stream forever.
pub fn read_capped(path: &Path) -> std::io::Result<String> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let file = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(MAX_YAML_BYTES as u64 + 1).read_to_end(&mut buf)?;
    if buf.len() > MAX_YAML_BYTES {
        return Err(too_large_io(path));
    }
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn too_large_io(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "{} is larger than the {} MiB limit",
            path.display(),
            MAX_YAML_BYTES / (1024 * 1024)
        ),
    )
}

/// Deserializes `content` after enforcing the size cap and rejecting anchors and aliases.
/// `label` names the input (a path, `devy.lock`, "the proposed devy.yml") in errors.
pub fn from_str_strict<T: DeserializeOwned>(content: &str, label: &str) -> Result<T> {
    check(content, label, Aliases::Reject)?;
    Ok(yaml::from_str(content)?)
}

/// Enforces the size cap and `aliases` policy on `content` without deserializing it.
/// Syntax errors are left for the deserializer, which reports them with a location.
pub fn check(content: &str, label: &str, aliases: Aliases) -> Result<()> {
    if content.len() > MAX_YAML_BYTES {
        bail!(
            "{label} is larger than the {} MiB limit",
            MAX_YAML_BYTES / (1024 * 1024)
        );
    }
    match scan(content.as_bytes(), aliases) {
        Scan::Ok => Ok(()),
        Scan::Anchor(line) => bail!(
            "{label}: YAML anchors and aliases are not supported (`&name` or `*name` on line {line})"
        ),
        Scan::Recursive(line) => bail!(
            "{label}: a YAML alias on line {line} refers to a node that contains it; refusing to parse it"
        ),
        Scan::OverBudget => bail!(
            "{label}: YAML aliases expand the document past devy's size limit; refusing to parse it"
        ),
    }
}

enum Scan {
    Ok,
    /// 1-based line of the first anchor or alias.
    Anchor(u64),
    /// 1-based line of an alias to a node that is still open (it contains the alias).
    Recursive(u64),
    OverBudget,
}

/// The expanded size of a node: nodes, and bytes of scalar text.
#[derive(Clone, Copy, Default)]
struct Cost {
    nodes: u64,
    bytes: u64,
}

impl Cost {
    fn plus(self, other: Cost) -> Cost {
        Cost {
            nodes: self.nodes.saturating_add(other.nodes),
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }
}

/// An initialized libyaml parser over borrowed input. Boxed so it never moves after
/// `yaml_parser_initialize`, and deleted on drop.
struct Parser<'a> {
    sys: Box<MaybeUninit<unsafe_sys::yaml_parser_t>>,
    _input: &'a [u8],
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8]) -> Option<Self> {
        let mut sys = Box::new(MaybeUninit::<unsafe_sys::yaml_parser_t>::uninit());
        // SAFETY: `yaml_parser_initialize` zeroes and sets up the struct in place (in
        // this crate version it cannot fail; allocation failure aborts). If it ever did
        // report failure, `Parser` is never built, so `yaml_parser_delete` never runs on
        // it. The parser stores a pointer to itself as read-handler data, which is why it
        // stays boxed and never moves. The input slice outlives it through `_input`.
        unsafe {
            let parser = sys.as_mut_ptr();
            if !unsafe_sys::yaml_parser_initialize(parser).ok {
                return None;
            }
            unsafe_sys::yaml_parser_set_encoding(parser, unsafe_sys::YAML_UTF8_ENCODING);
            unsafe_sys::yaml_parser_set_input_string(parser, input.as_ptr(), input.len() as _);
        }
        Some(Parser { sys, _input: input })
    }

    /// Calls `f` with the next event, then frees it. `None` on a syntax error.
    fn next<R>(&mut self, f: impl FnOnce(&unsafe_sys::yaml_event_t) -> R) -> Option<R> {
        let mut event = MaybeUninit::<unsafe_sys::yaml_event_t>::uninit();
        // SAFETY: the parser was initialized in `new`; a successful parse fully
        // initializes `event`, which is deleted exactly once after `f` borrows it.
        unsafe {
            if !unsafe_sys::yaml_parser_parse(self.sys.as_mut_ptr(), event.as_mut_ptr()).ok {
                return None;
            }
            let ret = f(&*event.as_ptr());
            unsafe_sys::yaml_event_delete(event.as_mut_ptr());
            Some(ret)
        }
    }
}

impl Drop for Parser<'_> {
    fn drop(&mut self) {
        // SAFETY: only constructed after a successful `yaml_parser_initialize`.
        unsafe { unsafe_sys::yaml_parser_delete(self.sys.as_mut_ptr()) }
    }
}

/// What one event contributes to the scan, copied out of libyaml's buffers.
enum Ev {
    End,
    Other,
    Alias(Vec<u8>),
    /// Anchor and length in bytes.
    Scalar(Option<Vec<u8>>, u64),
    Start(Option<Vec<u8>>),
    Close,
}

/// Copies a NUL-terminated anchor name, if present.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated string valid for the call.
unsafe fn anchor(ptr: *const u8) -> Option<Vec<u8>> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    let cstr = unsafe { std::ffi::CStr::from_ptr(ptr.cast()) };
    Some(cstr.to_bytes().to_vec())
}

fn classify(e: &unsafe_sys::yaml_event_t) -> Ev {
    // SAFETY: each union field is read only for the event type that libyaml documents
    // as setting it; anchor pointers are null or NUL-terminated strings owned by `e`.
    unsafe {
        match e.type_ {
            unsafe_sys::YAML_STREAM_END_EVENT => Ev::End,
            unsafe_sys::YAML_ALIAS_EVENT => {
                Ev::Alias(anchor(e.data.alias.anchor).unwrap_or_default())
            }
            unsafe_sys::YAML_SCALAR_EVENT => {
                Ev::Scalar(anchor(e.data.scalar.anchor), e.data.scalar.length)
            }
            unsafe_sys::YAML_SEQUENCE_START_EVENT => {
                Ev::Start(anchor(e.data.sequence_start.anchor))
            }
            unsafe_sys::YAML_MAPPING_START_EVENT => Ev::Start(anchor(e.data.mapping_start.anchor)),
            unsafe_sys::YAML_SEQUENCE_END_EVENT | unsafe_sys::YAML_MAPPING_END_EVENT => Ev::Close,
            _ => Ev::Other,
        }
    }
}

/// Walks the event stream once. Under `Budget`, each anchored subtree's expanded size
/// is recorded when it closes and charged again at every alias to it, so the total is
/// what a full expansion would produce, computed in linear time. serde_norway registers
/// an anchor when its node starts, so an alias inside the node it names would expand
/// recursively; such an alias is refused outright.
fn scan(input: &[u8], aliases: Aliases) -> Scan {
    let Some(mut parser) = Parser::new(input) else {
        return Scan::Ok;
    };
    let budget = match aliases {
        Aliases::Reject => None,
        Aliases::Budget { nodes, bytes } => Some(Cost { nodes, bytes }),
    };
    let mut sizes: HashMap<Vec<u8>, Cost> = HashMap::new();
    // Anchors of currently open collections, with how many times each is open.
    let mut open: HashMap<Vec<u8>, u32> = HashMap::new();
    // Open collections: (anchor, expanded size of their children so far).
    let mut stack: Vec<(Option<Vec<u8>>, Cost)> = Vec::new();
    // What a full expansion produces so far: every scalar and closed collection once,
    // plus the recorded size of the target at every alias.
    let mut expanded = Cost::default();
    loop {
        let Some((ev, line)) = parser.next(|e| (classify(e), e.start_mark.line.saturating_add(1)))
        else {
            // Syntax error: serde_norway hits the same error and reports it.
            return Scan::Ok;
        };
        // (anchored or alias, expanded size of this node, anchor it closes, newly
        // counted size: a closing collection's children were counted as they arrived)
        let (anchored, size, closed_anchor, added) = match ev {
            Ev::End => return Scan::Ok,
            Ev::Other => continue,
            Ev::Alias(name) => {
                if budget.is_some() && open.contains_key(&name) {
                    return Scan::Recursive(line);
                }
                let size = sizes
                    .get(&name)
                    .copied()
                    .unwrap_or(Cost { nodes: 1, bytes: 0 });
                (true, size, None, size)
            }
            Ev::Scalar(a, len) => {
                let size = Cost {
                    nodes: 1,
                    bytes: len,
                };
                (a.is_some(), size, a, size)
            }
            Ev::Start(a) => {
                if let Some(name) = &a {
                    if budget.is_none() {
                        return Scan::Anchor(line);
                    }
                    *open.entry(name.clone()).or_default() += 1;
                }
                stack.push((a, Cost::default()));
                continue;
            }
            Ev::Close => match stack.pop() {
                Some((a, inner)) => {
                    if let Some(name) = &a
                        && let Some(n) = open.get_mut(name)
                    {
                        *n -= 1;
                        if *n == 0 {
                            open.remove(name);
                        }
                    }
                    let own = Cost { nodes: 1, bytes: 0 };
                    (false, inner.plus(own), a, own)
                }
                None => continue,
            },
        };
        let Some(budget) = budget else {
            if anchored {
                return Scan::Anchor(line);
            }
            continue;
        };
        expanded = expanded.plus(added);
        if let Some(name) = closed_anchor {
            sizes.insert(name, size);
        }
        if let Some((_, inner)) = stack.last_mut() {
            *inner = inner.plus(size);
        }
        if expanded.nodes > budget.nodes || expanded.bytes > budget.bytes {
            return Scan::OverBudget;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{billion_laughs, flat_quadratic};

    fn rejects(doc: &str) -> bool {
        check(doc, "devy.yml", Aliases::Reject).is_err()
    }

    #[test]
    fn plain_documents_pass() {
        assert!(!rejects("dependencies:\n  - node\n"));
        assert!(!rejects("name: \"*not an alias\"\n# *nor this &or this\n"));
        assert!(!rejects(
            "x: '&a *b'\ny: |\n  *literal\n  &block\nz: a*b&c\n"
        ));
        assert!(
            !rejects("name: [unclosed\n"),
            "syntax errors are left to serde"
        );
    }

    #[test]
    fn any_anchor_or_alias_is_rejected() {
        assert!(rejects("a: &x 1\n"));
        assert!(rejects("a: &x [1]\n"));
        assert!(rejects("a: &x {b: 1}\n"));
        assert!(rejects("a: *x\n"));
        assert!(rejects(&billion_laughs("dependencies")));
        assert!(rejects(&flat_quadratic("dependencies", 1000, 1000)));
        let err = check("a: 1\nb: &x 2\n", "devy.yml", Aliases::Reject).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("devy.yml") && msg.contains("not supported"),
            "{msg}"
        );
        assert!(msg.contains("line 2"), "{msg}");
    }

    const BIG: Aliases = Aliases::Budget {
        nodes: 100_000,
        bytes: 8 << 20,
    };

    #[test]
    fn budget_counts_expanded_bytes_and_refuses_recursion() {
        // One large anchored scalar aliased many times: few nodes, many bytes.
        let big = "x".repeat(64 * 1024);
        let mut doc = format!("s: &s {big}\nservices: [");
        doc.push_str(&vec!["*s"; 1000].join(","));
        doc.push_str("]\n");
        let err = check(&doc, "compose.yml", BIG).unwrap_err().to_string();
        assert!(err.contains("size limit"), "{err}");
        // The same with a few aliases is fine.
        let few = format!("s: &s {big}\nservices: [*s, *s]\n");
        assert!(check(&few, "compose.yml", BIG).is_ok());

        // An alias inside the node it names expands recursively in serde_norway.
        for doc in ["services: &a [x, *a]\n", "a: &a [1]\nb: &a [*a]\n"] {
            let err = check(doc, "compose.yml", BIG).unwrap_err().to_string();
            assert!(err.contains("contains it"), "{doc}: {err}");
        }
        // Aliasing a closed sibling with the same structure is fine.
        assert!(check("a: &a [1]\nb: [*a, *a]\n", "compose.yml", BIG).is_ok());
    }

    #[test]
    fn budget_counts_expanded_nodes() {
        let small = "x: &c {image: redis}\nservices:\n  a: *c\n  b: *c\n";
        assert!(
            check(
                small,
                "compose.yml",
                Aliases::Budget {
                    nodes: 100,
                    bytes: 1 << 20
                }
            )
            .is_ok()
        );
        assert!(
            check(
                small,
                "compose.yml",
                Aliases::Budget {
                    nodes: 5,
                    bytes: 1 << 20
                }
            )
            .is_err()
        );
        let flat = flat_quadratic("services", 1000, 1000);
        assert!(check(&flat, "compose.yml", BIG).is_err());
        let laughs = billion_laughs("services");
        assert!(check(&laughs, "compose.yml", BIG).is_err());
    }

    #[test]
    fn oversized_input_is_rejected() {
        let big = format!("name: \"{}\"\n", "a".repeat(MAX_YAML_BYTES));
        let err = check(&big, "devy.yml", Aliases::Reject).unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
        let path = crate::test_support::tmp_path(".yml");
        std::fs::write(&path, &big).unwrap();
        let err = read_capped(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let dir = crate::test_support::tmp_dir();
        let err = read_capped(&dir).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
        assert_eq!(
            read_capped(&dir.join("missing")).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        std::fs::write(&path, "a: 1\n").unwrap();
        assert_eq!(read_capped(&path).unwrap(), "a: 1\n");
    }
}
