# `pixforge`

A fast, single-binary CLI for generating images from text prompts. Speaks
multiple providers natively — switch between **OpenAI**, **Azure OpenAI**,
**Azure MAI**, **Google Gemini**, and **LocalAI** by changing one config
value, no code or env-var juggling required.

```bash
pixforge -p "Suburban house with car parked upfront"
# → ./pixforge-20260503-100201-688df5.png  (and opens it in your default viewer)

pixforge --profile gemini -p "Watercolor mountains at dawn"
# → uses your Gemini profile instead
```

Built in Rust on a synchronous HTTP stack (`ureq` + `rustls`) for fast cold
starts and a small binary.

## Install

### Shell installer (any Unix — recommended for v0.2)

```bash
curl -fsSL https://github.com/GitAashishG/pixforge/releases/latest/download/pixforge-installer.sh | sh
```

This downloads the right binary for your platform (macOS arm64/x86_64, Linux
arm64/x86_64) and installs it into `$CARGO_HOME/bin` (defaults to
`~/.cargo/bin`).

### From crates.io (Rust users)

```bash
cargo install pixforge
```

### Homebrew

> Coming in a follow-up release. Currently the binary ships via the shell
> installer above and via crates.io.

### Shell completions and man page

After installing, set up completions and the man page from the binary itself:

```bash
# bash
pixforge completions bash | sudo tee /usr/local/etc/bash_completion.d/pixforge >/dev/null

# zsh (adjust the path to a directory in your $fpath)
pixforge completions zsh > "${fpath[1]}/_pixforge"

# fish
pixforge completions fish > ~/.config/fish/completions/pixforge.fish

# man page
pixforge man | sudo tee /usr/local/share/man/man1/pixforge.1 >/dev/null
```

### Build from source

```bash
git clone https://github.com/GitAashishG/pixforge.git
cd pixforge
cargo build --release
# binary: ./target/release/pixforge
```

## First-time setup

Generate a starter config and pick which provider(s) you want to enable:

```bash
pixforge init
# wrote starter config to ~/.config/pixforge/config.toml

$EDITOR "$(pixforge config-path)"
```

Uncomment the profile(s) you want to use, set the env-var name for each
credential, then export the matching env vars in your shell profile.

`config.toml` example with two providers:

```toml
default_profile = "azure-mai"

[profile.azure-mai]
provider     = "azure-mai"
endpoint     = "https://your-resource.services.ai.azure.com"
model        = "MAI-Image-2"
api_key_env  = "AZURE_API_KEY"
api_version  = "preview"

[profile.openai]
provider     = "openai-compat"
endpoint     = "https://api.openai.com/v1"
model        = "gpt-image-1"
api_key_env  = "OPENAI_API_KEY"
```

Then in your shell:

```bash
export AZURE_API_KEY="..."
export OPENAI_API_KEY="..."
pixforge -p "a cyberpunk Tokyo street"                # uses default_profile
pixforge --profile openai -p "a cyberpunk Tokyo street"
```

The config file lives at `$XDG_CONFIG_HOME/pixforge/config.toml`, defaulting
to `~/.config/pixforge/config.toml` on every OS. Run `pixforge config-path`
to see the resolved location.

> **Security note:** literal `api_key = "..."` is **rejected** in
> `config.toml` to keep committed configs safe. Only `api_key_env = "VAR"`
> is allowed; the actual secret is read from the environment at request time.

## Supported providers

