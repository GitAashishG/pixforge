//! Profile-based configuration for pixforge.
//!
//! Layered resolution order: **CLI flags > env vars > selected profile in
//! config.toml > built-in defaults**.
//!
//! API keys are *never* read from `config.toml` directly. Profiles point at
//! an environment variable name (`api_key_env = "AZURE_API_KEY"`) and the
//! actual secret is read at request time. Setting `api_key = "..."` literally
//! is a hard load error — this keeps committed configs safe by construction.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

pub const DEFAULT_API_VERSION_AZURE_MAI: &str = "preview";
pub const DEFAULT_WIDTH: u32 = 1024;
pub const DEFAULT_HEIGHT: u32 = 1024;
pub const DEFAULT_TIMEOUT_SECS: u64 = 180;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;
pub const DEFAULT_GEMINI_ENDPOINT: &str = "https://generativelanguage.googleapis.com";

/// Resolve `$XDG_CONFIG_HOME/pixforge/config.toml`, defaulting to
/// `~/.config/pixforge/config.toml` on every OS for predictability.
pub fn config_path() -> Result<PathBuf> {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("pixforge").join("config.toml"));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".config").join("pixforge").join("config.toml"))
}

// ---------------------------------------------------------------------------
// On-disk schema
// ---------------------------------------------------------------------------

/// Raw `config.toml` shape. `serde_deny_unknown_fields` is intentionally
/// *off* at the top level so future additions don't break older binaries,
/// but we deny it inside each profile (see `RawProfile`) to catch typos.
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    default_profile: Option<String>,
    #[serde(default)]
    profile: BTreeMap<String, RawProfile>,
}

/// One `[profile.X]` block. Provider-specific required fields are validated
/// after parsing — this struct just collects whatever the user wrote and
/// rejects the literal `api_key` key entirely (security hard rule).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    provider: Option<String>,
    endpoint: Option<String>,
    model: Option<String>,
    api_version: Option<String>,
    api_key_env: Option<String>,
    auth_style: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    timeout_secs: Option<u64>,
    max_attempts: Option<u32>,

    /// Only meaningful for `provider = "azure-openai"`. Values: `deployment`
    /// (default, for DALL·E 2/3) or `v1` (required for gpt-image-1, gpt-image-2).
    dialect: Option<String>,

    /// Hard-rejected at parse time. We *accept* the field in the schema only
    /// so we can produce a precise error message instead of a generic
    /// "unknown field". See [`RawConfig::validate`].
    api_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Resolved profile (post-validation, ready to hand to a provider adapter)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    AzureMai,
    AzureOpenai,
    OpenaiCompat,
    Gemini,
}

