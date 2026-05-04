//! Trait seams for the `pixforge setup` wizard.
//!
//! These let unit tests inject canned answers, in-memory files, no-op
//! shell-rc writers, and mock connection testers — so wizard logic can
//! be tested without a TTY or any real IO.

use anyhow::Result;
use std::path::PathBuf;

use crate::config::Profile;

/// Anything that can ask the user for input. Production impl uses
/// `dialoguer`; tests use a canned-answer queue.
pub trait Prompter {
    fn ask_text(&mut self, label: &str, default: Option<&str>) -> Result<String>;
    fn ask_choice(&mut self, label: &str, choices: &[String]) -> Result<usize>;
    fn confirm(&mut self, label: &str, default: bool) -> Result<bool>;
    fn ask_secret(&mut self, label: &str) -> Result<String>;
    /// Print a one-line informational message (not a prompt).
    fn info(&mut self, msg: &str);
    /// Print a multi-line block (e.g. instructions). No interaction.
    fn note(&mut self, msg: &str);
}

/// Reads + writes the pixforge config file. Production impl uses real fs;
/// tests use an in-memory string.
pub trait ConfigStore {
    /// Returns `Ok(None)` if the file doesn't exist; `Ok(Some(text))` if it
    /// does. `Err` only on IO error (permissions, etc.).
    fn read(&self) -> Result<Option<String>>;
    /// Atomic write: caller is expected to have already serialized the new
    /// full file contents.
    fn write(&self, contents: &str) -> Result<()>;
    /// Where the file lives, for user-facing messages.
    fn path(&self) -> PathBuf;
}

/// Detects the user's shell rc file and appends an export line idempotently.
pub trait ShellRcWriter {
    /// Returns `Some(path)` of the rc file we'd edit, or `None` if we can't
    /// determine the user's shell. Used by the wizard to decide whether to
    /// even offer the "save to shell" option.
    fn rc_path(&self) -> Option<PathBuf>;
    /// Append `export VAR="VAL"` to the rc file unless an existing
    /// `export VAR=` line is already present (idempotent). Returns whether
    /// the file was modified.
    fn append_export(&self, var: &str, val: &str) -> Result<AppendOutcome>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended { path: PathBuf },
    AlreadyPresent { path: PathBuf },
}

/// Run a real generation against a profile to confirm it works end-to-end.
/// Production impl calls the actual provider; tests use a mock that returns
/// canned ok/err.
pub trait ConnectionTester {
    fn test(&mut self, profile: &Profile) -> Result<TestOutcome>;
}

#[derive(Debug)]
pub struct TestOutcome {
    pub bytes: usize,
    pub latency_secs: f64,
    pub attempts: u32,
}
