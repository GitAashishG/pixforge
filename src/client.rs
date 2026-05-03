use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::Serialize;
use serde_json::Value;
use std::time::{Duration, Instant};

use crate::config::Config;

const MAX_BACKOFF_SECS: f64 = 60.0;
const RESPONSE_BODY_EXCERPT: usize = 400;

/// Pseudo-random jitter in [0, 1.0) without pulling rand as a dep.
fn jitter() -> f64 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos as f64 / 1_000_000_000.0).fract()
}

fn backoff_secs(attempt: u32) -> f64 {
    let base = 2_f64.powi((attempt as i32 - 1).max(0));
    base.min(MAX_BACKOFF_SECS) + jitter()
}

#[derive(Debug, Serialize)]
struct RequestBody<'a> {
    prompt: &'a str,
    width: u32,
    height: u32,
    n: u32,
    model: &'a str,
}

#[derive(Debug)]
pub struct GenerationResult {
    pub image_bytes: Vec<u8>,
    pub revised_prompt: String,
    pub deployment: String,
    pub width: u32,
    pub height: u32,
    pub latency_secs: f64,
    pub attempts: u32,
}

pub struct AzureImageClient {
    cfg: Config,
    agent: ureq::Agent,
}

impl AzureImageClient {
    pub fn new(cfg: Config) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build();
        Self { cfg, agent }
    }

    pub fn generate<F>(&self, prompt: &str, mut on_retry: F) -> Result<GenerationResult>
    where
        F: FnMut(u32, &str, f64),
    {
        validate_dims(self.cfg.width, self.cfg.height)?;

        let body = RequestBody {
            prompt,
            width: self.cfg.width,
            height: self.cfg.height,
            n: 1,
            model: &self.cfg.deployment,
        };
        let body_json = serde_json::to_string(&body).context("serializing request body")?;
        let url = self.cfg.url();

        let started = Instant::now();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=self.cfg.max_attempts {
            let attempt_started = Instant::now();
            let resp = self
                .agent
                .post(&url)
                .set("Content-Type", "application/json")
                .set("api-key", &self.cfg.api_key)
                .send_string(&body_json);

            match resp {
                Ok(r) => {
                    let status = r.status();
                    if status == 200 {
                        let text = r
                            .into_string()
                            .context("reading 200 response body")?;
                        let parsed: Value = serde_json::from_str(&text)
                            .with_context(|| format!("parsing JSON response: {}", excerpt(&text)))?;
                        let (image_bytes, revised_prompt) = decode_image(&parsed, &text)?;
                        return Ok(GenerationResult {
                            image_bytes,
                            revised_prompt,
                            deployment: self.cfg.deployment.clone(),
                            width: self.cfg.width,
                            height: self.cfg.height,
                            latency_secs: started.elapsed().as_secs_f64(),
                            attempts: attempt,
                        });
                    }
                    // Should not be reached: ureq turns non-2xx into Err.
                    let body = r.into_string().unwrap_or_default();
                    bail!("HTTP {status}: {}", excerpt(&body));
                }
                Err(ureq::Error::Status(code, r)) => {
                    let retryable = code == 429 || (500..=599).contains(&code);
                    let retry_after = retry_after_from_response(&r, attempt);
                    let body = r.into_string().unwrap_or_default();
                    let msg = format!("HTTP {code}: {}", excerpt(&body));
                    if !retryable || attempt == self.cfg.max_attempts {
                        last_err = Some(anyhow!(msg));
                        break;
                    }
                    on_retry(attempt, &msg, retry_after);
                    std::thread::sleep(Duration::from_secs_f64(retry_after));
                    last_err = Some(anyhow!(msg));
                }
                Err(ureq::Error::Transport(t)) => {
                    let elapsed = attempt_started.elapsed().as_secs_f64();
                    let msg = format!("transport error after {elapsed:.1}s: {t}");
                    if attempt == self.cfg.max_attempts {
                        last_err = Some(anyhow!(msg));
                        break;
                    }
                    let wait = backoff_secs(attempt);
                    on_retry(attempt, &msg, wait);
                    std::thread::sleep(Duration::from_secs_f64(wait));
                    last_err = Some(anyhow!(msg));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow!("all attempts failed (no error captured)")))
    }
}

fn validate_dims(width: u32, height: u32) -> Result<()> {
    if width < 768 || height < 768 {
        bail!("invalid dimensions {width}x{height}: each side must be >= 768");
    }
    if (width as u64) * (height as u64) > 1_048_576 {
        bail!(
            "invalid dimensions {width}x{height}: width*height must be <= 1,048,576 (got {})",
            (width as u64) * (height as u64)
        );
    }
    Ok(())
}

fn decode_image(parsed: &Value, raw_text: &str) -> Result<(Vec<u8>, String)> {
    let arr = parsed
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow!("response missing `data` array: {}", excerpt(raw_text)))?;
    let item = arr
        .first()
        .ok_or_else(|| anyhow!("response `data` array is empty: {}", excerpt(raw_text)))?;
    let b64 = item
        .get("b64_json")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("response item missing `b64_json`: {}", excerpt(raw_text)))?;
    let bytes = B64
        .decode(b64)
        .context("decoding base64 image data")?;
    let revised = item
        .get("revised_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok((bytes, revised))
}

fn retry_after_from_response(r: &ureq::Response, attempt: u32) -> f64 {
    // Try Azure-style millisecond headers first.
    for h in ["retry-after-ms", "x-ms-retry-after-ms"] {
        if let Some(v) = r.header(h) {
            if let Ok(ms) = v.trim().parse::<u64>() {
                return (ms as f64) / 1000.0;
            }
        }
    }
    // RFC 7231: Retry-After can be seconds or HTTP-date.
    if let Some(v) = r.header("Retry-After") {
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

fn excerpt(body: &str) -> String {
    if body.chars().count() <= RESPONSE_BODY_EXCERPT {
        body.replace('\n', " ")
    } else {
        let truncated: String = body.chars().take(RESPONSE_BODY_EXCERPT).collect();
        format!("{truncated}…")
    }
}
