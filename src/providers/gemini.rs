//! Google Gemini native image generation provider.
//!
//! Google's OpenAI-compatible layer is officially chat + embeddings only;
//! image generation requires the *native* API.
//!
//! API contract:
//! - URL: `{endpoint}/v1beta/models/{model}:generateContent`
//!   (default endpoint `https://generativelanguage.googleapis.com`)
//! - Header: `x-goog-api-key: {key}`
//! - Body:
//!   ```json
//!   {
//!     "contents": [{ "parts": [{ "text": "<prompt>" }] }],
//!     "generationConfig": { "responseModalities": ["IMAGE"] }
//!   }
//!   ```
//! - Successful response (camelCase!):
//!   ```json
//!   {
//!     "candidates": [{
//!       "content": {
//!         "parts": [
//!           { "text": "..." },
//!           { "inlineData": { "mimeType": "image/png", "data": "<base64>" } }
//!         ]
//!       },
//!       "finishReason": "STOP"
//!     }]
//!   }
//!   ```
//! - Blocked response: top-level `promptFeedback.blockReason` set, no candidates.
//! - Gemini does not accept `width`/`height`; if the user explicitly set
//!   `-W` / `-H`, we error out before sending.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

use super::{
    backoff_secs, build_agent, decode_b64, excerpt, parse_json_body, retry_after_from_headers,
    GeneratedImage, ImageProvider, Request, Response,
};

const PROVIDER_ID: &str = "gemini";

pub struct GeminiProvider {
    pub endpoint: String,
    pub api_key: String,
    pub timeout_secs: u64,
    pub max_attempts: u32,
}

impl GeminiProvider {
    fn url(&self, model: &str) -> String {
        format!(
            "{}/v1beta/models/{}:generateContent",
            self.endpoint.trim_end_matches('/'),
            model
        )
    }
}

#[derive(Serialize)]
struct Body<'a> {
    contents: [Content<'a>; 1],
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
struct Content<'a> {
    parts: [Part<'a>; 1],
}

#[derive(Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct GenerationConfig {
    #[serde(rename = "responseModalities")]
    response_modalities: [&'static str; 1],
}

impl ImageProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn generate(
        &self,
        req: &Request<'_>,
        on_retry: &mut dyn FnMut(u32, &str, f64),
    ) -> Result<Response> {
        if req.size_explicit {
            bail!(
                "gemini does not accept explicit width/height. \
                 Drop -W/-H or use a different provider."
            );
        }

        let body = Body {
            contents: [Content {
                parts: [Part { text: req.prompt }],
            }],
            generation_config: GenerationConfig {
                response_modalities: ["IMAGE"],
            },
        };
        let body_json = serde_json::to_string(&body).context("serializing request body")?;
        let url = self.url(req.model);

        let agent = build_agent(self.timeout_secs);
        let started = Instant::now();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=self.max_attempts {
            let attempt_started = Instant::now();
            let resp = agent
                .post(&url)
                .set("Content-Type", "application/json")
                .set("x-goog-api-key", &self.api_key)
                .send_string(&body_json);

            match resp {
                Ok(r) => {
                    let status = r.status();
                    let text = r.into_string().context("reading 200 response body")?;
                    if status == 200 {
                        let parsed = parse_json_body(&text)?;
                        let (bytes, mime) = parse_gemini_response(&parsed, &text)?;
                        return Ok(Response {
                            images: vec![GeneratedImage {
                                bytes,
                                revised_prompt: None,
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

/// Walk a Gemini `generateContent` response and pull out the first image part.
///
/// Failure modes (in priority order):
/// 1. `promptFeedback.blockReason` set → blocked-by-safety error.
/// 2. No candidates → error mentioning whatever metadata is present.
/// 3. Candidate exists but no `inlineData` part → error including any text
///    parts and `finishReason` to help the user understand why no image
///    came back.
fn parse_gemini_response(parsed: &serde_json::Value, raw_text: &str) -> Result<(Vec<u8>, String)> {
    if let Some(reason) = parsed
        .get("promptFeedback")
        .and_then(|f| f.get("blockReason"))
        .and_then(|v| v.as_str())
    {
        bail!("gemini blocked the request: blockReason = {reason}");
    }

    let candidates = parsed
        .get("candidates")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("response missing `candidates`: {}", excerpt(raw_text)))?;
    let cand = candidates
        .first()
        .ok_or_else(|| anyhow!("response `candidates` is empty: {}", excerpt(raw_text)))?;

    let parts = cand
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .ok_or_else(|| anyhow!("candidate missing content.parts: {}", excerpt(raw_text)))?;

    let mut text_snippets: Vec<String> = Vec::new();
    for part in parts {
        if let Some(inline) = part.get("inlineData") {
            let data = inline
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("inlineData missing `data`: {}", excerpt(raw_text)))?;
            let mime = inline
                .get("mimeType")
                .and_then(|v| v.as_str())
                .unwrap_or("image/png")
                .to_string();
            return Ok((decode_b64(data)?, mime));
        }
        if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
            text_snippets.push(t.to_string());
        }
    }

    let finish = cand
        .get("finishReason")
        .and_then(|v| v.as_str())
        .unwrap_or("<none>");
    let combined_text = if text_snippets.is_empty() {
        "<no text>".to_string()
    } else {
        text_snippets.join(" / ")
    };
    Err(anyhow!(
        "gemini returned no image part (finishReason={finish}, text={combined_text})"
    ))
}
