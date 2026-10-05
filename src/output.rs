use colored::Colorize;
use std::borrow::Cow;

/// Removes terminal control characters and escape sequences from text that comes from
/// outside devy (config values, file contents, logs, child-process errors, model
/// replies), so it cannot move the cursor, set the clipboard (OSC 52), forge links
/// (OSC 8) or otherwise drive the terminal. Newline and tab are kept; every other C0
/// control, DEL and every C1 control is removed, along with the whole escape sequence
/// it introduces (CSI, OSC, DCS, SOS, PM, APC and `ESC <intermediates> <final>`).
///
/// Bidirectional-override and invisible formatting characters (Trojan-Source style
/// reordering, zero-width text, Unicode line separators) are removed too. ZWJ/ZWNJ are
/// kept because emoji and several scripts need them.
///
/// Use it for multi-line content (log tails, previews, model replies). For a value shown
/// inside one line (a table cell, a label, a name) use [`clean_line`].
///
/// Apply it to the untrusted text *before* colorizing it, never to devy's own styled
/// output.
pub fn clean(s: &str) -> Cow<'_, str> {
    sanitize(s, false)
}

/// [`clean`], with newlines and tabs also turned into spaces, so a value cannot add
/// forged rows to a table or break its column alignment.
pub fn clean_line(s: &str) -> Cow<'_, str> {
    match clean(s) {
        c if c.contains(['\n', '\t']) => Cow::Owned(c.replace(['\n', '\t'], " ")),
        c => c,
    }
}

/// For service log text: like [`clean`], but when `tty` is set SGR color sequences
/// (`ESC [ <digits and ;> m`) are kept. Each line that kept one ends with an SGR reset,
/// so a log line cannot conceal or recolor what follows it. Every other escape sequence
/// is always removed.
pub fn clean_log(s: &str, tty: bool) -> Cow<'_, str> {
    sanitize(s, tty)
}

const ESC: char = '\u{1b}';
const BEL: char = '\u{7}';
/// C1 String Terminator.
const ST: char = '\u{9c}';
const SGR_RESET: &str = "\u{1b}[0m";

fn needs_cleaning(c: char) -> bool {
    // `is_control` covers C0, DEL and C1.
    (c.is_control() && c != '\n' && c != '\t') || is_invisible_format(c)
}

/// Bidi marks, embeddings, overrides and isolates (including the Arabic letter mark),
/// zero-width and invisible characters (soft hyphen, word joiner, invisible operators,
/// BOM, Mongolian vowel separator, deprecated format controls, interlinear annotation
/// marks), the Unicode line/paragraph separators and the tag
/// characters used for invisible "ASCII smuggling" (this also drops the tag sequences of
/// subdivision-flag emoji, which then show as a plain black flag).
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{61c}'
            | '\u{180e}'
            | '\u{200b}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e007f}'
    )
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

fn sanitize(s: &str, keep_sgr: bool) -> Cow<'_, str> {
    if !s.chars().any(needs_cleaning) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    // Whether an SGR was kept on the current line (and needs a reset before it ends).
    let mut styled = false;
    while let Some(c) = chars.next() {
        if !needs_cleaning(c) {
            if c == '\n' && styled {
                out.push_str(SGR_RESET);
                styled = false;
            }
            out.push(c);
            continue;
        }
        match c {
            ESC => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    styled |= csi(&mut chars, &mut out, keep_sgr);
                }
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    chars.next();
                    control_string(&mut chars);
                }
                // `ESC <intermediates 0x20-0x2f> <final 0x30-0x7e>`, e.g. `ESC 7`,
                // `ESC ( B`, `ESC c`. Anything out of range (a newline, ordinary
                // text) ends the sequence and is kept.
                Some(_) => {
                    while chars
                        .next_if(|n| ('\u{20}'..='\u{2f}').contains(n))
                        .is_some()
                    {}
                    chars.next_if(|n| ('\u{30}'..='\u{7e}').contains(n));
                }
                None => {}
            },
            // 8-bit forms of `ESC [` and `ESC ] / P / X / ^ / _`.
            '\u{9b}' => styled |= csi(&mut chars, &mut out, keep_sgr),
            '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => control_string(&mut chars),
            _ => {}
        }
    }
    if styled {
        out.push_str(SGR_RESET);
    }
    Cow::Owned(out)
}

