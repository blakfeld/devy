use colored::Colorize;

pub fn header(msg: &str) {
    println!("\n{}", msg.bold());
}

pub fn step(msg: &str) {
    println!("  {} {}", "→".blue().bold(), msg);
}

pub fn success(msg: &str) {
    println!("  {} {}", "✓".green().bold(), msg);
}

pub fn skip(msg: &str) {
    println!("  {} {}", "○".dimmed(), msg.dimmed());
}

pub fn info(msg: &str) {
    println!("  {} {}", "·".cyan(), msg);
}

pub fn info_code(msg: &str, code: &str) {
    println!("  {} {} {}", "·".cyan(), msg, code.bold());
}

pub fn blank_line() {
    println!();
}

pub fn warn(msg: &str) {
    #[cfg(test)]
    if WARN_HOOK.with(|cell| {
        if let Some(f) = cell.borrow().as_ref() {
            f(msg);
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
