//! Interactive `pixforge setup` wizard. Pure logic — all IO goes through
//! the trait seams in [`super::traits`], so unit tests can run the wizard
//! end-to-end with canned answers and an in-memory config.

use anyhow::{anyhow, bail, Context, Result};
use std::env;

use super::config_edit::{EditableConfig, ProfileDraft};
use super::traits::{
    AppendOutcome, ConfigStore, ConnectionTester, Prompter, ShellRcWriter, TestOutcome,
};

const PROVIDERS: &[(&str, &str)] = &[
    ("azure-mai", "Azure MAI (Microsoft AI image models on Azure AI Foundry)"),
    ("azure-openai", "Azure OpenAI (gpt-image-1, gpt-image-2, DALL·E 3)"),
    ("openai-compat", "OpenAI proper / LocalAI / any OpenAI-compatible server"),
    ("gemini", "Google Gemini (paid-only as of 2026)"),
];

/// What the wizard needs to know about the current shell environment.
/// Production passes real `std::env::var`; tests pass canned values.
pub struct EnvProbe<'a> {
    pub get: &'a dyn Fn(&str) -> Option<String>,
}

impl<'a> EnvProbe<'a> {
    pub fn from_real_env() -> Self {
        Self {
            get: &|k| env::var(k).ok().filter(|s| !s.is_empty()),
        }
    }

    pub fn is_set(&self, var: &str) -> bool {
        (self.get)(var).is_some()
    }
}

pub struct WizardDeps<'a> {
    pub prompter: &'a mut dyn Prompter,
    pub config: &'a dyn ConfigStore,
    pub shell_rc: &'a dyn ShellRcWriter,
    pub tester: &'a mut dyn ConnectionTester,
    pub env: EnvProbe<'a>,
}

/// Final outcome a wizard run reports back to `main.rs` for printing.
pub struct WizardResult {
    pub profile_name: String,
    pub config_path_str: String,
    pub set_as_default: bool,
    pub shell_rc_outcome: Option<AppendOutcome>,
    pub test_outcome: Option<Result<TestOutcome>>,
}

impl std::fmt::Debug for WizardResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WizardResult")
            .field("profile_name", &self.profile_name)
            .field("config_path_str", &self.config_path_str)
            .field("set_as_default", &self.set_as_default)
            .field("shell_rc_outcome", &self.shell_rc_outcome)
            .field("test_outcome", &self.test_outcome.as_ref().map(|r| r.is_ok()))
            .finish()
    }
}