impl ProviderKind {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "azure-mai" => Ok(Self::AzureMai),
            "azure-openai" => Ok(Self::AzureOpenai),
            "openai-compat" => Ok(Self::OpenaiCompat),
            "gemini" => Ok(Self::Gemini),
            other => bail!(
                "unknown provider {other:?}. Valid: azure-mai, azure-openai, openai-compat, gemini"
            ),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::AzureMai => "azure-mai",
            Self::AzureOpenai => "azure-openai",
            Self::OpenaiCompat => "openai-compat",
            Self::Gemini => "gemini",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>` — default for `openai-compat`.
    Bearer,
    /// `api-key: <key>` — used by Azure variants and some compat shims.
    ApiKey,
    /// No auth header. Allowed only when `provider = "openai-compat"`,
    /// for use with local servers like LocalAI.
    None,
}

impl AuthStyle {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "bearer" => Ok(Self::Bearer),
            "api-key" => Ok(Self::ApiKey),
            "none" => Ok(Self::None),
            other => bail!("unknown auth_style {other:?}. Valid: bearer, api-key, none"),
        }
    }
}

/// Which Azure OpenAI URL dialect to use. Only consulted for
/// `provider = "azure-openai"` profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AzureOpenaiDialect {
    /// Legacy `/openai/deployments/{name}/images/generations?api-version=...`
    /// for DALL·E 2 / DALL·E 3.
    Deployment,
    /// Modern `/openai/v1/images/generations` for gpt-image-1, gpt-image-2,
    /// and any model released after Aug 2025.
    V1,
}

impl AzureOpenaiDialect {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "deployment" => Ok(Self::Deployment),
            "v1" => Ok(Self::V1),
            other => bail!(
                "unknown dialect {other:?} for provider \"azure-openai\". Valid: deployment, v1"
            ),
        }
    }
}

/// A fully validated profile. Everything an adapter needs is already
/// resolved to native Rust types.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub provider: ProviderKind,
    pub endpoint: String,
    pub model: String,
    pub api_version: Option<String>,
    pub api_key_env: Option<String>,
    pub auth_style: AuthStyle,
    /// Only meaningful when `provider == AzureOpenai`. Defaults to `Deployment`.
    pub azure_openai_dialect: AzureOpenaiDialect,
    pub width: u32,
    pub height: u32,
    pub timeout_secs: u64,
    pub max_attempts: u32,
}

impl Profile {
    /// Read the API key from the env var named in `api_key_env`. Errors
    /// distinguish unset / empty so the user knows what to fix.
    /// Returns `Ok(None)` only when `auth_style == None`.
    ///
    /// Defensive: if the env var name is over 32 chars (already validated at
    /// config-parse time, but belt-and-suspenders), redact it in error
    /// messages so a bypassed validation can't leak a pasted secret.
    #[allow(dead_code)] // consumed by adapters in the next commits
    pub fn read_api_key(&self) -> Result<Option<String>> {
        if matches!(self.auth_style, AuthStyle::None) {
            return Ok(None);
        }
        let var = self.api_key_env.as_ref().ok_or_else(|| {
            anyhow!(
                "profile {:?}: api_key_env must be set when auth_style is {:?}",
                self.name,
                self.auth_style
            )
        })?;
        let display = redact_if_suspicious(var);
        match env::var(var) {
            Err(env::VarError::NotPresent) => Err(anyhow!(
                "profile {:?}: ${display} is not set (required by api_key_env)",
                self.name
            )),
            Err(env::VarError::NotUnicode(_)) => Err(anyhow!(
                "profile {:?}: ${display} contains non-UTF8 bytes",
                self.name
            )),
            Ok(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    Err(anyhow!(
                        "profile {:?}: ${display} is set but empty",
                        self.name
                    ))
                } else {
                    Ok(Some(trimmed.to_string()))
                }
            }
        }
    }
}

/// Status of an env-var-backed credential. Used by `pixforge profile show`.
#[derive(Debug)]
pub enum EnvKeyStatus {
    Set,
    Empty,
    Unset,
    NotApplicable,
}

impl Profile {
    pub fn env_key_status(&self) -> EnvKeyStatus {
        if matches!(self.auth_style, AuthStyle::None) {
            return EnvKeyStatus::NotApplicable;
        }
        let Some(var) = self.api_key_env.as_deref() else {
            return EnvKeyStatus::Unset;
        };
        match env::var(var) {
            Err(_) => EnvKeyStatus::Unset,
            Ok(s) if s.trim().is_empty() => EnvKeyStatus::Empty,
            Ok(_) => EnvKeyStatus::Set,
        }
    }
}

// ---------------------------------------------------------------------------
// Loaded config (set of profiles + selection)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub default_profile: Option<String>,
    pub profiles: BTreeMap<String, Profile>,
    pub source_path: PathBuf,
}

impl LoadedConfig {
    pub fn load_from_default_path() -> Result<Self> {
        let path = config_path()?;
        Self::load_from(path)
    }

    pub fn load_from(path: PathBuf) -> Result<Self> {
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "reading config file {}\n\nRun `pixforge init` to scaffold one.",
                path.display()
            )
        })?;
        Self::parse(&text, path)
    }

    pub fn parse(text: &str, source_path: PathBuf) -> Result<Self> {
        let raw: RawConfig = toml::from_str(text)
            .with_context(|| format!("parsing TOML from {}", source_path.display()))?;
        let mut profiles = BTreeMap::new();
        for (name, raw_profile) in raw.profile {
            let p = resolve_profile(name.clone(), raw_profile)
                .with_context(|| format!("in profile [{name}]"))?;
            profiles.insert(name, p);
        }
        if profiles.is_empty() {
            let commented = find_commented_profiles(text);
            if !commented.is_empty() {
                bail!(
                    "{} contains no enabled profiles, but does contain commented-out \
                     profile blocks: {}.\n\n\
                     Remove the leading `# ` from one of those `[profile.X]` blocks \
                     (and its lines) to enable it.",
                    source_path.display(),
                    commented.join(", ")
                );
            }
            bail!(
                "{} contains no profiles. Add at least one [profile.X] section, \
                 or run `pixforge init --force` to overwrite with the starter template.",
                source_path.display()
            );
        }
        Ok(Self {
            default_profile: raw.default_profile,
            profiles,
            source_path,
        })
    }

    /// Pick a profile by the precedence: CLI flag > env > default_profile >
    /// the only profile (if there is exactly one).
    pub fn select(&self, cli_choice: Option<&str>) -> Result<&Profile> {
        let chosen = cli_choice
            .map(str::to_string)
            .or_else(|| env::var("PIXFORGE_PROFILE").ok().filter(|s| !s.is_empty()))
            .or_else(|| self.default_profile.clone())
            .or_else(|| {
                if self.profiles.len() == 1 {
                    self.profiles.keys().next().cloned()
                } else {
                    None
                }
            });

        match chosen {
            Some(name) => self.profiles.get(&name).ok_or_else(|| {
                anyhow!(
                    "profile {name:?} not found in {}. Available: {}",
                    self.source_path.display(),
                    self.profile_names().join(", ")
                )
            }),
            None => Err(anyhow!(
                "no profile selected and no default_profile in {}. \
                 Pass --profile <name> or set default_profile = \"...\". \
                 Available: {}",
                self.source_path.display(),
                self.profile_names().join(", ")
            )),
        }
    }

    pub fn profile_names(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }
}

