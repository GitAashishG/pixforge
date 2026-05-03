//! Azure OpenAI image generation provider.
//!
//! Two URL dialects are supported:
//!
//! - **`deployment`** (legacy, default): used by DALL·E 2 / DALL·E 3.
//!   - URL: `{endpoint}/openai/deployments/{model}/images/generations?api-version={ver}`
//!   - Body: `{"prompt", "n", "size"}` — model is in the URL, NOT the body.
//!   - Requires a dated `api_version` like `2024-02-01`.
//!
//! - **`v1`** (modern, required for `gpt-image-1` / `gpt-image-2`): the new
//!   Azure-OpenAI v1 API. Released ~Aug 2025. Older dated api-versions hang
//!   indefinitely on these models because the legacy URL doesn't recognize
//!   them.
//!   - URL: `{endpoint}/openai/v1/images/generations` (no api-version)
//!   - Body: `{"model", "prompt", "n", "size"}` — model goes in the body.
//!
//! In both dialects: header is `api-key: {key}`. We do NOT send
//! `response_format` (gpt-image-* rejects it; DALL·E variants default
//! sensibly and our parser handles both b64 and url responses).
//!
//! Response shape (both dialects): `{"data": [{"b64_json"|"url", "revised_prompt"?}]}`

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

use super::{
    backoff_secs, build_agent, decode_b64, excerpt, parse_json_body, retry_after_from_headers,
    GeneratedImage, ImageProvider, Request, Response,
};

const PROVIDER_ID: &str = "azure-openai";

/// Which Azure OpenAI URL dialect this profile speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Legacy `/openai/deployments/{name}/images/generations?api-version=...`.
    /// Required for DALL·E 2 / DALL·E 3.
    Deployment,
    /// Modern `/openai/v1/images/generations` (no api-version).
    /// Required for gpt-image-1, gpt-image-2 and any post-2025 image model.
    V1,
}

pub struct AzureOpenaiProvider {
    pub endpoint: String,
    /// Only consulted when `dialect = Dialect::Deployment`.
    pub api_version: String,
    pub api_key: String,
    pub dialect: Dialect,
    pub timeout_secs: u64,
    pub max_attempts: u32,
}

impl AzureOpenaiProvider {
    fn url(&self, deployment: &str) -> String {
        let base = self.endpoint.trim_end_matches('/');
        match self.dialect {
            Dialect::Deployment => format!(
                "{}/openai/deployments/{}/images/generations?api-version={}",
                base, deployment, self.api_version
            ),
            Dialect::V1 => format!("{}/openai/v1/images/generations", base),
        }
    }
}

#[derive(Serialize)]
struct DeploymentBody<'a> {
    prompt: &'a str,
    n: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality: Option<&'a str>,
}

#[derive(Serialize)]
struct V1Body<'a> {
    model: &'a str,
    prompt: &'a str,
    n: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality: Option<&'a str>,
}

impl ImageProvider for AzureOpenaiProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn generate(
        &self,
        req: &Request<'_>,
        on_retry: &mut dyn FnMut(u32, &str, f64),
    ) -> Result<Response> {
        let body_json = match self.dialect {
            Dialect::Deployment => serde_json::to_string(&DeploymentBody {
                prompt: req.prompt,
                n: req.n,
                size: req.size.map(|s| s.as_string()),
                quality: req.quality,
            }),
            Dialect::V1 => serde_json::to_string(&V1Body {
                model: req.model,
                prompt: req.prompt,
                n: req.n,
                size: req.size.map(|s| s.as_string()),
                quality: req.quality,
            }),
        }
        .context("serializing request body")?;
        let url = self.url(req.model);

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
                        let (bytes, revised, mime) = parse_image_data(&parsed, &text, &agent)?;
                        return Ok(Response {
                            images: vec![GeneratedImage {
                                bytes,
                                revised_prompt: revised,
                                mime_type: mime,
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

/// Same shape as openai-compat — DALL·E on Azure can return `b64_json` or
/// `url`. We prefer b64; fall back to GETting url.
fn parse_image_data(
    parsed: &serde_json::Value,
    raw_text: &str,
    agent: &ureq::Agent,
) -> Result<(Vec<u8>, Option<String>, String)> {
    let arr = parsed
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow!("response missing `data` array: {}", excerpt(raw_text)))?;
    let item = arr
        .first()
        .ok_or_else(|| anyhow!("response `data` array is empty: {}", excerpt(raw_text)))?;
    let revised = item
        .get("revised_prompt")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    if let Some(b64) = item.get("b64_json").and_then(|v| v.as_str()) {
        return Ok((decode_b64(b64)?, revised, "image/png".to_string()));
    }
    if let Some(url) = item.get("url").and_then(|v| v.as_str()) {
        let resp = agent
            .get(url)
            .call()
            .with_context(|| format!("fetching image bytes from {url}"))?;
        let mime = resp
            .header("content-type")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "image/png".to_string());
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes)
            .with_context(|| format!("reading image bytes from {url}"))?;
        return Ok((bytes, revised, mime));
    }
    Err(anyhow!(
        "response item has neither `b64_json` nor `url`: {}",
        excerpt(raw_text)
    ))
}
