use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::client::GenerationResult;

pub fn prompt_hash(prompt: &str) -> String {
    let mut h = Sha256::new();
    h.update(prompt.as_bytes());
    hex::encode(h.finalize())[..12].to_string()
}

fn short_hash(prompt: &str) -> String {
    prompt_hash(prompt)[..6].to_string()
}

/// Compute a default output path in the current directory:
/// `./imagine-{YYYYMMDD-HHMMSS}-{hash6}.png`, with `-1`, `-2`, … on collision.
pub fn default_output_path(prompt: &str) -> PathBuf {
    let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let hash = short_hash(prompt);
    let mut candidate = PathBuf::from(format!("./imagine-{stamp}-{hash}.png"));
    let mut suffix = 1u32;
    while candidate.exists() {
        candidate = PathBuf::from(format!("./imagine-{stamp}-{hash}-{suffix}.png"));
        suffix += 1;
    }
    candidate
}

/// Write `bytes` to `path` atomically: create `path.tmp` exclusively, write+sync, rename.
/// Refuses to clobber if `path` already exists (caller resolves collisions).
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

#[derive(Debug, Serialize)]
struct Sidecar<'a> {
    generated_at: String,
    deployment: &'a str,
    endpoint: &'a str,
    width: u32,
    height: u32,
    prompt: &'a str,
    prompt_hash: String,
    revised_prompt: &'a str,
    latency_s: f64,
    attempts: u32,
}

pub fn sidecar_path(image_path: &Path) -> PathBuf {
    with_extra_extension(image_path, "prompt.json")
}

pub fn write_sidecar(
    image_path: &Path,
    prompt: &str,
    endpoint: &str,
    res: &GenerationResult,
) -> Result<PathBuf> {
    let path = sidecar_path(image_path);
    let sidecar = Sidecar {
        generated_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        deployment: &res.deployment,
        endpoint,
        width: res.width,
        height: res.height,
        prompt,
        prompt_hash: prompt_hash(prompt),
        revised_prompt: &res.revised_prompt,
        latency_s: round2(res.latency_secs),
        attempts: res.attempts,
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

/// Best-effort: open `path` in the OS default viewer. Errors are returned for the
/// caller to log (non-fatal).
pub fn open_in_default_app(path: &Path) -> Result<()> {
    open::that_detached(path).with_context(|| format!("opening {}", path.display()))
}
