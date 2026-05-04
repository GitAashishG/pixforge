//! TOML-edit-based reader/writer for the pixforge config file. Preserves
//! comments and formatting in existing files; refuses to mutate invalid
//! TOML; supports profile collision detection.

use anyhow::{anyhow, Context, Result};
use toml_edit::{value, DocumentMut, Item, Table};

use crate::config::Profile;

pub struct EditableConfig {
    pub doc: DocumentMut,
}

impl std::fmt::Debug for EditableConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EditableConfig({} chars)", self.doc.to_string().len())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CollisionAction {
    Overwrite,
    UseNewName(String),
    Abort,
}

impl EditableConfig {
    /// Create a fresh config in memory (no profiles, no default).
    pub fn empty() -> Self {
        Self {
            doc: DocumentMut::new(),
        }
    }

    /// Parse an existing config. Refuses ambiguous parses (returns Err).
    pub fn parse(text: &str) -> Result<Self> {
        let doc: DocumentMut = text
            .parse()
            .context("config has a TOML syntax error; fix it manually before re-running setup")?;
        Ok(Self { doc })
    }

    /// True if a `[profile.<name>]` table is already present.
    pub fn has_profile(&self, name: &str) -> bool {
        self.doc
            .get("profile")
            .and_then(|p| p.as_table())
            .and_then(|t| t.get(name))
            .is_some()
    }

    pub fn list_profiles(&self) -> Vec<String> {
        self.doc
            .get("profile")
            .and_then(|p| p.as_table())
            .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
            .unwrap_or_default()
    }

    /// Insert (or replace) a `[profile.<name>]` table from a `Profile`.
    /// Caller is expected to have resolved any collision policy before
    /// calling — this just performs the write.
    pub fn upsert_profile(&mut self, profile_name: &str, profile: &ProfileDraft) -> Result<()> {
        // Ensure `[profile]` table exists.
        if self.doc.get("profile").is_none() {
            self.doc["profile"] = Item::Table(Table::new());
        }
        let table = self
            .doc
            .get_mut("profile")
            .and_then(|i| i.as_table_mut())
            .ok_or_else(|| anyhow!("internal: `profile` is not a table"))?;
        // Mark as "dotted" so it serializes as `[profile.X]` not nested.
        table.set_implicit(true);

        let mut entry = Table::new();
        entry["provider"] = value(profile.provider.as_str());
        if let Some(ep) = &profile.endpoint {
            entry["endpoint"] = value(ep.as_str());
        }
        entry["model"] = value(profile.model.as_str());
        if let Some(av) = &profile.api_version {
            entry["api_version"] = value(av.as_str());
        }
        entry["api_key_env"] = value(profile.api_key_env.as_str());
        if let Some(auth) = &profile.auth_style {
            entry["auth_style"] = value(auth.as_str());
        }
        if let Some(d) = &profile.dialect {
            entry["dialect"] = value(d.as_str());
        }
        // `keywords`-like fields could go here later (width, height, etc.).

        table.insert(profile_name, Item::Table(entry));
        Ok(())
    }

    pub fn set_default_profile(&mut self, name: &str) {
        self.doc["default_profile"] = value(name);
    }

    pub fn current_default_profile(&self) -> Option<String> {
        self.doc
            .get("default_profile")
            .and_then(|i| i.as_str())
            .map(|s| s.to_string())
    }

    pub fn to_string(&self) -> String {
        self.doc.to_string()
    }
}

/// Sanitized profile draft assembled by the wizard. Mirrors the shape we
/// want in TOML; the actual `Profile` struct in src/config.rs is built
/// from this after the file is written and re-parsed.
#[derive(Debug, Clone)]
pub struct ProfileDraft {
    pub provider: String,
    pub endpoint: Option<String>,
    pub model: String,
    pub api_version: Option<String>,
    pub api_key_env: String,
    pub auth_style: Option<String>,
    pub dialect: Option<String>,
}

impl ProfileDraft {
    /// Run the existing config-time validators against this draft. Returns
    /// the first validation error, or `Ok(())` if all checks pass.
    pub fn validate(&self) -> Result<()> {
        // Reuse the public ValidationCheck functions exposed by config.
        crate::config::validate_provider_and_dialect(
            &self.provider,
            self.dialect.as_deref(),
        )?;
        if let Some(ep) = &self.endpoint {
            crate::config::validate_endpoint_for_provider(ep, &self.provider)?;
        }
        crate::config::validate_api_key_env_name(&self.api_key_env)?;
        Ok(())
    }
}

/// Render a `Profile` (resolved, post-parse) back into a `ProfileDraft`
/// for collision-overwrite scenarios where we want to compare or rebuild.
#[allow(dead_code)]
pub fn profile_to_draft(p: &Profile) -> ProfileDraft {
    ProfileDraft {
        provider: p.provider.id().to_string(),
        endpoint: Some(p.endpoint.clone()),
        model: p.model.clone(),
        api_version: p.api_version.clone(),
        api_key_env: p.api_key_env.clone().unwrap_or_default(),
        auth_style: None,
        dialect: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_draft() -> ProfileDraft {
        ProfileDraft {
            provider: "openai-compat".to_string(),
            endpoint: Some("https://api.openai.com/v1".to_string()),
            model: "gpt-image-1".to_string(),
            api_version: None,
            api_key_env: "OPENAI_API_KEY".to_string(),
            auth_style: None,
            dialect: None,
        }
    }

    #[test]
    fn upsert_into_empty_config() {
        let mut c = EditableConfig::empty();
        c.upsert_profile("openai", &fixture_draft()).unwrap();
        c.set_default_profile("openai");
        let out = c.to_string();
        assert!(out.contains("default_profile = \"openai\""));
        assert!(out.contains("[profile.openai]"));
        assert!(out.contains("provider = \"openai-compat\""));
        assert!(out.contains("api_key_env = \"OPENAI_API_KEY\""));
    }

    #[test]
    fn parse_preserves_existing_comments_when_appending() {
        let original = r#"# my hand-written config
default_profile = "azure-mai"

# my azure profile, do not touch
[profile.azure-mai]
provider = "azure-mai"
endpoint = "https://x.services.ai.azure.com"
model = "MAI-Image-2"
api_key_env = "AZURE_API_KEY"
api_version = "preview"
"#;
        let mut c = EditableConfig::parse(original).unwrap();
        assert!(c.has_profile("azure-mai"));
        assert!(!c.has_profile("openai"));
        c.upsert_profile("openai", &fixture_draft()).unwrap();
        let out = c.to_string();
        assert!(out.contains("# my hand-written config"));
        assert!(out.contains("# my azure profile"));
        assert!(out.contains("[profile.azure-mai]"));
        assert!(out.contains("[profile.openai]"));
    }

    #[test]
    fn invalid_toml_is_rejected() {
        let bad = "this is not [valid toml = yes\n";
        let err = EditableConfig::parse(bad).unwrap_err();
        assert!(format!("{err:#}").contains("syntax"), "got: {err}");
    }

    #[test]
    fn collision_detected_via_has_profile() {
        let original = r#"
[profile.openai]
provider = "openai-compat"
endpoint = "https://api.openai.com/v1"
model = "x"
api_key_env = "OPENAI_API_KEY"
"#;
        let c = EditableConfig::parse(original).unwrap();
        assert!(c.has_profile("openai"));
        assert!(!c.has_profile("openai2"));
    }

    fn _suppress_unused_warning(_: CollisionAction) {}
}
