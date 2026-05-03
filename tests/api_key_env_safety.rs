//! Security tests: ensure pasted secrets in `api_key_env` are caught at
//! config-parse time and never echoed in any error message.

use std::path::PathBuf;

use pixforge::config::LoadedConfig;

fn parse(s: &str) -> Result<LoadedConfig, String> {
    LoadedConfig::parse(s, PathBuf::from("/tmp/test-config.toml")).map_err(|e| format!("{e:#}"))
}

/// The exact shape of the leak scenario from the v0.2.1 install report:
/// the user pasted a long alphanumeric secret where the env var NAME was
/// expected. We use a clearly-fake placeholder of similar length here so
/// the test exercises the same code path without itself looking like a
/// real key to secret scanners.
const LEAKED_KEY_LIKE: &str =
    "PLACEHOLDERkeyPLACEHOLDERkeyPLACEHOLDERkeyPLACEHOLDERkeyPLACEHOLDERkeyPLACEHOLDER123";

#[test]
fn pasted_secret_in_api_key_env_is_rejected_and_never_echoed() {
    let txt = format!(
        r#"
[profile.azure-mai]
provider     = "azure-mai"
endpoint     = "https://x.services.ai.azure.com"
model        = "MAI-Image-2"
api_key_env  = "{LEAKED_KEY_LIKE}"
"#
    );
    let err = parse(&txt).expect_err("must reject");
    assert!(
        !err.contains(LEAKED_KEY_LIKE),
        "ERROR MUST NOT CONTAIN THE LEAKED VALUE.\nGot error: {err}"
    );
    assert!(
        err.contains("does not look like an environment variable name")
            || err.contains("suspiciously long"),
        "should explain what's wrong: {err}"
    );
    assert!(
        err.contains("rotate")
            || err.contains("compromised")
            || err.contains("paste"),
        "should warn about secret rotation: {err}"
    );
}

#[test]
fn long_value_with_only_valid_chars_is_still_rejected_as_too_long() {
    // 80 chars, all alphanumeric — would pass the regex check but is
    // suspiciously long for an env var name. This is a real-world Azure
    // key shape (without `=`/`+`/`/`).
    let suspicious = "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZAB";
    assert_eq!(suspicious.len(), 80);
    let txt = format!(
        r#"
[profile.azure-mai]
provider     = "azure-mai"
endpoint     = "https://x.services.ai.azure.com"
model        = "MAI-Image-2"
api_key_env  = "{suspicious}"
"#
    );
    let err = parse(&txt).expect_err("80-char alphanum must fail");
    assert!(
        !err.contains(suspicious),
        "MUST NOT echo the value, got: {err}"
    );
    assert!(err.contains("suspiciously long"), "got: {err}");
}

#[test]
fn value_with_special_chars_is_rejected() {
    // Real OpenAI-style key prefix shape: starts with sk-, contains dashes.
    let bad = "sk-proj-abc123-def456";
    let txt = format!(
        r#"
[profile.openai]
provider     = "openai-compat"
endpoint     = "https://api.openai.com/v1"
model        = "gpt-image-1"
api_key_env  = "{bad}"
"#
    );
    let err = parse(&txt).expect_err("`-` in name must fail");
    assert!(!err.contains(bad), "MUST NOT echo the value, got: {err}");
}

#[test]
fn value_starting_with_digit_is_rejected() {
    // Posix env var names cannot start with a digit.
    let bad = "1AZURE_KEY";
    let txt = format!(
        r#"
[profile.azure-mai]
provider     = "azure-mai"
endpoint     = "https://x.services.ai.azure.com"
model        = "MAI-Image-2"
api_key_env  = "{bad}"
"#
    );
    let err = parse(&txt).expect_err("leading digit must fail");
    assert!(!err.contains(bad), "MUST NOT echo the value, got: {err}");
}

#[test]
fn empty_api_key_env_is_rejected() {
    let txt = r#"
[profile.azure-mai]
provider     = "azure-mai"
endpoint     = "https://x.services.ai.azure.com"
model        = "MAI-Image-2"
api_key_env  = ""
"#;
    let err = parse(txt).expect_err("empty must fail");
    assert!(err.contains("empty"), "got: {err}");
}

#[test]
fn normal_env_var_names_are_accepted() {
    let names = [
        "AZURE_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "_PRIVATE_VAR",
        "MY_KEY_2",
        "X",
    ];
    for name in names {
        let txt = format!(
            r#"
[profile.p]
provider     = "openai-compat"
endpoint     = "https://api.openai.com/v1"
model        = "gpt-image-1"
api_key_env  = "{name}"
"#
        );
        parse(&txt).unwrap_or_else(|e| panic!("name {name:?} should be accepted, got: {e}"));
    }
}