pub fn run(deps: &mut WizardDeps<'_>) -> Result<WizardResult> {
    deps.prompter.note(
        "pixforge setup\n\
         Walks you through configuring a provider profile. Press Ctrl-C any \
         time to abort without changes.",
    );

    // --- Step 1: pick provider -------------------------------------------
    let provider_choices: Vec<String> = PROVIDERS
        .iter()
        .map(|(id, desc)| {
            let env_var = recommended_env_var(id);
            let detected = deps.env.is_set(env_var);
            let mark = if detected { " ✓ env detected" } else { "" };
            format!("{desc}{mark}")
        })
        .collect();

    let idx = deps
        .prompter
        .ask_choice("Which provider do you want to set up?", &provider_choices)?;
    let (provider_id, _provider_desc) = PROVIDERS[idx];

    // --- Step 2: per-provider field-by-field prompts ----------------------
    let draft = collect_provider_fields(deps.prompter, provider_id, &deps.env)?;
    draft
        .validate()
        .context("validation failed (this should have been caught per-field; please report)")?;

    // --- Step 3: profile name + collision policy --------------------------
    let existing_text = deps.config.read()?;
    let mut existing = match &existing_text {
        Some(t) => EditableConfig::parse(t)
            .context("can't safely edit your config (syntax error). Use `pixforge advanced-config` to fix it manually, then re-run setup.")?,
        None => EditableConfig::empty(),
    };

    let default_profile_name = provider_id.to_string();
    let mut profile_name = deps.prompter.ask_text(
        "Name for this profile (used as `--profile <name>`)",
        Some(&default_profile_name),
    )?;
    profile_name = profile_name.trim().to_string();
    if profile_name.is_empty() {
        bail!("profile name cannot be empty");
    }

    if existing.has_profile(&profile_name) {
        let action_choices = vec![
            "Overwrite the existing profile".to_string(),
            "Pick a different name".to_string(),
            "Abort without saving".to_string(),
        ];
        let action = deps.prompter.ask_choice(
            &format!("A profile named {profile_name:?} already exists. What now?"),
            &action_choices,
        )?;
        match action {
            0 => {} // overwrite — fall through
            1 => {
                let new_name = deps.prompter.ask_text("New profile name", None)?;
                profile_name = new_name.trim().to_string();
                if profile_name.is_empty() || existing.has_profile(&profile_name) {
                    bail!("name {profile_name:?} is empty or also already exists; aborting");
                }
            }
            _ => bail!("aborted by user"),
        }
    }

    // --- Step 4: optional connection test (with cost warning) ------------
    let mut test_outcome: Option<Result<TestOutcome>> = None;
    let cost_note = match provider_id {
        "azure-mai" | "azure-openai" | "openai-compat" => {
            "This will make ONE real generation request and may incur a small charge \
             per your provider's pricing. LocalAI users: $0 but may take 30s–2min on CPU."
        }
        "gemini" => {
            "This will make ONE real generation request. Gemini image gen is paid-only \
             in 2026 and may incur a small charge."
        }
        _ => "This will make ONE real generation request.",
    };
    deps.prompter.note(cost_note);
    let mut draft = draft;
    if deps.prompter.confirm("Test connection now?", false)? {
        test_outcome = Some(run_connection_test_loop(deps, &profile_name, &mut draft)?);
    }

    // --- Step 5: optional shell rc append --------------------------------
    let shell_rc_outcome = maybe_append_shell_rc(deps, &draft.api_key_env)?;

    // --- Step 6: write config --------------------------------------------
    existing.upsert_profile(&profile_name, &draft)?;
    let make_default = if existing.current_default_profile().is_none() {
        true
    } else {
        deps.prompter.confirm(
            &format!("Set {profile_name:?} as the default profile?"),
            false,
        )?
    };
    if make_default {
        existing.set_default_profile(&profile_name);
    }
    deps.config.write(&existing.to_string())?;

    Ok(WizardResult {
        profile_name,
        config_path_str: deps.config.path().display().to_string(),
        set_as_default: make_default,
        shell_rc_outcome,
        test_outcome,
    })
}

/// Run the connection test and on failure offer retry / save-anyway /
/// edit-endpoint / abort. Returns the final `Result<TestOutcome>` to be
/// stored on the `WizardResult`. If the user picks "abort", returns Err
/// to bubble out of the wizard.
fn run_connection_test_loop(
    deps: &mut WizardDeps<'_>,
    profile_name: &str,
    draft: &mut ProfileDraft,
) -> Result<Result<TestOutcome>> {
    loop {
        let probe = build_probe_profile(profile_name, draft)?;
        let outcome = deps.tester.test(&probe);
        match outcome {
            Ok(o) => {
                deps.prompter.info(&format!(
                    "✓ test ok ({} bytes, {:.1}s, {} attempts)",
                    o.bytes, o.latency_secs, o.attempts
                ));
                return Ok(Ok(o));
            }
            Err(e) => {
                deps.prompter.info(&format!("✗ test failed: {e:#}"));
                let choices = vec![
                    "Edit the endpoint and retry".to_string(),
                    "Retry as-is".to_string(),
                    "Save profile anyway (skip test)".to_string(),
                    "Abort wizard".to_string(),
                ];
                let pick = deps.prompter.ask_choice("What now?", &choices)?;
                match pick {
                    0 => {
                        let new_ep = ask_validated(
                            deps.prompter,
                            "New endpoint",
                            draft.endpoint.as_deref(),
                            |s| crate::config::validate_endpoint_for_provider(s, &draft.provider),
                        )?;
                        draft.endpoint = Some(new_ep);
                    }
                    1 => {} // retry as-is
                    2 => return Ok(Err(e)), // save anyway, return the error to record
                    _ => bail!("aborted by user after failed connection test"),
                }
            }
        }
    }
}

