//! Contract tests for the azure-openai provider against a local mock server.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use httpmock::prelude::*;
use serde_json::json;

use pixforge::providers::azure_openai::AzureOpenaiProvider;
use pixforge::providers::{ImageProvider, Request, Size};

const TINY_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn provider(server: &MockServer) -> AzureOpenaiProvider {
    AzureOpenaiProvider {
        endpoint: server.base_url(),
        api_version: "2024-02-01".to_string(),
        api_key: "az-key-xyz".to_string(),
        timeout_secs: 5,
        max_attempts: 3,
    }
}

fn make_request<'a>(
    prompt: &'a str,
    deployment: &'a str,
    extra: &'a serde_json::Map<String, serde_json::Value>,
) -> Request<'a> {
    Request {
        prompt,
        model: deployment,
        n: 1,
        size: Some(Size {
            width: 1024,
            height: 1024,
        }),
        size_explicit: false,
        seed: None,
        negative_prompt: None,
        quality: None,
        extra,
    }
}

#[test]
fn url_includes_deployment_and_api_version_and_uses_api_key_header() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/openai/deployments/dall-e-3/images/generations")
            .query_param("api-version", "2024-02-01")
            .header("api-key", "az-key-xyz")
            .header("Content-Type", "application/json")
            .json_body(json!({
                "prompt": "a serene lake at dawn",
                "n": 1,
                "size": "1024x1024",
                "response_format": "b64_json"
            }));
        then.status(200).json_body(json!({
            "data": [{
                "b64_json": TINY_PNG_B64,
                "revised_prompt": "a tranquil mountain lake at sunrise"
            }]
        }));
    });

    let p = provider(&server);
    let mut nr = |_, _: &str, _| panic!("no retry expected");
    let r = p
        .generate(&make_request("a serene lake at dawn", "dall-e-3", &extra), &mut nr)
        .expect("generate should succeed");
    mock.assert();
    assert_eq!(r.images[0].bytes, B64.decode(TINY_PNG_B64).unwrap());
    assert_eq!(
        r.images[0].revised_prompt.as_deref(),
        Some("a tranquil mountain lake at sunrise")
    );
}

#[test]
fn body_does_not_include_model_field() {
    // The Azure deployment is in the URL path, so the request body should
    // NOT include a `model` key (DALL·E on Azure rejects it).
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/openai/deployments/dall-e-3/images/generations")
            .matches(|req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body.as_deref().unwrap_or(&[])).unwrap();
                body.get("model").is_none()
            });
        then.status(200)
            .json_body(json!({"data": [{"b64_json": TINY_PNG_B64}]}));
    });
    let p = provider(&server);
    let mut nr = |_, _: &str, _| {};
    p.generate(&make_request("x", "dall-e-3", &extra), &mut nr)
        .expect("ok");
    mock.assert();
}

#[test]
fn url_response_falls_back_to_get() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let path = "/blob/img.png";
    let bytes = B64.decode(TINY_PNG_B64).unwrap();
    let cdn = server.mock(|when, then| {
        when.method(GET).path(path);
        then.status(200)
            .header("content-type", "image/png")
            .body(bytes.clone());
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/openai/deployments/dall-e-3/images/generations");
        then.status(200).json_body(json!({
            "data": [{ "url": format!("{}{}", server.base_url(), path) }]
        }));
    });
    let p = provider(&server);
    let mut nr = |_, _: &str, _| {};
    let r = p
        .generate(&make_request("x", "dall-e-3", &extra), &mut nr)
        .expect("ok");
    cdn.assert();
    assert_eq!(r.images[0].bytes, bytes);
}

#[test]
fn retries_on_429_with_x_ms_retry_after_ms_header() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let m = server.mock(|when, then| {
        when.method(POST)
            .path("/openai/deployments/dall-e-3/images/generations");
        then.status(429)
            .header("x-ms-retry-after-ms", "10")
            .body("throttled");
    });
    let p = provider(&server);
    let mut waits = Vec::new();
    let mut on_retry = |_a: u32, _m: &str, w: f64| waits.push(w);
    let err = p
        .generate(&make_request("x", "dall-e-3", &extra), &mut on_retry)
        .expect_err("must fail");
    assert!(format!("{err}").contains("HTTP 429"));
    assert_eq!(m.hits(), 3);
    for w in &waits {
        assert!(*w < 1.0, "expected ~0.01s from Azure-style header; got {w}");
    }
}

#[test]
fn missing_data_array_errors_clearly() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST)
            .path("/openai/deployments/dall-e-3/images/generations");
        then.status(200).json_body(json!({"oops": true}));
    });
    let p = provider(&server);
    let mut nr = |_, _: &str, _| {};
    let err = p
        .generate(&make_request("x", "dall-e-3", &extra), &mut nr)
        .expect_err("must fail");
    assert!(format!("{err}").contains("`data`"), "got: {err}");
}
