mod cli;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use is_terminal::IsTerminal;
use std::io::Read;
use std::process::ExitCode;

use crate::cli::{Cli, Command, ProfileCommand};
use pixforge::config::{
    self, EnvKeyStatus, LoadedConfig, Profile, ProviderKind,
};
use pixforge::output;
use pixforge::providers::{self, ImageProvider, Request, Size};

const EXIT_OK: u8 = 0;
const EXIT_GENERIC: u8 = 1;
const EXIT_CONFIG: u8 = 2;

fn main() -> ExitCode {
    let args = Cli::parse();
    let quiet = args.quiet;
    match dispatch(args) {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(RunError::Config(e)) => {
            eprintln!("pixforge: {e:#}");
            ExitCode::from(EXIT_CONFIG)
        }
        Err(RunError::Other(e)) => {
            if quiet {
                eprintln!("pixforge: {e}");
            } else {
                eprintln!("pixforge: {e:#}");
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
        Some(Command::Profiles) => cmd_profiles().map_err(RunError::Config),
        Some(Command::Profile {
            action: ProfileCommand::Show { name },
        }) => cmd_profile_show(&name).map_err(RunError::Config),
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
    eprintln!(
        "Edit it to enable a profile and set api_key_env, then run:  \
         pixforge -p \"your prompt\""
    );
    Ok(())
}

fn cmd_profiles() -> Result<()> {
    let cfg = LoadedConfig::load_from_default_path()?;
    let default = cfg.default_profile.as_deref().unwrap_or("");
    println!("{:<20}  {:<14}  default", "name", "provider");
    println!("{:<20}  {:<14}  -------", "----", "--------");
    for (name, p) in &cfg.profiles {
        let mark = if name == default { "*" } else { " " };
        println!("{:<20}  {:<14}  {}", name, p.provider.id(), mark);
    }
    Ok(())
}

fn cmd_profile_show(name: &str) -> Result<()> {
    let cfg = LoadedConfig::load_from_default_path()?;
    let p = cfg.profiles.get(name).ok_or_else(|| {
        anyhow!(
            "profile {name:?} not found in {}. Available: {}",
            cfg.source_path.display(),
            cfg.profile_names().join(", ")
        )
    })?;
    println!("name         = {}", p.name);
    println!("provider     = {}", p.provider.id());
    println!("endpoint     = {}", p.endpoint);
    println!("model        = {}", p.model);
    if let Some(v) = &p.api_version {
        println!("api_version  = {v}");
    }
    println!("auth_style   = {:?}", p.auth_style);
    match (&p.api_key_env, p.env_key_status()) {
        (Some(var), status) => println!("api_key      = env ${var} ({})", status_word(&status)),
        (None, EnvKeyStatus::NotApplicable) => println!("api_key      = (none — auth disabled)"),
        (None, _) => println!("api_key      = (missing api_key_env)"),
    }
    println!("width        = {}", p.width);
    println!("height       = {}", p.height);
    println!("timeout_secs = {}", p.timeout_secs);
    println!("max_attempts = {}", p.max_attempts);
    Ok(())
}

fn status_word(s: &EnvKeyStatus) -> &'static str {
    match s {
        EnvKeyStatus::Set => "set",
        EnvKeyStatus::Empty => "empty",
        EnvKeyStatus::Unset => "unset",
        EnvKeyStatus::NotApplicable => "n/a",
    }
}

fn cmd_generate(args: Cli) -> Result<(), RunError> {
    let prompt = read_prompt(args.prompt.as_deref()).map_err(RunError::Other)?;

    let cfg = LoadedConfig::load_from_default_path().map_err(RunError::Config)?;
    let base = cfg
        .select(args.profile.as_deref())
        .map_err(RunError::Config)?
        .clone();
    let profile = apply_overrides(base, &args).map_err(RunError::Config)?;

    let size_explicit = args.width.is_some() || args.height.is_some();
    let size = Some(Size {
        width: profile.width,
        height: profile.height,
    });

    let out_path = match args.output {
        Some(p) => p,
        None => output::default_output_path(&prompt),
    };

    if !args.quiet {
        let dims = match size {
            Some(s) => s.as_string(),
            None => "default".to_string(),
        };
        eprintln!(
            "pixforge: generating {dims} via {} ({}/{}) → {}",
            profile.name,
            profile.provider.id(),
            profile.model,
            out_path.display()
        );
    }

    let provider = build_provider(&profile).map_err(RunError::Config)?;

    let req = Request {
        prompt: &prompt,
        model: &profile.model,
        n: 1,
        size,
        size_explicit,
        seed: None,
        negative_prompt: None,
        quality: None,
        extra: &serde_json::Map::new(),
    };

    let mut on_retry = |attempt: u32, msg: &str, wait: f64| {
        if !args.quiet {
            eprintln!("pixforge: retry attempt {attempt} ({msg}); sleeping {wait:.1}s");
        }
    };

    let result = provider
        .generate(&req, &mut on_retry)
        .with_context(|| "image generation failed")?;

    if result.images.is_empty() {
        return Err(RunError::Other(anyhow!("provider returned no images")));
    }
    let image = &result.images[0];

    output::write_image_atomic(&out_path, &image.bytes)
        .with_context(|| format!("writing image to {}", out_path.display()))?;
    let sidecar = output::write_sidecar(
        &out_path,
        &output::SidecarInput {
            prompt: &prompt,
            provider_id: provider.id(),
            profile_name: &profile.name,
            model: &profile.model,
            endpoint: &profile.endpoint,
            width: profile.width,
            height: profile.height,
            mime_type: &image.mime_type,
            revised_prompt: image.revised_prompt.as_deref(),
            latency_secs: result.latency_secs,
            attempts: result.attempts,
        },
    )
    .with_context(|| "writing sidecar")?;

    if !args.quiet {
        eprintln!(
            "pixforge: ok ({} attempts, {:.1}s, {} bytes)",
            result.attempts,
            result.latency_secs,
            image.bytes.len()
        );
        eprintln!("pixforge: sidecar {}", sidecar.display());
    }

    println!("{}", out_path.display());

    if !args.no_open {
        if let Err(e) = output::open_in_default_app(&out_path) {
            if !args.quiet {
                eprintln!("pixforge: warning: could not open viewer: {e}");
            }
        }
    }

    Ok(())
}

/// Apply CLI flag overrides on top of the selected profile.
fn apply_overrides(mut p: Profile, args: &Cli) -> Result<Profile> {
    if let Some(m) = &args.model {
        p.model = m.clone();
    }
    if let Some(e) = &args.endpoint {
        p.endpoint = e.trim_end_matches('/').to_string();
    }
    if let Some(v) = &args.api_version {
        p.api_version = Some(v.clone());
    }
    if let Some(w) = args.width {
        p.width = w;
    }
    if let Some(h) = args.height {
        p.height = h;
    }
    if let Some(t) = args.timeout {
        p.timeout_secs = t;
    }
    if let Some(a) = args.max_attempts {
        p.max_attempts = a;
    }
    Ok(p)
}

/// Construct a provider trait object for the given profile. Returns a
/// helpful "not implemented yet" error for adapters that haven't landed yet.
fn build_provider(profile: &Profile) -> Result<Box<dyn ImageProvider>> {
    match profile.provider {
        ProviderKind::AzureMai => {
            let api_key = profile.read_api_key()?.ok_or_else(|| {
                anyhow!("internal: azure-mai requires an api key but none resolved")
            })?;
            let api_version = profile
                .api_version
                .clone()
                .unwrap_or_else(|| "preview".to_string());
            Ok(Box::new(providers::azure_mai::AzureMaiProvider {
                endpoint: profile.endpoint.clone(),
                api_version,
                api_key,
                timeout_secs: profile.timeout_secs,
                max_attempts: profile.max_attempts,
            }))
        }
        ProviderKind::AzureOpenai => Err(anyhow!(
            "azure-openai adapter not implemented yet (planned for v0.2)"
        )),
        ProviderKind::OpenaiCompat => Err(anyhow!(
            "openai-compat adapter not implemented yet (planned for v0.2)"
        )),
        ProviderKind::Gemini => Err(anyhow!(
            "gemini adapter not implemented yet (planned for v0.2)"
        )),
    }
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
