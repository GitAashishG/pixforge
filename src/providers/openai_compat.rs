//! OpenAI-compatible image generation provider.
//!
//! Covers OpenAI proper, LocalAI (with `auth_style = "none"`), and any
//! third-party service exposing `POST {endpoint}/images/generations` in
//! OpenAI's shape:
//!
//! - Header: `Authorization: Bearer <key>` (default), `api-key: <key>`, or none
//! - Body: `{"model", "prompt", "n", "size": "WxH"}`
//!   We deliberately do NOT send `response_format`. OpenAI's gpt-image-*
//!   models reject it; DALL·E variants accept it but default sensibly
//!   (url for DALL·E 3, b64_json for gpt-image-*). Our response parser
//!   handles either case (b64 inline or URL-fetch fallback).
//! - Response: `{"data": [{"b64_json": "..." | "url": "..."}]}`
//!
//! When the response contains `url` instead of `b64_json`, we GET the URL
//! to fetch the image bytes, propagating retries on transient failures.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

use super::{
    backoff_secs, build_agent, decode_b64, excerpt, parse_json_body, retry_after_from_headers,
    GeneratedImage, ImageProvider, Request, Response,
};

const PROVIDER_ID: &str = "openai-compat";

/// Authentication header style used by the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    Bearer,
    ApiKey,
    None,
}

pub struct OpenaiCompatProvider {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub auth_style: AuthStyle,
    pub timeout_secs: u64,
    pub max_attempts: u32,
}

impl OpenaiCompatProvider {
    fn url(&self) -> String {
        format!("{}/images/generations", self.endpoint.trim_end_matches('/'))
    }

    fn apply_auth(&self, mut req: ureq::Request) -> Result<ureq::Request> {
        match self.auth_style {
            AuthStyle::None => Ok(req),
            AuthStyle::Bearer => {
                let key = self.api_key.as_deref().ok_or_else(|| {
                    anyhow!("openai-compat: api_key is required when auth_style = bearer")
                })?;
                req = req.set("Authorization", &format!("Bearer {key}"));
                Ok(req)
            }
            AuthStyle::ApiKey => {
                let key = self.api_key.as_deref().ok_or_else(|| {
                    anyhow!("openai-compat: api_key is required when auth_style = api-key")
                })?;
                req = req.set("api-key", key);
                Ok(req)
            }
        }
    }
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    prompt: &'a str,
    n: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
}

impl ImageProvider for OpenaiCompatProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn generate(
        &self,
        req: &Request<'_>,
        on_retry: &mut dyn FnMut(u32, &str, f64),
    ) -> Result<Response> {
        let body = Body {
            model: req.model,
            prompt: req.prompt,
            n: req.n,
            size: req.size.map(|s| s.as_string()),
        };
        let body_json = serde_json::to_string(&body).context("serializing request body")?;
        let url = self.url();

        let agent = build_agent(self.timeout_secs);
        let started = Instant::now();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=self.max_attempts {
            let attempt_started = Instant::now();
            let request_builder = agent
                .post(&url)
                .set("Content-Type", "application/json");
            let request_builder = self.apply_auth(request_builder)?;
            let resp = request_builder.send_string(&body_json);

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

/// Pull image bytes out of the response. Prefers `data[0].b64_json` (which
/// is what we asked for); falls back to GETting `data[0].url` if the server
/// returned a URL instead.
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
        let bytes = decode_b64(b64)?;
        return Ok((bytes, revised, "image/png".to_string()));
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
