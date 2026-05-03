use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn prompt_hash(prompt: &str) -> String {
    let mut h = Sha256::new();
    h.update(prompt.as_bytes());
    hex::encode(h.finalize())[..12].to_string()
}

fn short_hash(prompt: &str) -> String {
    prompt_hash(prompt)[..6].to_string()
}

/// `./pixforge-{YYYYMMDD-HHMMSS}-{hash6}.png`, with `-1`, `-2`, … on collision.
pub fn default_output_path(prompt: &str) -> PathBuf {
    let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let hash = short_hash(prompt);
    let mut candidate = PathBuf::from(format!("./pixforge-{stamp}-{hash}.png"));
    let mut suffix = 1u32;
    while candidate.exists() {
        candidate = PathBuf::from(format!("./pixforge-{stamp}-{hash}-{suffix}.png"));
        suffix += 1;
    }
    candidate
}

/// Write `bytes` to `path` atomically: write to `path.tmp` then rename.
pub fn write_image_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
    }
    let tmp = with_extra_extension(path, "tmp");
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .with_context(|| format!("opening temp file {}", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("writing temp file {}", tmp.display()))?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn with_extra_extension(path: &Path, extra: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(extra);
    PathBuf::from(s)
}

/// Inputs for the sidecar JSON. Keeps the call-site at `main.rs` from
/// having to know the JSON field order or schema version.
pub struct SidecarInput<'a> {
    pub prompt: &'a str,
    pub provider_id: &'a str,
    pub profile_name: &'a str,
    pub model: &'a str,
    pub endpoint: &'a str,
    pub width: u32,
    pub height: u32,
    pub mime_type: &'a str,
    pub revised_prompt: Option<&'a str>,
    pub latency_secs: f64,
    pub attempts: u32,
}

#[derive(Debug, Serialize)]
struct Sidecar<'a> {
    schema_version: u32,
    generated_at: String,
    provider: &'a str,
    profile: &'a str,
    model: &'a str,
    endpoint: &'a str,
    width: u32,
    height: u32,
    prompt: &'a str,
    prompt_hash: String,
    revised_prompt: &'a str,
    mime_type: &'a str,
    latency_s: f64,
    attempts: u32,
}

pub fn sidecar_path(image_path: &Path) -> PathBuf {
    with_extra_extension(image_path, "prompt.json")
}

pub fn write_sidecar(image_path: &Path, input: &SidecarInput<'_>) -> Result<PathBuf> {
    let path = sidecar_path(image_path);
    let sidecar = Sidecar {
        schema_version: 2,
        generated_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        provider: input.provider_id,
        profile: input.profile_name,
        model: input.model,
        endpoint: input.endpoint,
        width: input.width,
        height: input.height,
        prompt: input.prompt,
        prompt_hash: prompt_hash(input.prompt),
        revised_prompt: input.revised_prompt.unwrap_or(""),
        mime_type: input.mime_type,
        latency_s: round2(input.latency_secs),
        attempts: input.attempts,
    };
    let mut text = serde_json::to_string_pretty(&sidecar).context("serializing sidecar")?;
    text.push('\n');
    let tmp = with_extra_extension(&path, "tmp");
    std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(path)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Best-effort: open `path` in the OS default viewer. Returns the error
/// for the caller to log (non-fatal).
pub fn open_in_default_app(path: &Path) -> Result<()> {
    open::that_detached(path).with_context(|| format!("opening {}", path.display()))
}