fn maybe_append_shell_rc(
    deps: &mut WizardDeps<'_>,
    var: &str,
) -> Result<Option<AppendOutcome>> {
    if deps.env.is_set(var) {
        deps.prompter
            .info(&format!("✓ ${var} already set in your current shell"));
        return Ok(None);
    }
    let Some(rc) = deps.shell_rc.rc_path() else {
        deps.prompter.note(&format!(
            "${var} is not set in your shell, and pixforge couldn't detect your shell rc \
             file (set $SHELL?). Add this line manually:\n  export {var}='<your-key>'"
        ));
        return Ok(None);
    };
    deps.prompter.note(&format!(
        "${var} is not set in your shell. The wizard can append \
         `export {var}=...` to {} for you.",
        rc.display()
    ));
    if !deps
        .prompter
        .confirm("Save the secret to your shell config?", false)?
    {
        deps.prompter.note(&format!(
            "OK — set it manually before running pixforge:\n  export {var}='<your-key>'"
        ));
        return Ok(None);
    }
    let secret = deps.prompter.ask_secret(&format!("Paste your {var}"))?;
    if secret.trim().is_empty() {
        bail!("got empty secret; aborting shell rc edit");
    }
    let outcome = deps.shell_rc.append_export(var, secret.trim())?;
    match &outcome {
        AppendOutcome::Appended { path } => deps.prompter.info(&format!(
            "✓ wrote export to {} (run `source {0}` or open a new shell)",
            path.display()
        )),
        AppendOutcome::AlreadyPresent { path } => deps.prompter.info(&format!(
            "✓ {} already exports {var}; left it untouched",
            path.display()
        )),
    }
    Ok(Some(outcome))
}

fn collect_provider_fields(
    p: &mut dyn Prompter,
    provider_id: &str,
    env: &EnvProbe<'_>,
) -> Result<ProfileDraft> {
    let default_env_var = recommended_env_var(provider_id);
    let api_key_env = ask_validated(p, "Env var name that holds the API key", Some(default_env_var), |s| {
        crate::config::validate_api_key_env_name(s)
    })?;

    match provider_id {
        "azure-mai" => {
            let endpoint = ask_validated(
                p,
                "Azure MAI endpoint (base URL only — pixforge appends the path)",
                None,
                |s| crate::config::validate_endpoint_for_provider(s, "azure-mai"),
            )?;
            let model =
                p.ask_text("Model / deployment name (e.g. MAI-Image-2 or MAI-Image-2e)", None)?;
            let api_version = p.ask_text("API version", Some("preview"))?;
            Ok(ProfileDraft {
                provider: "azure-mai".to_string(),
                endpoint: Some(endpoint),
                model,
                api_version: Some(api_version),
                api_key_env,
                auth_style: None,
                dialect: None,
            })
        }
        "azure-openai" => {
            let endpoint = ask_validated(
                p,
                "Azure OpenAI endpoint (e.g. https://your-resource.openai.azure.com)",
                None,
                |s| crate::config::validate_endpoint_for_provider(s, "azure-openai"),
            )?;
            let model = p.ask_text("Deployment name (e.g. gpt-image-2 or dall-e-3)", None)?;
            let dialect_choices = vec![
                "v1 — required for gpt-image-1, gpt-image-2 (recommended)".to_string(),
                "deployment — for DALL·E 3 / DALL·E 2".to_string(),
            ];
            let dialect_idx = p.ask_choice("Which Azure URL dialect does this model use?", &dialect_choices)?;
            let (dialect, api_version) = if dialect_idx == 0 {
                ("v1", None)
            } else {
                let v =
                    p.ask_text("API version (required for `deployment` dialect)", Some("2024-02-01"))?;
                ("deployment", Some(v))
            };
            Ok(ProfileDraft {
                provider: "azure-openai".to_string(),
                endpoint: Some(endpoint),
                model,
                api_version,
                api_key_env,
                auth_style: None,
                dialect: Some(dialect.to_string()),
            })
        }
        "openai-compat" => {
            let endpoint = ask_validated(
                p,
                "Endpoint (OpenAI: https://api.openai.com/v1; LocalAI: http://localhost:8080/v1)",
                Some("https://api.openai.com/v1"),
                |s| crate::config::validate_endpoint_for_provider(s, "openai-compat"),
            )?;
            let model = p.ask_text("Model name (e.g. gpt-image-1 / sd-1.5-ggml)", None)?;
            let auth_choices = vec![
                "bearer (default for OpenAI proper)".to_string(),
                "api-key (some compat shims use this header)".to_string(),
                "none (LocalAI etc.)".to_string(),
            ];
            let auth_idx = p.ask_choice("Auth header style", &auth_choices)?;
            let auth_style = match auth_idx {
                0 => "bearer",
                1 => "api-key",
                _ => "none",
            };
            // For auth_style=none, api_key_env is moot but still validated above.
            let _ = env; // currently unused; placeholder for future smarts
            Ok(ProfileDraft {
                provider: "openai-compat".to_string(),
                endpoint: Some(endpoint),
                model,
                api_version: None,
                api_key_env,
                auth_style: Some(auth_style.to_string()),
                dialect: None,
            })
        }
        "gemini" => {
            let model = p.ask_text(
                "Model (e.g. gemini-2.5-flash-image or gemini-3.1-flash-image-preview)",
                Some("gemini-2.5-flash-image"),
            )?;
            // Gemini's endpoint defaults to Google's URL; advanced users can
            // edit config.toml to override.
            Ok(ProfileDraft {
                provider: "gemini".to_string(),
                endpoint: None,
                model,
                api_version: None,
                api_key_env,
                auth_style: None,
                dialect: None,
            })
        }
        other => bail!("unknown provider {other:?}"),
    }
}

