mod cli;
mod client;
mod config;
mod output;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use is_terminal::IsTerminal;
use std::io::Read;
use std::process::ExitCode;

use crate::cli::{Cli, Command};
use crate::client::AzureImageClient;
use crate::config::CliOverrides;

const EXIT_OK: u8 = 0;
const EXIT_GENERIC: u8 = 1;
const EXIT_CONFIG: u8 = 2;

fn main() -> ExitCode {
    let args = Cli::parse();
    let quiet = args.quiet;
    match dispatch(args) {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(RunError::Config(e)) => {
            eprintln!("imagine: {e:#}");
            ExitCode::from(EXIT_CONFIG)
        }
        Err(RunError::Other(e)) => {
            if quiet {
                eprintln!("imagine: {e}");
            } else {
                eprintln!("imagine: {e:#}");
            }
            ExitCode::from(EXIT_GENERIC)
        }
    }
}

enum RunError {
    Config(anyhow::Error),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RunError {
    fn from(e: anyhow::Error) -> Self {
        RunError::Other(e)
    }
}

fn dispatch(args: Cli) -> Result<(), RunError> {
    match args.command {
        Some(Command::Init { force }) => cmd_init(force).map_err(RunError::Other),
        Some(Command::ConfigPath) => cmd_config_path().map_err(RunError::Other),
        None => cmd_generate(args),
    }
}

fn cmd_config_path() -> Result<()> {
    let path = config::config_path()?;
    println!("{}", path.display());
    Ok(())
}

fn cmd_init(force: bool) -> Result<()> {
    let path = config::write_starter_config(force)?;
    eprintln!("wrote starter config to {}", path.display());
    eprintln!("Edit it to set your Azure api_key, then run:  imagine -p \"your prompt\"");
    Ok(())
}

fn cmd_generate(args: Cli) -> Result<(), RunError> {
    let prompt = read_prompt(args.prompt.as_deref()).map_err(RunError::Other)?;

    let cfg = config::load(CliOverrides {
        endpoint: args.endpoint,
        deployment: args.model,
        api_version: args.api_version,
        width: args.width,
        height: args.height,
        timeout_secs: args.timeout,
        max_attempts: args.max_attempts,
    })
    .map_err(RunError::Config)?;

    let out_path = match args.output {
        Some(p) => p,
        None => output::default_output_path(&prompt),
    };

    let url = cfg.url();
    let dep = cfg.deployment.clone();
    let dims = format!("{}x{}", cfg.width, cfg.height);

    if !args.quiet {
        eprintln!(
            "imagine: generating {dims} via {dep} → {}",
            out_path.display()
        );
    }

    let client = AzureImageClient::new(cfg);
    let result = client
        .generate(&prompt, |attempt, msg, wait| {
            if !args.quiet {
                eprintln!("imagine: retry attempt {attempt} ({msg}); sleeping {wait:.1}s");
            }
        })
        .with_context(|| "image generation failed")?;

    output::write_image_atomic(&out_path, &result.image_bytes)
        .with_context(|| format!("writing image to {}", out_path.display()))?;
    let sidecar = output::write_sidecar(&out_path, &prompt, &url, &result)
        .with_context(|| "writing sidecar")?;

    if !args.quiet {
        eprintln!(
            "imagine: ok ({} attempts, {:.1}s, {} bytes)",
            result.attempts,
            result.latency_secs,
            result.image_bytes.len()
        );
        eprintln!("imagine: sidecar {}", sidecar.display());
    }

    println!("{}", out_path.display());

    if !args.no_open {
        if let Err(e) = output::open_in_default_app(&out_path) {
            if !args.quiet {
                eprintln!("imagine: warning: could not open viewer: {e}");
            }
        }
    }

    Ok(())
}

fn read_prompt(arg: Option<&str>) -> Result<String> {
    match arg {
        None => Err(anyhow!(
            "missing --prompt (use -p \"your prompt\" or pass `-` to read from stdin)"
        )),
        Some("-") => {
            if std::io::stdin().is_terminal() {
                return Err(anyhow!(
                    "no stdin attached but `-p -` was given; type a prompt or pipe it in"
                ));
            }
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading prompt from stdin")?;
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                return Err(anyhow!("stdin produced an empty prompt"));
            }
            Ok(trimmed.to_string())
        }
        Some(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err(anyhow!("--prompt is empty"));
            }
            Ok(trimmed.to_string())
        }
    }
}