/// Consumes a CSI sequence after its introducer, writing it back (in 7-bit form) only
/// when it is SGR and `keep_sgr` is set. A character that cannot belong to a CSI (a
/// newline, ordinary text, another control) ends the sequence without being consumed,
/// and the unfinished sequence is dropped. Returns whether an SGR was written.
fn csi(chars: &mut Chars<'_>, out: &mut String, keep_sgr: bool) -> bool {
    let mut sgr = keep_sgr;
    let start = out.len();
    if keep_sgr {
        out.push(ESC);
        out.push('[');
    }
    // Parameter and intermediate bytes.
    while let Some(c) = chars.next_if(|c| ('\u{20}'..='\u{3f}').contains(c)) {
        if sgr && (c.is_ascii_digit() || c == ';') {
            out.push(c);
        } else {
            sgr = false;
        }
    }
    // Final byte.
    let fin = chars.next_if(|c| ('\u{40}'..='\u{7e}').contains(c));
    if sgr && fin == Some('m') {
        out.push('m');
        true
    } else {
        out.truncate(start);
        false
    }
}

/// Consumes an OSC/DCS/SOS/PM/APC string up to and including its terminator (BEL,
/// `ESC \` or C1 ST). An unterminated string ends at the next newline, which is kept, so
/// a stray introducer can hide at most the rest of its line.
fn control_string(chars: &mut Chars<'_>) {
    while let Some(c) = chars.next_if(|&c| c != '\n') {
        match c {
            BEL | ST => return,
            ESC => {
                chars.next_if_eq(&'\\');
                return;
            }
            _ => {}
        }
    }
}

pub fn header(msg: &str) {
    println!("\n{}", clean_line(msg).as_ref().bold());
}

pub fn step(msg: &str) {
    println!("  {} {}", "→".blue().bold(), clean(msg));
}

pub fn success(msg: &str) {
    println!("  {} {}", "✓".green().bold(), clean(msg));
}

pub fn skip(msg: &str) {
    println!("  {} {}", "○".dimmed(), clean(msg).as_ref().dimmed());
}

pub fn info(msg: &str) {
    println!("  {} {}", "·".cyan(), clean(msg));
}

pub fn info_code(msg: &str, code: &str) {
    println!(
        "  {} {} {}",
        "·".cyan(),
        clean(msg),
        clean(code).as_ref().bold()
    );
}

pub fn blank_line() {
    println!();
}

/// Prints a top-level error to stderr as `error: <msg>`, cleaned.
pub fn error(msg: &str) {
    eprintln!("error: {}", clean(msg));
}

pub fn warn(msg: &str) {
    let msg = clean(msg);
    #[cfg(test)]
    if WARN_HOOK.with(|cell| {
        if let Some(f) = cell.borrow().as_ref() {
            f(&msg);
            true
        } else {
            false
        }
    }) {
        return;
    }
    eprintln!("  {} {}", "!".yellow().bold(), msg);
}

#[cfg(test)]
type WarnHook = Box<dyn Fn(&str)>;

