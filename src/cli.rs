use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Fast CLI for generating images via Microsoft's MAI image models on Azure.
#[derive(Debug, Parser)]
#[command(name = "imagine", version, about, long_about = None)]
pub struct Cli {
    /// Prompt text. Use `-` to read the prompt from stdin (until EOF).
    #[arg(short = 'p', long = "prompt", global = true)]
    pub prompt: Option<String>,

    /// Output PNG path. Default: ./imagine-{YYYYMMDD-HHMMSS}-{hash6}.png
    #[arg(short = 'o', long = "output", global = true)]
    pub output: Option<PathBuf>,

    /// Override the deployment / model name (e.g. MAI-Image-2e).
    #[arg(short = 'm', long = "model", global = true)]
    pub model: Option<String>,

    /// Image width in pixels (must be >= 768; width*height <= 1,048,576).
    #[arg(short = 'W', long = "width", global = true)]
    pub width: Option<u32>,

    /// Image height in pixels (must be >= 768; width*height <= 1,048,576).
    #[arg(short = 'H', long = "height", global = true)]
    pub height: Option<u32>,

    /// Override the Azure endpoint base URL.
    #[arg(long = "endpoint", global = true)]
    pub endpoint: Option<String>,

    /// API version pinned in the request URL (default: preview).
    #[arg(long = "api-version", global = true)]
    pub api_version: Option<String>,

    /// HTTP timeout in seconds for a single generation call (default: 180).
    #[arg(long = "timeout", global = true)]
    pub timeout: Option<u64>,

    /// Max attempts (1 + retries) for transient failures (default: 5).
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
    /// Write a starter config file at $XDG_CONFIG_HOME/imagine/config.toml.
    Init {
        /// Overwrite an existing config file.
        #[arg(long = "force")]
        force: bool,
    },
    /// Print the resolved config file path and exit.
    ConfigPath,
}
