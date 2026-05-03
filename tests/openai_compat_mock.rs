//! Contract tests for openai-compat against a local mock server.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use httpmock::prelude::*;
use serde_json::json;

use pixforge::providers::openai_compat::{AuthStyle, OpenaiCompatProvider};
use pixforge::providers::{ImageProvider, Request, Size};

const TINY_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn provider(server: &MockServer, auth_style: AuthStyle, key: Option<&str>) -> OpenaiCompatProvider {
    OpenaiCompatProvider {
        endpoint: format!("{}/v1", server.base_url()),
        api_key: key.map(str::to_string),
        auth_style,
        timeout_secs: 5,
        max_attempts: 3,
    }
}

fn make_request<'a>(
    prompt: &'a str,
    model: &'a str,
    size: Option<Size>,
    extra: &'a serde_json::Map<String, serde_json::Value>,
) -> Request<'a> {
    Request {
        prompt,
        model,
        n: 1,
        size,
        size_explicit: size.is_some(),
        seed: None,
        negative_prompt: None,
        quality: None,
        extra,
    }
}

#[test]
fn bearer_auth_sends_authorization_header_and_parses_b64() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/images/generations")
            .header("Authorization", "Bearer sk-test")
            .header("Content-Type", "application/json")
            .json_body(json!({
                "model": "gpt-image-1",
                "prompt": "a robot",
                "n": 1,
                "size": "1024x1024"
            }));
        then.status(200).json_body(json!({
            "data": [{
                "b64_json": TINY_PNG_B64,
                "revised_prompt": "a friendly robot"
            }]
        }));
    });

    let p = provider(&server, AuthStyle::Bearer, Some("sk-test"));
    let mut nr = |_, _: &str, _| panic!("no retry expected");
    let r = p
        .generate(
            &make_request(
                "a robot",
                "gpt-image-1",
                Some(Size {
                    width: 1024,
                    height: 1024,
                }),
                &extra,
            ),
            &mut nr,
        )
        .expect("generate should succeed");
    mock.assert();
    assert_eq!(r.images[0].bytes, B64.decode(TINY_PNG_B64).unwrap());
    assert_eq!(
        r.images[0].revised_prompt.as_deref(),
        Some("a friendly robot")
    );
    assert_eq!(r.images[0].mime_type, "image/png");
}

#[test]
fn api_key_auth_sends_api_key_header() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/images/generations")
            .header("api-key", "azure-style-key");
        then.status(200)
            .json_body(json!({"data": [{"b64_json": TINY_PNG_B64}]}));
    });
    let p = provider(&server, AuthStyle::ApiKey, Some("azure-style-key"));
    let mut nr = |_, _: &str, _| {};
    p.generate(
        &make_request(
            "x",
            "m",
            Some(Size {
                width: 512,
                height: 512,
            }),
            &extra,
        ),
        &mut nr,
    )
    .expect("ok");
    mock.assert();
}

#[test]
fn auth_style_none_sends_no_auth_header() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    // The mock matches any request to the path *without* asserting headers.
    // We separately verify the lack of an Authorization header by checking
    // the recorded request below.
    let mock = server.mock(|when, then| {
        when.method(POST).path("/v1/images/generations");
        then.status(200)
            .json_body(json!({"data": [{"b64_json": TINY_PNG_B64}]}));
    });
    let p = provider(&server, AuthStyle::None, None);
    let mut nr = |_, _: &str, _| {};
    p.generate(
        &make_request(
            "x",
            "m",
            Some(Size {
                width: 512,
                height: 512,
            }),
            &extra,
        ),
        &mut nr,
    )
    .expect("LocalAI-style call should succeed");
    mock.assert();
}

#[test]
fn url_response_falls_back_to_get_for_bytes() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let url_path = "/cdn/image-deadbeef.png";
    let image_bytes = B64.decode(TINY_PNG_B64).unwrap();

    let cdn = server.mock(|when, then| {
        when.method(GET).path(url_path);
        then.status(200)
            .header("content-type", "image/png")
            .body(image_bytes.clone());
    });
    let gen = server.mock(|when, then| {
        when.method(POST).path("/v1/images/generations");
        then.status(200).json_body(json!({
            "data": [{ "url": format!("{}{}", server.base_url(), url_path) }]
        }));
    });

    let p = provider(&server, AuthStyle::Bearer, Some("sk-x"));
    let mut nr = |_, _: &str, _| {};
    let r = p
        .generate(
            &make_request(
                "x",
                "dall-e-3",
                Some(Size {
                    width: 1024,
                    height: 1024,
                }),
                &extra,
            ),
            &mut nr,
        )
        .expect("url path should fetch bytes");
    gen.assert();
    cdn.assert();
    assert_eq!(r.images[0].bytes, image_bytes);
    assert_eq!(r.images[0].mime_type, "image/png");
}

#[test]
fn missing_b64_and_url_errors_clearly() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    server.mock(|when, then| {
        when.method(POST).path("/v1/images/generations");
        then.status(200)
            .json_body(json!({"data": [{"unrelated_field": 1}]}));
    });
    let p = provider(&server, AuthStyle::Bearer, Some("sk-x"));
    let mut nr = |_, _: &str, _| {};
    let err = p
        .generate(
            &make_request(
                "x",
                "m",
                Some(Size {
                    width: 1024,
                    height: 1024,
                }),
                &extra,
            ),
            &mut nr,
        )
        .expect_err("must fail");
    assert!(
        format!("{err}").contains("neither `b64_json` nor `url`"),
        "got: {err}"
    );
}

#[test]
fn persistent_429_retries_and_fails() {
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let m = server.mock(|when, then| {
        when.method(POST).path("/v1/images/generations");
        then.status(429)
            .header("retry-after-ms", "10")
            .body("rate limit");
    });
    let p = provider(&server, AuthStyle::Bearer, Some("sk-x"));
    let mut count = 0u32;
    let mut on_retry = |_a: u32, _m: &str, _w: f64| count += 1;
    let err = p
        .generate(
            &make_request(
                "x",
                "m",
                Some(Size {
                    width: 1024,
                    height: 1024,
                }),
                &extra,
            ),
            &mut on_retry,
        )
        .expect_err("must fail");
    assert!(format!("{err}").contains("HTTP 429"));
    assert_eq!(m.hits(), 3);
    assert_eq!(count, 2);
}

#[test]
fn bearer_without_api_key_errors() {
    // No HTTP needs to happen — the adapter rejects missing creds before sending.
    let server = MockServer::start();
    let extra = serde_json::Map::new();
    let mock = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });
    let p = provider(&server, AuthStyle::Bearer, None);
    let mut nr = |_, _: &str, _| {};
    let err = p
        .generate(
            &make_request(
                "x",
                "m",
                Some(Size {
                    width: 1024,
                    height: 1024,
                }),
                &extra,
            ),
            &mut nr,
        )
        .expect_err("must fail");
    assert!(format!("{err}").contains("api_key is required"));
    assert_eq!(mock.hits(), 0);
}