#[cfg(test)]
std::thread_local! {
    static WARN_HOOK: std::cell::RefCell<Option<WarnHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `f`, returns the number of times `warn()` was called during `f`.
/// Safe to use in parallel tests — each thread has its own counter.
#[cfg(test)]
pub fn with_warn_capture<F: FnOnce()>(f: F) -> usize {
    with_warn_messages(f).len()
}

/// Runs `f`, returns every message passed to `warn()` during `f`.
/// Safe to use in parallel tests — each thread has its own buffer.
#[cfg(test)]
pub fn with_warn_messages<F: FnOnce()>(f: F) -> Vec<String> {
    use std::cell::RefCell;
    use std::rc::Rc;
    let msgs = Rc::new(RefCell::new(Vec::new()));
    let msgs_clone = Rc::clone(&msgs);
    WARN_HOOK.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move |m: &str| {
            msgs_clone.borrow_mut().push(m.to_string());
        }));
    });
    f();
    WARN_HOOK.with(|cell| *cell.borrow_mut() = None);
    msgs.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_functions_accept_arbitrary_messages() {
        header("hello world");
        step("doing something");
        success("all good");
        skip("skipping this");
        info("some info");
    }

    #[test]
    fn clean_leaves_plain_text_borrowed() {
        let s = "plain → text ✓\n\twith tab";
        assert!(matches!(clean(s), Cow::Borrowed(_)));
        assert_eq!(clean(s), s);
    }

    #[test]
    fn clean_removes_osc52_clipboard_write() {
        // BEL- and ST-terminated forms.
        assert_eq!(clean("app\x1b]52;c;ZWNobyBoaQ==\x07!"), "app!");
        assert_eq!(clean("app\x1b]52;c;ZWNobyBoaQ==\x1b\\!"), "app!");
        assert_eq!(clean_log("a\x1b]52;c;eA==\x07b", true), "ab");
    }

    #[test]
    fn clean_removes_osc8_hyperlinks() {
        let link = "\x1b]8;;https://evil.example\x1b\\click\x1b]8;;\x1b\\";
        assert_eq!(clean(link), "click");
        assert_eq!(clean_log(link, true), "click");
    }

    #[test]
    fn clean_removes_cursor_movement_and_screen_control() {
        let s = "a\x1b[2Jb\x1b[1;1Hc\x1b[3Ad\x1b[?25le\x1b7f\x1b(Bg\x1bch";
        assert_eq!(clean(s), "abcdefgh");
        assert_eq!(clean_log(s, true), "abcdefgh");
    }

    #[test]
    fn clean_removes_c0_del_and_c1_controls() {
        assert_eq!(clean("a\rb\x08c\x07d\x7fe\u{85}f\0g"), "abcdefg");
        // 8-bit CSI and OSC.
        assert_eq!(clean("a\u{9b}2Jb\u{9d}52;c;eA==\u{9c}c"), "abc");
        assert_eq!(clean("keep\nnew\tlines"), "keep\nnew\tlines");
    }

    #[test]
    fn clean_drops_unterminated_sequences() {
        assert_eq!(clean("a\x1b]52;c;eA=="), "a");
        assert_eq!(clean("a\x1b[12"), "a");
        assert_eq!(clean("a\x1b"), "a");
    }

    #[test]
    fn unterminated_sequences_never_swallow_later_lines() {
        assert_eq!(clean("a\x1b]52;x\nb\nc"), "a\nb\nc");
        assert_eq!(clean("a\x1bP junk\nb"), "a\nb");
        assert_eq!(clean("a\u{9d}junk\nb"), "a\nb");
        assert_eq!(clean("a\x1b[1\nb"), "a\nb");
        assert_eq!(clean("line1\x1b\nline2"), "line1\nline2");
        // An invalid CSI character is kept, not eaten.
        assert_eq!(clean("a\x1b[1ëb"), "aëb");
    }

    #[test]
    fn clean_removes_bidi_and_invisible_format_characters() {
        assert_eq!(
            clean("a\u{202e}b\u{2066}c\u{2069}d\u{200b}e\u{feff}f"),
            "abcdef"
        );
        assert_eq!(clean("x\u{2028}y"), "xy");
        assert_eq!(
            clean(
                "a\u{ad}b\u{61c}c\u{180e}d\u{2060}e\u{2064}f\u{e0041}\u{e007f}g\u{206a}h\u{206f}i\u{fff9}j\u{fffb}"
            ),
            "abcdefghij"
        );
        // ZWJ emoji sequences survive.
        assert_eq!(clean("👩\u{200d}💻"), "👩\u{200d}💻");
    }

    #[test]
    fn clean_line_flattens_newlines_and_tabs() {
        assert_eq!(clean_line("debug\n  FAKE  row\tx"), "debug   FAKE  row x");
        assert_eq!(clean_line("a\x1b[2J\nb"), "a b");
        assert!(matches!(clean_line("plain"), Cow::Borrowed(_)));
    }

    #[test]
    fn kept_sgr_is_reset_at_the_end_of_each_line() {
        assert_eq!(
            clean_log("a\x1b[8mhidden\nnext", true),
            "a\x1b[8mhidden\x1b[0m\nnext"
        );
        assert_eq!(clean_log("a\u{9b}31mred", true), "a\x1b[31mred\x1b[0m");
        assert_eq!(clean_log("a\x1b[8mhidden\nnext", false), "ahidden\nnext");
    }

    #[test]
    fn clean_strips_sgr_but_clean_log_keeps_it_on_a_tty() {
        let s = "\x1b[1;31merror\x1b[0m done\x1b[m";
        assert_eq!(clean(s), "error done");
        assert_eq!(clean_log(s, false), "error done");
        assert_eq!(clean_log(s, true), format!("{s}\x1b[0m"));
        // Not SGR even though it ends in `m`: private-mode parameters.
        assert_eq!(clean_log("a\x1b[?1mb", true), "ab");
        assert_eq!(clean_log("a\x1b[>4;2mb", true), "ab");
    }

    #[test]
    fn warn_passes_cleaned_message() {
        let msgs = with_warn_messages(|| warn("x\x1b]52;c;eA==\x07y"));
        assert_eq!(msgs, vec!["xy".to_string()]);
    }

    #[test]
    fn warn_capture_counts_calls() {
        let n = with_warn_capture(|| {
            warn("first");
            warn("second");
        });
        assert_eq!(n, 2);
    }

    #[test]
    fn warn_capture_resets_after_closure() {
        with_warn_capture(|| warn("inside"));
        // After the closure, hook is cleared — calling warn again must not panic.
        warn("outside");
    }
}
