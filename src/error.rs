/// Signals that the process should exit with the given code without printing an
/// additional error message. Used by commands that already printed their own
/// diagnostic output (e.g. `devy check`) and just need a non-zero exit code.
#[derive(Debug)]
pub struct SilentExit(pub i32);

impl std::fmt::Display for SilentExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit code {}", self.0)
    }
}

impl std::error::Error for SilentExit {}

/// An error followed by a hint line. `main` prints `error: <inner>` and then `hint`.
#[derive(Debug)]
pub struct HintedError {
    pub inner: anyhow::Error,
    pub hint: String,
}

impl std::fmt::Display for HintedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.inner)
    }
}

impl std::error::Error for HintedError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_exit_displays_code() {
        assert_eq!(format!("{}", SilentExit(1)), "exit code 1");
        assert_eq!(format!("{}", SilentExit(42)), "exit code 42");
    }

    #[test]
    fn hinted_error_displays_inner_chain() {
        let err = HintedError {
            inner: anyhow::anyhow!("root").context("outer"),
            hint: "try this".into(),
        };
        assert_eq!(err.to_string(), "outer: root");
    }

    #[test]
    fn silent_exit_code_zero_displays() {
        assert_eq!(format!("{}", SilentExit(0)), "exit code 0");
    }
}
