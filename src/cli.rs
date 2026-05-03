use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Fast CLI for generating images via OpenAI, Azure, Gemini, LocalAI, and other
/// image-gen providers. Configure profiles in `~/.config/pixforge/config.toml`.
#[derive(Debug, Parser)]
#[command(name = "pixforge", version, about, long_about = None)]
pub struct Cli {
    /// Prompt text. Use `-` to read the prompt from stdin (until EOF).
    #[arg(short = 'p', long = "prompt", global = true)]
    pub prompt: Option<String>,

    /// Output PNG path. Default: ./pixforge-{YYYYMMDD-HHMMSS}-{hash6}.png
    #[arg(short = 'o', long = "output", global = true)]
    pub output: Option<PathBuf>,

    /// Profile name from config.toml. Overrides PIXFORGE_PROFILE and default_profile.
    #[arg(long = "profile", global = true)]
    pub profile: Option<String>,

    /// Override the model / deployment name for this call.
    #[arg(short = 'm', long = "model", global = true)]
    pub model: Option<String>,

    /// Image width in pixels. Adapter validates against its provider's allowed sizes.
    #[arg(short = 'W', long = "width", global = true)]
    pub width: Option<u32>,

    /// Image height in pixels. Adapter validates against its provider's allowed sizes.
    #[arg(short = 'H', long = "height", global = true)]
    pub height: Option<u32>,

    /// Override the provider endpoint URL.
    #[arg(long = "endpoint", global = true)]
    pub endpoint: Option<String>,

    /// Override the API version (where applicable).
    #[arg(long = "api-version", global = true)]
    pub api_version: Option<String>,

    /// HTTP timeout in seconds for a single generation call.
    #[arg(long = "timeout", global = true)]
    pub timeout: Option<u64>,

    /// Max attempts (1 + retries) for transient failures.
    #[arg(long = "max-attempts", global = true)]
    pub max_attempts: Option<u32>,

    /// Don't open the resulting image in the default viewer.
    #[arg(long = "no-open", global = true)]
    pub no_open: bool,

    /// Suppress progress on stderr (final path is still printed on stdout).
    #[arg(short = 'q', long = "quiet", global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write a starter config file at $XDG_CONFIG_HOME/pixforge/config.toml.
    Init {
        /// Overwrite an existing config file.
        #[arg(long = "force")]
        force: bool,
    },
    /// Print the resolved config file path and exit.
    ConfigPath,
    /// List all profiles in the current config file.
    Profiles,
    /// Print the resolved settings for a profile (api_key shown only as env source + status).
    Profile {
        #[command(subcommand)]
        action: ProfileCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Print resolved profile fields. The api_key is never printed; only the
    /// env var name and a status (set | empty | unset).
    Show {
        name: String,
    },
}
