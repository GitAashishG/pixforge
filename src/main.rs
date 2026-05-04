mod cli;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use is_terminal::IsTerminal;
use std::io::Read;
use std::process::ExitCode;

use crate::cli::{Cli, Command, ProfileCommand};
use clap::CommandFactory;
use pixforge::config::{
    self, EnvKeyStatus, LoadedConfig, Profile, ProviderKind,
};
use pixforge::output;
use pixforge::providers::{self, ImageProvider, Request, Size};

const EXIT_OK: u8 = 0;
const EXIT_GENERIC: u8 = 1;
const EXIT_CONFIG: u8 = 2;

fn main() -> ExitCode {
    reset_sigpipe();
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
        Some(Command::Completions { shell }) => cmd_completions(shell).map_err(RunError::Other),
        Some(Command::Man) => cmd_man().map_err(RunError::Other),
        Some(Command::Setup { .. }) => cmd_setup(args).map_err(RunError::Other),
        Some(Command::AdvancedConfig) => cmd_advanced_config().map_err(RunError::Other),
        None => cmd_generate(args),
    }
}

fn cmd_advanced_config() -> Result<()> {
    let path = pixforge::config::config_path()?;
    match std::env::var("EDITOR") {
        Ok(editor) if !editor.trim().is_empty() => {
            eprintln!("opening {} in {}…", path.display(), editor);
            let status = std::process::Command::new(&editor)
                .arg(&path)
                .status()
                .with_context(|| format!("launching {editor}"))?;
            if !status.success() {
                anyhow::bail!("{editor} exited with status {status}");
            }
            Ok(())
        }
        _ => {
            println!("{}", path.display());
            eprintln!("(set $EDITOR to open this file directly next time.)");
            Ok(())
        }
    }
}

fn cmd_setup(args: Cli) -> Result<()> {
    use is_terminal::IsTerminal;
    let Some(Command::Setup {
        non_interactive,
        provider,
        endpoint,
        model,
        api_version,
        api_key_env,
        auth_style,
        dialect,
        profile_name,
        set_default,
    }) = args.command
    else {
        unreachable!("dispatch shape mismatch")
    };

    if non_interactive {
        return run_setup_non_interactive(
            provider,
            endpoint,
            model,
            api_version,
            api_key_env,
            auth_style,
            dialect,
            profile_name,
            set_default,
        );
    }

    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        anyhow::bail!(
            "interactive setup requires a TTY on both stdin and stderr.\n\
             Use `pixforge setup --non-interactive --provider <id> --set-endpoint <url> ...` \
             for scripts/CI."
        );
    }

    let path = pixforge::config::config_path()?;
    let store = pixforge::setup::io::FsConfigStore::new(path);
    let shell_rc = pixforge::setup::io::FsShellRcWriter::new();
    let mut tester = pixforge::setup::io::LiveConnectionTester {
        builder: Box::new(|p| build_provider(p)),
    };
    let mut prompter = pixforge::setup::io::DialoguerPrompter::default();
    let mut deps = pixforge::setup::WizardDeps {
        prompter: &mut prompter,
        config: &store,
        shell_rc: &shell_rc,
        tester: &mut tester,
        env: pixforge::setup::EnvProbe::from_real_env(),
    };
    let res = pixforge::setup::run(&mut deps).context("setup wizard failed")?;
    eprintln!();
    eprintln!("✓ saved profile {:?} to {}", res.profile_name, res.config_path_str);
    if res.set_as_default {
        eprintln!("  (set as default; just run `pixforge -p \"…\"`)");
    } else {
        eprintln!(
            "  (use `pixforge --profile {} -p \"…\"`)",
            res.profile_name
        );
    }
    Ok(())
}