| Provider       | `provider =` value | Auth header             | Notes                                                                  |
|----------------|--------------------|--------------------------|------------------------------------------------------------------------|
| Azure MAI      | `azure-mai`        | `api-key`                | Microsoft AI image models on Azure AI Foundry. Uses `width`/`height`. |
| OpenAI         | `openai-compat`    | `Authorization: Bearer`  | DALL·E, gpt-image-*. Also covers Gemini's OpenAI-compat layer for chat (NOT image gen — use `provider = "gemini"` for Imagen). |
| Azure OpenAI   | `azure-openai`     | `api-key`                | DALL·E etc. via Azure deployments. Requires `api_version`.            |
| Google Gemini  | `gemini`           | `x-goog-api-key`         | Native API for `gemini-*-image` and Imagen models. Image size is decided by Gemini — passing `-W`/`-H` is rejected. |
| LocalAI        | `openai-compat`    | none                     | Run image gen locally via Docker. See [LocalAI setup](#localai-setup). |

### LocalAI setup

LocalAI is the easiest way to run a real OpenAI-compatible image-gen server
on your own machine. CPU-only example (no GPU required):

```bash
docker run -p 8080:8080 --rm localai/localai:latest
# In another shell, install a small image model:
curl http://localhost:8080/models/apply -H "Content-Type: application/json" \
  -d '{"id": "huggingface@stable-diffusion-1.5"}'
```

Then add this to your `config.toml`:

```toml
[profile.local]
provider     = "openai-compat"
endpoint     = "http://localhost:8080/v1"
model        = "stable-diffusion-1.5"
auth_style   = "none"
```

```bash
pixforge --profile local -p "a cozy library"
```

### Environment variables

These override any config file values, in this order of precedence:
**CLI flags > env vars > config file > built-in defaults**.

| Variable             | Effect                                |
|----------------------|----------------------------------------|
| `PIXFORGE_PROFILE`   | Picks the profile when no `--profile` flag is given |
| `XDG_CONFIG_HOME`    | Locates the config file                |
| (the env var named in `api_key_env`) | Provides the credential at request time |

## Usage

```text
pixforge [OPTIONS] -p <PROMPT>

Options:
  -p, --prompt <PROMPT>          Prompt text. Use `-` to read from stdin.
  -o, --output <PATH>            Output PNG path (default: ./pixforge-{ts}-{hash}.png)
      --profile <NAME>           Pick a profile from your config
  -m, --model <NAME>             Override model / deployment for this call
  -W, --width  <N>               Width  in px (validated per-provider)
  -H, --height <N>               Height in px (validated per-provider)
      --endpoint <URL>           Override provider endpoint
      --api-version <STR>        Override API version (where applicable)
      --timeout <SECS>           HTTP timeout (default: 180)
      --max-attempts <N>         Retries on 429/5xx/transport (default: 5)
      --no-open                  Don't open the image after generation
  -q, --quiet                    Suppress progress on stderr

Subcommands:
  init                  Write a starter config file
  config-path           Print the resolved config path
  profiles              List profiles in your config
  profile show <name>   Show a profile's resolved settings (api_key masked)
```

### Examples

Pipe a long prompt from a file:

```bash
cat my-prompt.txt | pixforge -p -
```

Switch providers per call:

```bash
pixforge --profile openai      -p "watercolor mountains"
pixforge --profile azure-mai   -p "watercolor mountains"
pixforge --profile gemini      -p "watercolor mountains"
pixforge --profile local       -p "watercolor mountains"
```

Scriptable usage (only the path is on stdout):

```bash
img=$(pixforge -p "robot chef" --no-open --quiet)
mv "$img" hero.png
```

Inspect a profile (the api_key is never printed; you only see the env var
name and its status):

```bash
$ pixforge profile show azure-mai
name         = azure-mai
provider     = azure-mai
endpoint     = https://your-resource.services.ai.azure.com
model        = MAI-Image-2
api_version  = preview
auth_style   = ApiKey
api_key      = env $AZURE_API_KEY (set)
width        = 1024
height       = 1024
timeout_secs = 180
max_attempts = 5
```

## Output

For every successful generation, two files are written:

- `<output>.png` — the image
- `<output>.png.prompt.json` — sidecar with full provenance:
  ```json
  {
    "schema_version": 2,
    "generated_at":   "2026-05-03T17:30:00Z",
    "provider":       "openai-compat",
    "profile":        "openai",
    "model":          "gpt-image-1",
    "endpoint":       "https://api.openai.com/v1",
    "width":          1024,
    "height":         1024,
    "prompt":         "...",
    "prompt_hash":    "ab12cd34ef56",
    "revised_prompt": "...",
    "mime_type":      "image/png",
    "latency_s":      4.21,
    "attempts":       1
  }
  ```

Both are written atomically (`*.tmp` then `rename`).

## Behavior notes

- **Retries** on HTTP 429/5xx and transport errors (DNS/TLS/timeout) with
  exponential backoff (1, 2, 4, 8, 16s + jitter, capped at 60s). Honors
  `Retry-After`, `retry-after-ms`, and `x-ms-retry-after-ms`.
- **Per-provider validation** runs *before* the HTTP call where possible:
  - `azure-mai` requires `width >= 768 && height >= 768 && width*height <= 1,048,576`
  - `gemini` rejects `-W`/`-H` (the model picks the size)
  - `openai-compat` / `azure-openai` send the requested size verbatim;
    if it's not allowed for the model, the API returns a 400 with details.
- **Exit codes**: `0` success, `1` runtime error, `2` config / usage error.
- **stdout vs stderr**: progress and warnings go to stderr; the final image
  path is the only thing on stdout, so `$(pixforge -p …)` is safe in scripts.

## Adding a new provider

1. Create `src/providers/your_provider.rs` implementing `ImageProvider`
   (see `src/providers/azure_mai.rs` as a small reference).
2. Add a variant to `ProviderKind` in `src/config.rs` and wire its
   validator (required fields, default endpoint, default `auth_style`).
3. Construct the adapter in `build_provider` in `src/main.rs`.
4. Add a `tests/your_provider_mock.rs` covering URL/headers/body, success
   parsing, retry behavior, and error cases. Use the existing test files as
   templates.

PRs welcome.

## License

MIT — see [LICENSE](./LICENSE).
