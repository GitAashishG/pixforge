# `imagine`

A fast, single-binary CLI for generating images via Microsoft's MAI image
models on Azure AI Foundry.

```bash
imagine -p "Suburban house with car parked upfront"
# → ./imagine-20260502-225036-688df5.png  (and opens it in your default viewer)
```

Built in Rust on a synchronous HTTP stack (`ureq` + `rustls`) for fast cold
starts and a small binary.

## Install

Requires Rust 1.74+.

```bash
cargo install --path .
# or build manually:
cargo build --release
# binary: ./target/release/imagine
```

## First-time setup

Generate a starter config and edit it to add your Azure key:

```bash
imagine init
# wrote starter config to /Users/you/.config/imagine/config.toml

$EDITOR "$(imagine config-path)"
```

`config.toml`:

```toml
endpoint   = "https://your-resource.services.ai.azure.com"
deployment = "your-deployment-name"
api_key    = "your-azure-api-key"
# api_version  = "preview"
# width        = 1024
# height       = 1024
# timeout_secs = 180
# max_attempts = 5
```

`endpoint`, `deployment`, and `api_key` are **required** — there are no defaults.
You can find your endpoint and deployment name in the
[Azure AI Foundry portal](https://ai.azure.com) for your image-generation
resource.

The config file lives at `$XDG_CONFIG_HOME/imagine/config.toml`, defaulting
to `~/.config/imagine/config.toml` on every OS (use `imagine config-path` to
print the resolved location).

### Environment variables

These override the config file, in this order of precedence:
**CLI flags > env vars > config file > built-in defaults**.

| Variable          | Equivalent in `config.toml` |
|-------------------|-----------------------------|
| `AZURE_API_KEY`   | `api_key`                   |
| `MAI_ENDPOINT`    | `endpoint`                  |
| `MAI_DEPLOYMENT`  | `deployment`                |
| `MAI_API_VERSION` | `api_version`               |
| `XDG_CONFIG_HOME` | (locates the config file)   |

## Usage

```text
imagine [OPTIONS] -p <PROMPT>

Options:
  -p, --prompt <PROMPT>          Prompt text. Use `-` to read from stdin.
  -o, --output <PATH>            Output PNG path (default: ./imagine-{ts}-{hash}.png)
  -m, --model <NAME>             Override deployment (e.g. MAI-Image-2e)
  -W, --width  <N>               Width in px (>= 768)
  -H, --height <N>               Height in px (>= 768; W*H <= 1,048,576)
      --endpoint <URL>           Override Azure endpoint
      --api-version <STR>        API version (default: preview)
      --timeout <SECS>           HTTP timeout (default: 180)
      --max-attempts <N>         Retries on 429/5xx/transport (default: 5)
      --no-open                  Don't open the image after generation
  -q, --quiet                    Suppress progress on stderr
```

### Examples

Basic:

```bash
imagine -p "Cyberpunk Tokyo street at night, neon, rain"
```

Pipe a long prompt from a file:

```bash
cat my-prompt.txt | imagine -p -
```

Pick a different deployment and a non-square size:

```bash
imagine -p "watercolor mountains" -m MAI-Image-2e -W 1024 -H 1024
```

Scriptable usage (only the path is on stdout):

```bash
img=$(imagine -p "robot chef" --no-open --quiet)
mv "$img" hero.png
```

## Output

For every successful generation, two files are written:

- `<output>.png` — the image
- `<output>.png.prompt.json` — sidecar with `prompt`, `revised_prompt`,
  `deployment`, `width`, `height`, `latency_s`, `attempts`, `generated_at`.

Both are written atomically (`*.tmp` then `rename`).

## Behavior notes

- **Retries** on HTTP 429/5xx and on transport errors (DNS/TLS/timeout) with
  exponential backoff (1, 2, 4, 8, 16s + jitter, capped at 60s). Honors
  `Retry-After`, `retry-after-ms`, and `x-ms-retry-after-ms` headers.
- **Validation**: width and height must each be ≥ 768, and `width * height`
  must be ≤ 1,048,576 (Azure's MAI constraint).
- **Exit codes**: `0` success, `1` runtime error (network, decoding, etc.),
  `2` config / usage error.
- **stdout vs stderr**: progress and warnings go to stderr; the final image
  path is the only thing on stdout, so `$(imagine -p …)` is safe in scripts.

## Cost & quota (Azure MAI Tier 1)

| Model         | RPM | Per-image cost |
|---------------|----:|---------------:|
| `MAI-Image-2`  | 9  | ~$0.04         |
| `MAI-Image-2e` | 18 | ~$0.01         |

`MAI-Image-2` is higher fidelity; `MAI-Image-2e` is faster and cheaper. Set
your default in `config.toml` (`deployment = "..."`) or override per call
with `-m`.