fn run_setup_non_interactive(
    provider: Option<String>,
    endpoint: Option<String>,
    model: Option<String>,
    api_version: Option<String>,
    api_key_env: Option<String>,
    auth_style: Option<String>,
    dialect: Option<String>,
    profile_name: Option<String>,
    set_default: bool,
) -> Result<()> {
    let provider = provider.context("--non-interactive requires --provider")?;
    let model = model.context("--non-interactive requires --set-model")?;
    let api_key_env = api_key_env.context("--non-interactive requires --set-api-key-env")?;

    let draft = pixforge::setup::config_edit::ProfileDraft {
        provider: provider.clone(),
        endpoint: endpoint.map(|s| s.trim_end_matches('/').to_string()),
        model,
        api_version,
        api_key_env,
        auth_style,
        dialect,
    };
    draft.validate().context("validation failed")?;

    let path = pixforge::config::config_path()?;
    let store = pixforge::setup::io::FsConfigStore::new(path.clone());
    let existing_text = pixforge::setup::ConfigStore::read(&store)?;
    let mut existing = match existing_text {
        Some(t) => pixforge::setup::config_edit::EditableConfig::parse(&t)?,
        None => pixforge::setup::config_edit::EditableConfig::empty(),
    };
    let name = profile_name.unwrap_or_else(|| provider.clone());
    if existing.has_profile(&name) {
        anyhow::bail!(
            "profile {name:?} already exists; non-interactive setup refuses to overwrite \
             without explicit confirmation. Edit the file or pick a different --profile-name."
        );
    }
    existing.upsert_profile(&name, &draft)?;
    if set_default || existing.current_default_profile().is_none() {
        existing.set_default_profile(&name);
    }
    pixforge::setup::ConfigStore::write(&store, &existing.to_string())?;
    eprintln!("wrote profile {:?} to {}", name, path.display());
    Ok(())
}

fn cmd_completions(shell: clap_complete::Shell) -> Result<()> {
    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
    Ok(())
}

fn cmd_man() -> Result<()> {
    let cmd = Cli::command();
    let man = clap_mangen::Man::new(cmd);
    man.render(&mut std::io::stdout())
        .context("rendering man page")?;
    Ok(())
}

/// Restore the default SIGPIPE behavior on Unix so that piping into
/// commands like `head` terminates pixforge silently instead of panicking
/// when stdout is closed early. No-op on Windows.
#[cfg(unix)]
fn reset_sigpipe() {
    // SAFETY: setting a signal handler from the main thread before any
    // other threads are spawned is sound. SIG_DFL for SIGPIPE causes the
    // process to terminate quietly when writing to a closed pipe.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn reset_sigpipe() {}

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
        quality: args.quality.as_deref(),
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
        ProviderKind::AzureOpenai => {
            let api_key = profile.read_api_key()?.ok_or_else(|| {
                anyhow!("internal: azure-openai requires an api key but none resolved")
            })?;
            let dialect = match profile.azure_openai_dialect {
                config::AzureOpenaiDialect::Deployment => {
                    providers::azure_openai::Dialect::Deployment
                }
                config::AzureOpenaiDialect::V1 => providers::azure_openai::Dialect::V1,
            };
            // api_version is required only for the Deployment dialect; V1 ignores it.
            let api_version = profile.api_version.clone().unwrap_or_default();
            Ok(Box::new(providers::azure_openai::AzureOpenaiProvider {
                endpoint: profile.endpoint.clone(),
                api_version,
                api_key,
                dialect,
                timeout_secs: profile.timeout_secs,
                max_attempts: profile.max_attempts,
            }))
        }
        ProviderKind::OpenaiCompat => {
            let api_key = profile.read_api_key()?;
            let auth_style = match profile.auth_style {
                config::AuthStyle::Bearer => providers::openai_compat::AuthStyle::Bearer,
                config::AuthStyle::ApiKey => providers::openai_compat::AuthStyle::ApiKey,
                config::AuthStyle::None => providers::openai_compat::AuthStyle::None,
            };
            Ok(Box::new(providers::openai_compat::OpenaiCompatProvider {
                endpoint: profile.endpoint.clone(),
                api_key,
                auth_style,
                timeout_secs: profile.timeout_secs,
                max_attempts: profile.max_attempts,
            }))
        }
        ProviderKind::Gemini => {
            let api_key = profile.read_api_key()?.ok_or_else(|| {
                anyhow!("internal: gemini requires an api key but none resolved")
            })?;
            Ok(Box::new(providers::gemini::GeminiProvider {
                endpoint: profile.endpoint.clone(),
                api_key,
                timeout_secs: profile.timeout_secs,
                max_attempts: profile.max_attempts,
            }))
        }
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
