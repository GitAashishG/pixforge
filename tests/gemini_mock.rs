//! Contract tests for the gemini native adapter against a local mock server.
//!
//! Verifies the camelCase request/response shape, the `x-goog-api-key`
//! header, blocked-by-safety responses, text-only-no-image responses,
//! and the explicit-size rejection.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use httpmock::prelude::*;
use serde_json::json;

use pixforge::providers::gemini::GeminiProvider;
use pixforge::providers::{ImageProvider, Request, Size};

const TINY_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn provider(server: &MockServer) -> GeminiProvider {
    GeminiProvider {
        endpoint: server.base_url(),
        api_key: "AIza-test".to_string(),
        timeout_secs: 5,
        max_attempts: 3,
    }
}

fn implicit_request<'a>(
    prompt: &'a str,
    model: &'a str,
    extra: &'a serde_json::Map<String, serde_json::Value>,
) -> Request<'a> {
    Request {
        prompt,
        model,
        n: 1,
        size: None,
        size_explicit: false,
        seed: None,
        negative_prompt: None,
        quality: None,
        extra,
    }
}

#[test]
fn happy_path_sends_correct_url_headers_body_and_parses_inline_data() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1beta/models/gemini-2.5-flash-image:generateContent")
            .header("x-goog-api-key", "AIza-test")
            .header("Content-Type", "application/json")
            .json_body(json!({
                "contents": [{ "parts": [{ "text": "a friendly fox" }] }],
                "generationConfig": { "responseModalities": ["IMAGE"] }
            }));
        then.status(200).json_body(json!({
            "candidates": [{
                "content": {
                    "parts": [
                        { "text": "Here is your image:" },
                        { "inlineData": { "mimeType": "image/png", "data": TINY_PNG_B64 } }
                    ]
                },
                "finishReason": "STOP"
            }]
        }));
    });
    let p = provider(&server);
    let mut nr = |_, _: &str, _| panic!("no retry expected");
    let r = p
        .generate(
            &implicit_request("a friendly fox", "gemini-2.5-flash-image", &extra),
            &mut nr,
        )
        .expect("ok");
    mock.assert();
    assert_eq!(r.images[0].bytes, B64.decode(TINY_PNG_B64).unwrap());
    assert_eq!(r.images[0].mime_type, "image/png");
    // Gemini doesn't have a "revised_prompt" concept.
    assert!(r.images[0].revised_prompt.is_none());
}

#[test]
fn explicit_size_is_rejected_before_http() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let mock = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });
    let p = provider(&server);
    let req = Request {
        prompt: "x",
        model: "m",
        n: 1,
        size: Some(Size {
            width: 1024,
            height: 1024,
        }),
        size_explicit: true,
        seed: None,
        negative_prompt: None,
        quality: None,
        extra: &extra,
    };
    let mut nr = |_, _: &str, _| {};
    let err = p
        .generate(&req, &mut nr)
        .expect_err("explicit size must fail");
    assert!(
        format!("{err}").contains("does not accept explicit width/height"),
        "got: {err}"
    );
    assert_eq!(mock.hits(), 0);
}

#[test]
fn implicit_size_is_silently_dropped() {
    // size_explicit=false; the canonical size is ignored and not transmitted.
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1beta/models/gemini-2.5-flash-image:generateContent")
            .matches(|req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body.as_deref().unwrap_or(&[])).unwrap();
                // Body must NOT contain width/height/size keys at any depth.
                let s = serde_json::to_string(&body).unwrap();
                !s.contains("width") && !s.contains("height") && !s.contains("\"size\"")
            });
        then.status(200).json_body(json!({
            "candidates": [{ "content": { "parts": [
                { "inlineData": { "mimeType": "image/png", "data": TINY_PNG_B64 } }
            ]}, "finishReason": "STOP" }]
        }));
    });
    let p = provider(&server);
    let req = Request {
        prompt: "x",
        model: "gemini-2.5-flash-image",
        n: 1,
        size: Some(Size {
            width: 1024,
            height: 1024,
        }),
        size_explicit: false, // came from defaults, not user
        seed: None,
        negative_prompt: None,
        quality: None,
        extra: &extra,
    };
    let mut nr = |_, _: &str, _| {};
    p.generate(&req, &mut nr).expect("ok");
    mock.assert();
}

#[test]
fn block_reason_at_top_level_is_surfaced() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(json!({
            "promptFeedback": { "blockReason": "SAFETY" }
        }));
    });
    let p = provider(&server);
    let mut nr = |_, _: &str, _| {};
    let err = p
        .generate(&implicit_request("x", "m", &extra), &mut nr)
        .expect_err("blockReason must surface as error");
    let msg = format!("{err}");
    assert!(msg.contains("blocked"), "got: {msg}");
    assert!(msg.contains("SAFETY"), "got: {msg}");
}

#[test]
fn text_only_no_image_returns_clear_error_with_finish_reason() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(json!({
            "candidates": [{
                "content": { "parts": [
                    { "text": "I cannot generate that image." }
                ]},
                "finishReason": "RECITATION"
            }]
        }));
    });
    let p = provider(&server);
    let mut nr = |_, _: &str, _| {};
    let err = p
        .generate(&implicit_request("x", "m", &extra), &mut nr)
        .expect_err("no-image must surface as error");
    let msg = format!("{err}");
    assert!(msg.contains("no image part"), "got: {msg}");
    assert!(msg.contains("RECITATION"), "got: {msg}");
    assert!(
        msg.contains("I cannot generate that image"),
        "should include text snippet, got: {msg}"
    );
}

#[test]
fn empty_candidates_array_errors_clearly() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(json!({"candidates": []}));
    });
    let p = provider(&server);
    let mut nr = |_, _: &str, _| {};
    let err = p
        .generate(&implicit_request("x", "m", &extra), &mut nr)
        .expect_err("must fail");
    assert!(format!("{err}").contains("`candidates` is empty"), "got: {err}");
}

#[test]
fn missing_candidates_field_errors_clearly() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(json!({"unrelated": true}));
    });
    let p = provider(&server);
    let mut nr = |_, _: &str, _| {};
    let err = p
        .generate(&implicit_request("x", "m", &extra), &mut nr)
        .expect_err("must fail");
    assert!(format!("{err}").contains("missing `candidates`"), "got: {err}");
}

#[test]
fn picks_first_inline_data_even_when_text_part_comes_first() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(json!({
            "candidates": [{
                "content": { "parts": [
                    { "text": "preamble" },
                    { "text": "more chatter" },
                    { "inlineData": { "mimeType": "image/jpeg", "data": TINY_PNG_B64 } }
                ]},
                "finishReason": "STOP"
            }]
        }));
    });
    let p = provider(&server);
    let mut nr = |_, _: &str, _| {};
    let r = p
        .generate(&implicit_request("x", "m", &extra), &mut nr)
        .expect("ok");
    assert_eq!(r.images[0].mime_type, "image/jpeg");
    assert_eq!(r.images[0].bytes, B64.decode(TINY_PNG_B64).unwrap());
}

#[test]
fn retries_on_429() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let m = server.mock(|when, then| {
        when.method(POST);
        then.status(429)
            .header("retry-after-ms", "10")
            .body("quota");
    });
    let p = provider(&server);
    let mut count = 0u32;
    let mut on_retry = |_a: u32, _m: &str, _w: f64| count += 1;
    let err = p
        .generate(&implicit_request("x", "m", &extra), &mut on_retry)
        .expect_err("must fail");
    assert!(format!("{err}").contains("HTTP 429"));
    assert_eq!(m.hits(), 3);
    assert_eq!(count, 2);
}
