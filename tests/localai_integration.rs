//! Live integration test against a LocalAI server (free, runs locally).
//!
//! This test is `#[ignore]` by default so `cargo test` doesn't require
//! Docker to be running. To run it:
//!
//! 1. Start LocalAI (CPU-only image, ~3 GB):
//!    ```sh
//!    docker run --rm -p 8080:8080 --name pixforge-localai localai/localai:latest
//!    ```
//!
//! 2. Install a small image model (one-time, ~5 minutes):
//!    ```sh
//!    curl http://localhost:8080/models/apply \
//!      -H 'Content-Type: application/json' \
//!      -d '{"id":"huggingface@stable-diffusion-1.5"}'
//!    ```
//!
//! 3. Wait for the model to finish downloading, then run:
//!    ```sh
//!    PIXFORGE_LOCALAI_URL=http://localhost:8080/v1 \
//!    PIXFORGE_LOCALAI_MODEL=stable-diffusion-1.5 \
//!      cargo test --test localai_integration -- --ignored --nocapture
//!    ```
//!
//! If `PIXFORGE_LOCALAI_URL` is unset, the test self-skips with a clear
//! message instead of failing.

use pixforge::providers::openai_compat::{AuthStyle, OpenaiCompatProvider};
use pixforge::providers::{ImageProvider, Request, Size};

#[test]
#[ignore = "requires a running LocalAI server; see file docs"]
fn localai_generates_a_real_image() {
    let Some(url) = std::env::var("PIXFORGE_LOCALAI_URL").ok() else {
        eprintln!(
            "PIXFORGE_LOCALAI_URL not set; skipping. \
             See tests/localai_integration.rs for setup."
        );
        return;
    };
    let model = std::env::var("PIXFORGE_LOCALAI_MODEL")
        .unwrap_or_else(|_| "stable-diffusion-1.5".to_string());

    let p = OpenaiCompatProvider {
        endpoint: url,
        api_key: None,
        auth_style: AuthStyle::None,
        // LocalAI on CPU is slow — generation can take 60–120s per image.
        timeout_secs: 240,
        max_attempts: 1,
    };

    let extra = serde_json::Map::new();
    let req = Request {
        prompt: "a tiny pixel-art forest, low detail",
        model: &model,
        n: 1,
        size: Some(Size {
            width: 256,
            height: 256,
        }),
        size_explicit: true,
        seed: None,
        negative_prompt: None,
        quality: None,
        extra: &extra,
    };

    let mut on_retry = |a: u32, m: &str, w: f64| {
        eprintln!("retry attempt {a}: {m} (sleep {w:.1}s)");
    };

    let result = p
        .generate(&req, &mut on_retry)
        .expect("LocalAI generation failed; is the model installed?");

    assert!(!result.images.is_empty(), "no images returned");
    let img = &result.images[0];
    assert!(
        !img.bytes.is_empty(),
        "image bytes are empty (got mime={})",
        img.mime_type
    );
    // Sanity-check the magic number — should be a PNG or JPEG.
    let header = &img.bytes[..img.bytes.len().min(8)];
    assert!(
        header.starts_with(b"\x89PNG") || header.starts_with(b"\xff\xd8"),
        "image bytes don't look like PNG or JPEG; got header {:?}",
        header
    );

    eprintln!(
        "LocalAI generated {} bytes ({}) in {:.1}s",
        img.bytes.len(),
        img.mime_type,
        result.latency_secs
    );
}