fn resolve_profile(name: String, raw: RawProfile) -> Result<Profile> {
    if raw.api_key.is_some() {
        bail!(
            "literal `api_key = \"...\"` is not allowed in config.toml for safety. \
             Use `api_key_env = \"YOUR_VAR_NAME\"` and export YOUR_VAR_NAME."
        );
    }

    let provider = ProviderKind::parse(
        raw.provider
            .as_deref()
            .ok_or_else(|| anyhow!("`provider` is required"))?,
    )?;

    let auth_style = match raw.auth_style.as_deref() {
        Some(s) => AuthStyle::parse(s)?,
        None => default_auth_style(provider),
    };

    let api_key_env = raw.api_key_env.clone();
    if let Some(name) = &api_key_env {
        validate_env_var_name(name)?;
    }
    if !matches!(auth_style, AuthStyle::None) && api_key_env.is_none() {
        bail!(
            "`api_key_env` is required (auth_style = {:?}). \
             Add api_key_env = \"YOUR_VAR_NAME\" or set auth_style = \"none\".",
            auth_style
        );
    }
    if matches!(auth_style, AuthStyle::None) && !matches!(provider, ProviderKind::OpenaiCompat) {
        bail!(
            "auth_style = \"none\" is only valid for provider = \"openai-compat\" \
             (e.g. local LocalAI servers)."
        );
    }

    let model = raw
        .model
        .clone()
        .ok_or_else(|| anyhow!("`model` is required"))?;

    let endpoint = match (provider, raw.endpoint.clone()) {
        (ProviderKind::Gemini, None) => DEFAULT_GEMINI_ENDPOINT.to_string(),
        (_, None) => bail!("`endpoint` is required for provider {:?}", provider.id()),
        (_, Some(e)) => {
            let trimmed = e.trim_end_matches('/').to_string();
            check_endpoint_is_base(&trimmed, provider)?;
            trimmed
        }
    };

    let azure_openai_dialect = match (provider, raw.dialect.as_deref()) {
        (ProviderKind::AzureOpenai, Some(s)) => AzureOpenaiDialect::parse(s)?,
        (ProviderKind::AzureOpenai, None) => AzureOpenaiDialect::Deployment,
        (_, Some(_)) => bail!("`dialect` is only valid for provider = \"azure-openai\""),
        (_, None) => AzureOpenaiDialect::Deployment, // ignored for non-azure-openai
    };

    let api_version = match (provider, azure_openai_dialect, raw.api_version.clone()) {
        (ProviderKind::AzureMai, _, None) => Some(DEFAULT_API_VERSION_AZURE_MAI.to_string()),
        (ProviderKind::AzureOpenai, AzureOpenaiDialect::Deployment, None) => bail!(
            "`api_version` is required for provider \"azure-openai\" with the default `deployment` dialect (e.g. api_version = \"2024-02-01\"). \
             Or set `dialect = \"v1\"` for gpt-image-1, gpt-image-2, and other newer models — those don't use api_version."
        ),
        (ProviderKind::AzureOpenai, AzureOpenaiDialect::V1, _) => None,
        (_, _, v) => v,
    };

    Ok(Profile {
        name,
        provider,
        endpoint,
        model,
        api_version,
        api_key_env,
        auth_style,
        azure_openai_dialect,
        width: raw.width.unwrap_or(DEFAULT_WIDTH),
        height: raw.height.unwrap_or(DEFAULT_HEIGHT),
        timeout_secs: raw.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        max_attempts: raw.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS),
    })
}

