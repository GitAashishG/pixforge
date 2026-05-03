//! Contract tests for the azure-mai provider against a local mock server.
//!
//! Verifies URL/header/body shape, response parsing, retry on 429+5xx
//! (with `retry-after-ms` honored), and dimension validation.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use httpmock::prelude::*;
use serde_json::json;

use pixforge::providers::azure_mai::AzureMaiProvider;
use pixforge::providers::{ImageProvider, Request, Size};

/// Minimal valid PNG bytes, base64-encoded. Real Azure MAI returns much
/// larger PNGs but for contract tests any valid base64 round-trips.
const TINY_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn provider(server: &MockServer, max_attempts: u32) -> AzureMaiProvider {
    AzureMaiProvider {
        endpoint: server.base_url(),
        api_version: "preview".to_string(),
        api_key: "test-key-123".to_string(),
        timeout_secs: 5,
        max_attempts,
    }
}

fn make_request<'a>(
    prompt: &'a str,
    model: &'a str,
    size: Size,
    extra: &'a serde_json::Map<String, serde_json::Value>,
) -> Request<'a> {
    Request {
        prompt,
        model,
        n: 1,
        size: Some(size),
        size_explicit: false,
        seed: None,
        negative_prompt: None,
        quality: None,
        extra,
    }
}

fn default_size() -> Size {
    Size {
        width: 1024,
        height: 1024,
    }
}

fn no_retry_callback() -> impl FnMut(u32, &str, f64) {
    |_a, _m, _w| panic!("did not expect a retry")
}

#[test]
fn happy_path_sends_correct_url_headers_body_and_parses_response() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/mai/v1/images/generations")
            .query_param("api-version", "preview")
            .header("api-key", "test-key-123")
            .header("Content-Type", "application/json")
            .json_body(json!({
                "prompt": "a cat",
                "width": 1024,
                "height": 1024,
                "n": 1,
                "model": "MAI-Image-2"
            }));
        then.status(200).json_body(json!({
            "data": [{
                "b64_json": TINY_PNG_B64,
                "revised_prompt": "a fluffy cat"
            }]
        }));
    });

    let p = provider(&server, 3);
    let mut on_retry = no_retry_callback();
    let r = p
        .generate(
            &make_request("a cat", "MAI-Image-2", default_size(), &extra),
            &mut on_retry,
        )
        .expect("generate should succeed");

    mock.assert();
    assert_eq!(r.attempts, 1);
    assert_eq!(r.images.len(), 1);
    let img = &r.images[0];
    assert_eq!(img.bytes, B64.decode(TINY_PNG_B64).unwrap());
    assert_eq!(img.revised_prompt.as_deref(), Some("a fluffy cat"));
    assert_eq!(img.mime_type, "image/png");
}

#[test]
fn revised_prompt_is_optional() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST).path("/mai/v1/images/generations");
        then.status(200)
            .json_body(json!({"data": [{"b64_json": TINY_PNG_B64}]}));
    });
    let p = provider(&server, 3);
    let mut nr = no_retry_callback();
    let r = p
        .generate(
            &make_request("x", "m", default_size(), &extra),
            &mut nr,
        )
        .expect("missing revised_prompt should still parse");
    assert!(r.images[0].revised_prompt.is_none());
}

#[test]
fn persistent_429_fails_after_max_attempts_and_callback_fires() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let m = server.mock(|when, then| {
        when.method(POST).path("/mai/v1/images/generations");
        then.status(429)
            .header("retry-after-ms", "10")
            .body("rate limited");
    });
    let p = provider(&server, 3);
    let mut count = 0u32;
    let mut waits: Vec<f64> = Vec::new();
    let mut on_retry = |_a: u32, _msg: &str, w: f64| {
        count += 1;
        waits.push(w);
    };
    let err = p
        .generate(&make_request("x", "m", default_size(), &extra), &mut on_retry)
        .expect_err("persistent 429 should fail");
    assert!(format!("{err}").contains("HTTP 429"), "got: {err}");
    assert_eq!(m.hits(), 3);
    // Two retries between three attempts.
    assert_eq!(count, 2);
    // retry-after-ms = 10 → 0.01s sleep; verify both waits used the header.
    for w in &waits {
        assert!(*w < 1.0, "expected ~0.01s from retry-after-ms; got {w}");
    }
}

#[test]
fn persistent_500_also_retries() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let m = server.mock(|when, then| {
        when.method(POST).path("/mai/v1/images/generations");
        then.status(500).body("oops");
    });
    let p = provider(&server, 2);
    let mut nr = |_, _: &str, _| {};
    let err = p
        .generate(&make_request("x", "m", default_size(), &extra), &mut nr)
        .expect_err("persistent 500 should fail");
    assert!(format!("{err}").contains("HTTP 500"), "got: {err}");
    assert_eq!(m.hits(), 2);
}

#[test]
fn non_retryable_400_fails_immediately() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let m = server.mock(|when, then| {
        when.method(POST).path("/mai/v1/images/generations");
        then.status(400).body("bad prompt");
    });
    let p = provider(&server, 5);
    let mut on_retry = no_retry_callback();
    let err = p
        .generate(&make_request("x", "m", default_size(), &extra), &mut on_retry)
        .expect_err("400 should not retry");
    assert!(format!("{err}").contains("HTTP 400"), "got: {err}");
    assert_eq!(m.hits(), 1);
}

#[test]
fn dim_validation_too_small_fails_before_http() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let mock = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });
    let p = provider(&server, 1);
    let r = make_request(
        "x",
        "m",
        Size {
            width: 100,
            height: 100,
        },
        &extra,
    );
    let mut nr = no_retry_callback();
    let err = p.generate(&r, &mut nr).expect_err("100x100 must fail");
    assert!(format!("{err}").contains(">= 768"), "got: {err}");
    assert_eq!(mock.hits(), 0, "should not have hit the server");
}

#[test]
fn dim_validation_area_too_large_fails() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let p = provider(&server, 1);
    let r = make_request(
        "x",
        "m",
        Size {
            width: 2048,
            height: 2048,
        },
        &extra,
    );
    let mut nr = no_retry_callback();
    let err = p.generate(&r, &mut nr).expect_err("2048*2048 must fail");
    assert!(format!("{err}").contains("1,048,576"), "got: {err}");
}

#[test]
fn missing_b64_json_in_data_errors_clearly() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST).path("/mai/v1/images/generations");
        then.status(200)
            .json_body(json!({"data": [{"some_other_field": "oops"}]}));
    });
    let p = provider(&server, 1);
    let mut nr = no_retry_callback();
    let err = p
        .generate(&make_request("x", "m", default_size(), &extra), &mut nr)
        .expect_err("missing b64_json must fail");
    assert!(format!("{err}").contains("b64_json"), "got: {err}");
}

#[test]
fn empty_data_array_errors_clearly() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST).path("/mai/v1/images/generations");
        then.status(200).json_body(json!({"data": []}));
    });
    let p = provider(&server, 1);
    let mut nr = no_retry_callback();
    let err = p
        .generate(&make_request("x", "m", default_size(), &extra), &mut nr)
        .expect_err("empty data array must fail");
    assert!(format!("{err}").contains("empty"), "got: {err}");
}
