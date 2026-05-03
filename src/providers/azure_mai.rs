//! Microsoft AI (MAI) image generation on Azure AI Foundry.
//!
//! API contract:
//! - URL: `{endpoint}/mai/v1/images/generations?api-version={ver}`
//! - Header: `api-key: {key}`
//! - Body: `{"prompt", "width", "height", "n", "model"}`
//! - Response: `{"data": [{"b64_json", "revised_prompt"?}]}`
//! - Constraints: `width >= 768 && height >= 768 && width*height <= 1_048_576`

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

use super::{
    backoff_secs, build_agent, decode_b64, excerpt, parse_json_body, retry_after_from_headers,
    GeneratedImage, ImageProvider, Request, Response, Size,
};

const PROVIDER_ID: &str = "azure-mai";

pub struct AzureMaiProvider {
    pub endpoint: String,
    pub api_version: String,
    pub api_key: String,
    pub timeout_secs: u64,
    pub max_attempts: u32,
}

impl AzureMaiProvider {
    fn url(&self) -> String {
        format!(
            "{}/mai/v1/images/generations?api-version={}",
            self.endpoint.trim_end_matches('/'),
            self.api_version
        )
    }
}

#[derive(Serialize)]
struct Body<'a> {
    prompt: &'a str,
    width: u32,
    height: u32,
    n: u32,
    model: &'a str,
}

impl ImageProvider for AzureMaiProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn generate(
        &self,
        req: &Request<'_>,
        on_retry: &mut dyn FnMut(u32, &str, f64),
    ) -> Result<Response> {
        let size = req.size.ok_or_else(|| {
            anyhow!("azure-mai requires width and height (canonical Size)")
        })?;
        validate_dims(size)?;

        let body_json = serde_json::to_string(&Body {
            prompt: req.prompt,
            width: size.width,
            height: size.height,
            n: req.n,
            model: req.model,
        })
        .context("serializing request body")?;
        let url = self.url();

        let agent = build_agent(self.timeout_secs);
        let started = Instant::now();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=self.max_attempts {
            let attempt_started = Instant::now();
            let resp = agent
                .post(&url)
                .set("Content-Type", "application/json")
                .set("api-key", &self.api_key)
                .send_string(&body_json);

            match resp {
                Ok(r) => {
                    let status = r.status();
                    let text = r.into_string().context("reading 200 response body")?;
                    if status == 200 {
                        let parsed = parse_json_body(&text)?;
                        let (bytes, revised) = parse_data_b64(&parsed, &text)?;
                        return Ok(Response {
                            images: vec![GeneratedImage {
                                bytes,
                                revised_prompt: revised,
                                mime_type: "image/png".to_string(),
                            }],
                            latency_secs: started.elapsed().as_secs_f64(),
                            attempts: attempt,
                        });
                    }
                    bail!("HTTP {status}: {}", excerpt(&text));
                }
                Err(ureq::Error::Status(code, r)) => {
                    let retryable = code == 429 || (500..=599).contains(&code);
                    let header_lookup = |k: &str| r.header(k).map(|s| s.to_string());
                    let retry_after = retry_after_from_headers(&header_lookup, attempt);
                    let body = r.into_string().unwrap_or_default();
                    let msg = format!("HTTP {code}: {}", excerpt(&body));
                    if !retryable || attempt == self.max_attempts {
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
                    if attempt == self.max_attempts {
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

fn validate_dims(size: Size) -> Result<()> {
    if size.width < 768 || size.height < 768 {
        bail!(
            "invalid dimensions {}x{}: each side must be >= 768",
            size.width,
            size.height
        );
    }
    if (size.width as u64) * (size.height as u64) > 1_048_576 {
        bail!(
            "invalid dimensions {}x{}: width*height must be <= 1,048,576 (got {})",
            size.width,
            size.height,
            (size.width as u64) * (size.height as u64)
        );
    }
    Ok(())
}

fn parse_data_b64(parsed: &serde_json::Value, raw_text: &str) -> Result<(Vec<u8>, Option<String>)> {
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
    let bytes = decode_b64(b64)?;
    let revised = item
        .get("revised_prompt")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok((bytes, revised))
}