fn default_auth_style(provider: ProviderKind) -> AuthStyle {
    match provider {
        ProviderKind::AzureMai | ProviderKind::AzureOpenai => AuthStyle::ApiKey,
        ProviderKind::OpenaiCompat => AuthStyle::Bearer,
        ProviderKind::Gemini => AuthStyle::ApiKey, // x-goog-api-key style; adapter handles header name
    }
}

/// If the env var name is suspiciously long or contains characters not in the
/// POSIX env-var character set, redact it to `<redacted N chars>` so that we
/// never echo a pasted secret in error messages. Validation at config-parse
/// time is the primary defense; this is a runtime backstop.
fn redact_if_suspicious(name: &str) -> String {
    let len = name.len();
    let looks_safe = !name.is_empty()
        && len <= 32
        && name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if looks_safe {
        name.to_string()
    } else {
        format!("<redacted-{len}-chars>")
    }
}


/// Reject `api_key_env` values that don't look like POSIX environment variable
/// names. This catches the common mistake of pasting an actual secret as the
/// `api_key_env` value (a long alphanumeric token), which would otherwise
/// leak the secret into error messages when the lookup fails.
///
/// IMPORTANT: the returned error must NEVER contain the offending value.
/// We only report length and that it failed validation.
fn validate_env_var_name(s: &str) -> Result<()> {
    let len = s.len();
    if s.is_empty() {
        bail!(
            "`api_key_env` is an empty string. It must be the NAME of an \
             environment variable (e.g. AZURE_API_KEY)."
        );
    }
    let valid = s
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        bail!(
            "`api_key_env` value (length {len}) does not look like an environment \
             variable name. It must match `[A-Za-z_][A-Za-z0-9_]*`.\n\n\
             Did you paste the secret value here by mistake? \
             `api_key_env` is the NAME of an env var (e.g. `api_key_env = \"AZURE_API_KEY\"`); \
             the secret value goes in the env var itself \
             (e.g. `export AZURE_API_KEY=\"<the-secret>\"`).\n\n\
             If you did paste a real secret, treat it as compromised: rotate the key now \
             with your provider, then clear your shell history and terminal scrollback."
        );
    }
    if len > 32 {
        bail!(
            "`api_key_env` value is suspiciously long (length {len}). \
             It must be the NAME of an env var, not the secret itself. \
             POSIX env var names are typically under 32 characters.\n\n\
             Did you paste the secret value here by mistake? \
             `api_key_env` is the NAME of an env var (e.g. `api_key_env = \"AZURE_API_KEY\"`); \
             the secret value goes in the env var itself \
             (e.g. `export AZURE_API_KEY=\"<the-secret>\"`).\n\n\
             If you did paste a real secret, treat it as compromised: rotate the key now \
             with your provider, then clear your shell history and terminal scrollback."
        );
    }
    Ok(())
}


