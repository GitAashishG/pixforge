//! Tests for the v0.2.1 config UX fixes.

use std::path::PathBuf;

use pixforge::config::{LoadedConfig, STARTER_CONFIG};

fn parse(s: &str) -> Result<LoadedConfig, String> {
    LoadedConfig::parse(s, PathBuf::from("/tmp/test-config.toml")).map_err(|e| format!("{e:#}"))
}

#[test]
fn starter_config_has_one_active_profile_so_init_then_export_just_works() {
    // The "init then export OPENAI_API_KEY then run" path must succeed
    // through config parsing — adapter-side errors come later when the
    // env var is missing.
    let cfg = parse(STARTER_CONFIG).expect("starter config must parse with one active profile");
    assert!(
        cfg.profiles.contains_key("openai"),
        "starter must enable an openai profile by default; got: {:?}",
        cfg.profile_names()
    );
    assert_eq!(cfg.default_profile.as_deref(), Some("openai"));
}

#[test]
fn all_commented_profiles_produce_helpful_error_listing_their_names() {
    let txt = r#"
default_profile = "azure-mai"

# [profile.azure-mai]
# provider = "azure-mai"
# endpoint = "https://x.example.com"
# model = "y"
# api_key_env = "K"

# [profile.gemini]
# provider = "gemini"
# model = "g"
# api_key_env = "K2"
"#;
    let err = parse(txt).expect_err("all-commented config must fail");
    assert!(err.contains("commented-out"), "got: {err}");
    assert!(err.contains("azure-mai"), "should name the commented profiles; got: {err}");
    assert!(err.contains("gemini"), "should name all commented profiles; got: {err}");
    assert!(err.contains("Remove the leading"), "should tell user the fix; got: {err}");
}

#[test]
fn empty_config_without_commented_blocks_uses_generic_error() {
    let txt = r#"
# this is just a comment, no profile here
default_profile = "x"
"#;
    let err = parse(txt).expect_err("must fail");
    assert!(err.contains("contains no profiles"), "got: {err}");
    assert!(err.contains("init --force"), "should suggest init; got: {err}");
}

#[test]
fn azure_mai_endpoint_with_full_path_is_rejected() {
    let txt = r#"
[profile.azure-mai]
provider     = "azure-mai"
endpoint     = "https://x.services.ai.azure.com/mai/v1/images/generations?api-version=preview"
model        = "MAI-Image-2"
api_key_env  = "AZURE_API_KEY"
"#;
    let err = parse(txt).expect_err("full URL must be rejected");
    assert!(
        err.contains("query string") || err.contains("API path"),
        "got: {err}"
    );
}

#[test]
fn azure_mai_endpoint_with_only_path_is_rejected() {
    let txt = r#"
[profile.azure-mai]
provider     = "azure-mai"
endpoint     = "https://x.services.ai.azure.com/mai/v1/images/generations"
model        = "MAI-Image-2"
api_key_env  = "AZURE_API_KEY"
"#;
    let err = parse(txt).expect_err("path-included URL must be rejected");
    assert!(err.contains("API path"), "got: {err}");
    assert!(err.contains("/mai/v1/"), "should mention the offending path; got: {err}");
}

#[test]
fn openai_compat_endpoint_with_images_generations_path_is_rejected() {
    let txt = r#"
[profile.openai]
provider     = "openai-compat"
endpoint     = "https://api.openai.com/v1/images/generations"
model        = "gpt-image-1"
api_key_env  = "OPENAI_API_KEY"
"#;
    let err = parse(txt).expect_err("path-included URL must be rejected");
    assert!(err.contains("API path"), "got: {err}");
}

#[test]
fn azure_openai_endpoint_with_deployments_path_is_rejected() {
    let txt = r#"
[profile.azure-openai]
provider     = "azure-openai"
endpoint     = "https://x.openai.azure.com/openai/deployments/dall-e-3/images/generations"
model        = "dall-e-3"
api_version  = "2024-02-01"
api_key_env  = "AZURE_OPENAI_API_KEY"
"#;
    let err = parse(txt).expect_err("path-included URL must be rejected");
    assert!(err.contains("/openai/deployments/"), "got: {err}");
}

#[test]
fn gemini_endpoint_with_generatecontent_is_rejected() {
    let txt = r#"
[profile.gemini]
provider     = "gemini"
endpoint     = "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash-image:generateContent"
model        = "gemini-2.5-flash-image"
api_key_env  = "GEMINI_API_KEY"
"#;
    let err = parse(txt).expect_err("path-included URL must be rejected");
    assert!(err.contains("API path"), "got: {err}");
}

#[test]
fn clean_base_endpoints_are_accepted() {
    // All four providers, base URLs only — these must all parse cleanly.
    let cases: &[(&str, &str)] = &[
        (
            "azure-mai",
            r#"[profile.p]
provider = "azure-mai"
endpoint = "https://x.services.ai.azure.com"
model = "MAI-Image-2"
api_key_env = "K"
"#,
        ),
        (
            "azure-openai",
            r#"[profile.p]
provider = "azure-openai"
endpoint = "https://x.openai.azure.com"
model = "dall-e-3"
api_version = "2024-02-01"
api_key_env = "K"
"#,
        ),
        (
            "openai-compat",
            r#"[profile.p]
provider = "openai-compat"
endpoint = "https://api.openai.com/v1"
model = "gpt-image-1"
api_key_env = "K"
"#,
        ),
        (
            "gemini",
            r#"[profile.p]
provider = "gemini"
model = "gemini-2.5-flash-image"
api_key_env = "K"
"#,
        ),
    ];
    for (label, txt) in cases {
        parse(txt).unwrap_or_else(|e| panic!("{label} base URL should parse cleanly, got: {e}"));
    }
}
