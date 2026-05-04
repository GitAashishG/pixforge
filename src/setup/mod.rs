//! Interactive setup wizard for pixforge profiles.
//!
//! See [`wizard::run`] for the entry point. The wizard is structured around
//! four trait seams ([`Prompter`], [`ConfigStore`], [`ShellRcWriter`],
//! [`ConnectionTester`]) so unit tests can drive the entire flow with
//! canned answers and no real IO.

pub mod config_edit;
pub mod io;
pub mod traits;
pub mod wizard;

pub use traits::{AppendOutcome, ConfigStore, ConnectionTester, Prompter, ShellRcWriter, TestOutcome};
pub use wizard::{run, EnvProbe, WizardDeps, WizardResult};