/// or a query string. We construct the path ourselves; if the user pasted
/// the full URL from a quickstart doc, that would produce a doubled-up URL
/// like `.../mai/v1/.../mai/v1/...` and a confusing 404.
///
/// Heuristic and intentionally not exhaustive — the goal is to catch the
/// obvious copy-paste mistakes, not validate URLs in general.
fn check_endpoint_is_base(endpoint: &str, provider: ProviderKind) -> Result<()> {
    if endpoint.contains('?') {
        bail!(
            "endpoint contains a query string ({:?}). It should be just the base URL \
             (e.g. \"https://your-resource.services.ai.azure.com\") — pixforge appends \
             the path and api-version itself.",
            endpoint
        );
    }
    let lower = endpoint.to_ascii_lowercase();
    let bad_paths: &[&str] = match provider {
        ProviderKind::AzureMai => &["/mai/v1/", "/images/generations"],
        ProviderKind::AzureOpenai => &[
            "/openai/deployments/",
            "/openai/v1/",
            "/images/generations",
        ],
        ProviderKind::OpenaiCompat => &["/images/generations"],
        ProviderKind::Gemini => &[":generatecontent", "/v1beta/models/"],
    };
    for path in bad_paths {
        if lower.contains(path) {
            bail!(
                "endpoint includes the API path {:?}. It should be just the base URL \
                 (e.g. for azure-mai: \"https://your-resource.services.ai.azure.com\"); \
                 pixforge appends the rest itself. Got: {:?}",
                path,
                endpoint
            );
        }
    }
    Ok(())
}

/// Scan `text` for lines that look like commented-out `[profile.X]` headers,
/// returning the profile names found. Used to produce a more helpful error
/// when the parsed config has zero active profiles.
fn find_commented_profiles(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        let after_hash = trimmed.trim_start_matches('#').trim_start();
        if let Some(rest) = after_hash.strip_prefix("[profile.") {
            if let Some(end) = rest.find(']') {
                names.push(rest[..end].to_string());
            }
        }
    }
    names
}

// ---------------------------------------------------------------------------
// `pixforge init` template
// ---------------------------------------------------------------------------

pub const STARTER_CONFIG: &str = r#"# pixforge config — see https://github.com/GitAashishG/pixforge
#
# Pick a profile to use by default; override per call with `--profile <name>`.
default_profile = "openai"

# ---------- OpenAI (active by default) ----------
# Edit `model` if needed, then export OPENAI_API_KEY in your shell:
#   export OPENAI_API_KEY="sk-..."
[profile.openai]
provider     = "openai-compat"
endpoint     = "https://api.openai.com/v1"
model        = "gpt-image-1"
api_key_env  = "OPENAI_API_KEY"
# auth_style = "bearer"   # default

# ---------- Other providers (uncomment a block to enable it) ----------
#
# Tip: a profile is "active" only when its `[profile.X]` line and all the
# lines below it do NOT start with `#`. The other examples below are
# commented out by default. To use one, set `default_profile` above to its
# name and remove the leading `# ` from each line of its block.

# ---------- Azure MAI (Microsoft AI image models on Azure) ----------
# [profile.azure-mai]
# provider     = "azure-mai"
# endpoint     = "https://your-resource.services.ai.azure.com"
# model        = "MAI-Image-2"
# api_key_env  = "AZURE_API_KEY"
# api_version  = "preview"

# ---------- Azure OpenAI: DALL·E (legacy `deployment` dialect) ----------
# [profile.azure-openai-dalle]
# provider     = "azure-openai"
# endpoint     = "https://your-resource.openai.azure.com"
# model        = "dall-e-3"          # deployment name in your Azure resource
# api_version  = "2024-02-01"
# api_key_env  = "AZURE_OPENAI_API_KEY"
# # dialect    = "deployment"        # default

# ---------- Azure OpenAI: gpt-image-1 / gpt-image-2 (modern `v1` dialect) ----------
# [profile.azure-openai]
# provider     = "azure-openai"
# endpoint     = "https://your-resource.openai.azure.com"
# model        = "gpt-image-2"       # deployment name in your Azure resource
# api_key_env  = "AZURE_OPENAI_API_KEY"
# dialect      = "v1"                # required for gpt-image-* models

# ---------- Google Gemini (native API; OpenAI-compat does NOT cover images) ----------
# [profile.gemini]
# provider     = "gemini"
# model        = "gemini-2.5-flash-image"
# api_key_env  = "GEMINI_API_KEY"

# ---------- LocalAI (run image gen locally, no API key needed) ----------
# [profile.local]
# provider     = "openai-compat"
# endpoint     = "http://localhost:8080/v1"
# model        = "stablediffusion"
# auth_style   = "none"
"#;

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
    std::fs::write(&path, STARTER_CONFIG).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}
