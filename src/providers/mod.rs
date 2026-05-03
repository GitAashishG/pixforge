//! Image generation provider abstraction.
//!
//! Each provider implements [`ImageProvider`] by translating a canonical
//! [`Request`] into its native HTTP shape, then normalizing the response
//! back into [`Response`]. The trait is sync (matches our `ureq` stack).
//!
//! Shared HTTP / retry / parsing helpers live at the bottom of this module.
//! They are intentionally small and free-functions; each adapter writes its
//! own retry loop using them so the per-provider error semantics stay
//! local and explicit.

// Helpers and the canonical Request struct are constructed by callers but
// not yet *consumed* by every adapter until the rest land. Allow until then.
#![allow(dead_code)]

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::Value;
use std::time::Duration;

pub mod azure_mai;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub fn as_string(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }
}

/// Canonical request passed into an [`ImageProvider`]. Fields not relevant
/// to a given provider are silently ignored *unless* `size_explicit` is true,
/// in which case providers that can't honor a size should return an error.
#[derive(Debug)]
pub struct Request<'a> {
    pub prompt: &'a str,
    pub model: &'a str,
    pub n: u32,
    pub size: Option<Size>,
    pub size_explicit: bool,
    pub seed: Option<u64>,
    pub negative_prompt: Option<&'a str>,
    pub quality: Option<&'a str>,
    pub extra: &'a serde_json::Map<String, Value>,
}

#[derive(Debug)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub revised_prompt: Option<String>,
    pub mime_type: String,
}

#[derive(Debug)]
pub struct Response {
    pub images: Vec<GeneratedImage>,
    pub latency_secs: f64,
    pub attempts: u32,
}

/// Implemented by every backend (Azure MAI, Azure OpenAI, OpenAI-compat, Gemini, ...).
pub trait ImageProvider: Send + Sync {
    /// Stable id for the provider (e.g. `"azure-mai"`). Written into the sidecar.
    fn id(&self) -> &'static str;

    /// Issue one generation request, performing retries internally.
    /// `on_retry(attempt, message, sleep_secs)` is called before each backoff.
    fn generate(
        &self,
        req: &Request<'_>,
        on_retry: &mut dyn FnMut(u32, &str, f64),
    ) -> Result<Response>;
}

// ---------------------------------------------------------------------------
// Shared helpers (used by every adapter).
// ---------------------------------------------------------------------------

const MAX_BACKOFF_SECS: f64 = 60.0;
const RESPONSE_BODY_EXCERPT_CHARS: usize = 400;

/// Cheap pseudo-random jitter in `[0.0, 1.0)` without pulling in `rand`.
pub fn jitter() -> f64 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos as f64 / 1_000_000_000.0).fract()
}

/// Exponential backoff with jitter, capped at `MAX_BACKOFF_SECS`.
/// Sequence: ~1, 2, 4, 8, 16, 32, 60, 60, ... (+ up to 1s jitter each).
pub fn backoff_secs(attempt: u32) -> f64 {
    let base = 2_f64.powi((attempt as i32 - 1).max(0));
    base.min(MAX_BACKOFF_SECS) + jitter()
}

/// Truncate a body for inclusion in error messages, collapsing newlines.
pub fn excerpt(body: &str) -> String {
    if body.chars().count() <= RESPONSE_BODY_EXCERPT_CHARS {
        body.replace('\n', " ")
    } else {
        let truncated: String = body.chars().take(RESPONSE_BODY_EXCERPT_CHARS).collect();
        format!("{truncated}…")
    }
}

/// Decode a base64 string into raw bytes.
pub fn decode_b64(s: &str) -> Result<Vec<u8>> {
    B64.decode(s).context("decoding base64 image data")
}

/// Parse a `Retry-After` header, plus Azure-style `retry-after-ms` and
/// `x-ms-retry-after-ms`. Falls back to `backoff_secs(attempt)` if no header
/// is parseable.
pub fn retry_after_from_headers(
    header_lookup: &dyn Fn(&str) -> Option<String>,
    attempt: u32,
) -> f64 {
    for h in ["retry-after-ms", "x-ms-retry-after-ms"] {
        if let Some(v) = header_lookup(h) {
            if let Ok(ms) = v.trim().parse::<u64>() {
                return (ms as f64) / 1000.0;
            }
        }
    }
    if let Some(v) = header_lookup("retry-after") {
        let v = v.trim();
        if let Ok(secs) = v.parse::<f64>() {
            return secs;
        }
        if let Ok(when) = chrono::DateTime::parse_from_rfc2822(v) {
            let now = chrono::Utc::now();
            let secs = (when.with_timezone(&chrono::Utc) - now).num_milliseconds() as f64 / 1000.0;
            if secs > 0.0 {
                return secs;
            }
        }
    }
    backoff_secs(attempt)
}

/// Build a `ureq::Agent` with the given total timeout.
pub fn build_agent(timeout_secs: u64) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
}

/// Parse a JSON response body. Returns a friendly error if empty or malformed.
pub fn parse_json_body(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        bail!("empty response body");
    }
    serde_json::from_str(text)
        .with_context(|| format!("parsing JSON response: {}", excerpt(text)))
}

/// Pretty-print a `ureq::Error` from a non-2xx status code: turn it into
/// a `(status_code, body_excerpt, header_lookup)` tuple suitable for
/// inclusion in an error message and for retry-policy decisions.
///
/// Adapters typically use this in the `Err(ureq::Error::Status(_, _))`
/// arm of their request loop, after deciding the status code is not
/// retryable.
pub fn render_status_error(code: u16, body: String) -> anyhow::Error {
    anyhow!("HTTP {code}: {}", excerpt(&body))
}