fn ask_validated(
    p: &mut dyn Prompter,
    label: &str,
    default: Option<&str>,
    mut validator: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    loop {
        let val = p.ask_text(label, default)?;
        let val = val.trim().to_string();
        match validator(&val) {
            Ok(()) => return Ok(val),
            Err(e) => {
                p.info(&format!("✗ {e:#}"));
                p.info("Try again.");
            }
        }
    }
}

fn recommended_env_var(provider_id: &str) -> &'static str {
    match provider_id {
        "azure-mai" => "AZURE_API_KEY",
        "azure-openai" => "AZURE_OPENAI_API_KEY",
        "openai-compat" => "OPENAI_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        _ => "API_KEY",
    }
}

/// Build a `Profile` directly from a `ProfileDraft` for the purposes of the
/// connection test, WITHOUT round-tripping through TOML. This means we
/// can test before writing to disk.
fn build_probe_profile(
    name: &str,
    draft: &ProfileDraft,
) -> Result<crate::config::Profile> {
    let mut text = String::new();
    text.push_str(&format!("[profile.{}]\n", name));
    text.push_str(&format!("provider = \"{}\"\n", draft.provider));
    if let Some(ep) = &draft.endpoint {
        text.push_str(&format!("endpoint = {:?}\n", ep));
    }
    text.push_str(&format!("model = {:?}\n", draft.model));
    if let Some(v) = &draft.api_version {
        text.push_str(&format!("api_version = {:?}\n", v));
    }
    text.push_str(&format!("api_key_env = {:?}\n", draft.api_key_env));
    if let Some(a) = &draft.auth_style {
        text.push_str(&format!("auth_style = {:?}\n", a));
    }
    if let Some(d) = &draft.dialect {
        text.push_str(&format!("dialect = {:?}\n", d));
    }
    // Tight settings for probing: 1 attempt, 30s timeout. Connection tests
    // should be a quick binary "does this work?" — long retry loops on a
    // typo'd endpoint just waste the user's time.
    text.push_str("max_attempts = 1\n");
    text.push_str("timeout_secs = 30\n");
    let cfg = crate::config::LoadedConfig::parse(&text, std::path::PathBuf::from("<probe>"))?;
    let profile = cfg
        .profiles
        .get(name)
        .ok_or_else(|| anyhow!("internal: probe profile not found after parse"))?
        .clone();
    Ok(profile)
}
