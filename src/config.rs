use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::env;
use std::path::PathBuf;

pub const DEFAULT_API_VERSION: &str = "preview";
pub const DEFAULT_WIDTH: u32 = 1024;
pub const DEFAULT_HEIGHT: u32 = 1024;
pub const DEFAULT_TIMEOUT_SECS: u64 = 180;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// Resolved runtime configuration after layering defaults < file < env < CLI flags.
#[derive(Debug, Clone)]
pub struct Config {
    pub endpoint: String,
    pub api_key: String,
    pub deployment: String,
    pub api_version: String,
    pub width: u32,
    pub height: u32,
    pub timeout_secs: u64,
    pub max_attempts: u32,
}

impl Config {
    pub fn url(&self) -> String {
        format!(
            "{}/mai/v1/images/generations?api-version={}",
            self.endpoint.trim_end_matches('/'),
            self.api_version
        )
    }
}

/// On-disk config file shape. Every field is optional.
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    endpoint: Option<String>,
    api_key: Option<String>,
    deployment: Option<String>,
    api_version: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    timeout_secs: Option<u64>,
    max_attempts: Option<u32>,
}

/// Resolve `$XDG_CONFIG_HOME/pixforge/config.toml`, defaulting to
/// `~/.config/pixforge/config.toml`. Same path on every OS for predictability.
pub fn config_path() -> Result<PathBuf> {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("pixforge").join("config.toml"));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".config").join("pixforge").join("config.toml"))
}

fn load_file_config() -> Result<FileConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config file {}", path.display()))?;
    let cfg: FileConfig = toml::from_str(&text)
        .with_context(|| format!("parsing config file {}", path.display()))?;
    Ok(cfg)
}

/// Optional CLI overrides. Each Some() wins over env and file.
#[derive(Debug, Default)]
pub struct CliOverrides {
    pub endpoint: Option<String>,
    pub deployment: Option<String>,
    pub api_version: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub timeout_secs: Option<u64>,
    pub max_attempts: Option<u32>,
}

/// Build the final Config. Returns Err with a friendly message if API key is missing.
pub fn load(overrides: CliOverrides) -> Result<Config> {
    let file = load_file_config()?;

    let endpoint = overrides
        .endpoint
        .or_else(|| env::var("MAI_ENDPOINT").ok())
        .or(file.endpoint)
        .ok_or_else(|| missing_field_error("endpoint", "MAI_ENDPOINT"))?;

    let deployment = overrides
        .deployment
        .or_else(|| env::var("MAI_DEPLOYMENT").ok())
        .or(file.deployment)
        .ok_or_else(|| missing_field_error("deployment", "MAI_DEPLOYMENT"))?;

    let api_version = overrides
        .api_version
        .or_else(|| env::var("MAI_API_VERSION").ok())
        .or(file.api_version)
        .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());

    let width = overrides.width.or(file.width).unwrap_or(DEFAULT_WIDTH);
    let height = overrides.height.or(file.height).unwrap_or(DEFAULT_HEIGHT);
    let timeout_secs = overrides
        .timeout_secs
        .or(file.timeout_secs)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let max_attempts = overrides
        .max_attempts
        .or(file.max_attempts)
        .unwrap_or(DEFAULT_MAX_ATTEMPTS);

    let api_key = env::var("AZURE_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or(file.api_key)
        .ok_or_else(missing_api_key_error)?;

    Ok(Config {
        endpoint,
        api_key,
        deployment,
        api_version,
        width,
        height,
        timeout_secs,
        max_attempts,
    })
}

fn missing_api_key_error() -> anyhow::Error {
    let path = config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    anyhow!(
        "no Azure API key found.\n\n\
         Set AZURE_API_KEY in your environment, or create a config file at:\n  \
         {path}\n\n\
         Minimal config.toml:\n\
         {EXAMPLE_CONFIG}\n\
         Run `pixforge init` to scaffold it."
    )
}

fn missing_field_error(field: &str, env_var: &str) -> anyhow::Error {
    let path = config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    let flag = field.replace('_', "-");
    anyhow!(
        "missing required `{field}`.\n\n\
         Set the `{env_var}` environment variable, pass `--{flag}` on the command line,\n\
         or add `{field} = \"...\"` to your config file:\n  \
         {path}\n\n\
         Run `pixforge init` to scaffold a config template."
    )
}

const EXAMPLE_CONFIG: &str = r#"
  endpoint   = "https://your-resource.services.ai.azure.com"
  deployment = "your-deployment-name"
  api_key    = "your-azure-api-key"
  # width    = 1024
  # height   = 1024
"#;

/// Write a starter config to `config_path()`. Errors if it already exists unless `force`.
pub fn write_starter_config(force: bool) -> Result<PathBuf> {
    let path = config_path()?;
    if path.exists() && !force {
        return Err(anyhow!(
            "config already exists at {} (use --force to overwrite)",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let body = format!(
        "# pixforge config\n\
         # Required: endpoint, deployment, api_key (or set them via env vars / CLI flags).\n\
         endpoint   = \"https://your-resource.services.ai.azure.com\"\n\
         deployment = \"your-deployment-name\"\n\
         api_key    = \"REPLACE-ME\"\n\
         # api_version  = \"{DEFAULT_API_VERSION}\"\n\
         # width        = {DEFAULT_WIDTH}\n\
         # height       = {DEFAULT_HEIGHT}\n\
         # timeout_secs = {DEFAULT_TIMEOUT_SECS}\n\
         # max_attempts = {DEFAULT_MAX_ATTEMPTS}\n"
    );
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}
