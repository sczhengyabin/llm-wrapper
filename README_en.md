<p align="center">
  <img src="logo.svg" alt="LLM Wrapper Logo" width="180">
</p>

# LLM Wrapper

A lightweight LLM API aggregation gateway (Rust + actix-web), similar to litellm but more minimal. Multi-upstream aggregation, protocol conversion, parameter injection, OAuth upstreams, with a visual WebUI for management.

[中文文档](README_zh.md)

## Features

- **Multi-upstream Aggregation**: Unify multiple upstream LLM APIs behind one endpoint
- **Multi-protocol Support**: Chat Completions, Responses, Anthropic Messages, with automatic conversion between protocols
- **Model Aliases & Routing**: Define local aliases for upstream models with parameter injection
- **Parameter Injection**: `override` (force) and `default` (only when unset by client) modes
- **OAuth Upstreams**: Built-in CLIProxyAPI sidecar manages Claude / Codex login and automatic token refresh
- **Quota Query**: Inspect usage and quota of CLIProxyAPI accounts
- **Hot Config Reload**: Config file edits or WebUI saves take effect immediately, no restart
- **Visual WebUI**: Single-file management interface — config editing, model aggregation, debug panel
- **Admin Authentication**: Argon2 password hashing + HttpOnly session cookie protect the admin panel
- **Client API Keys**: Optional auth for `/v1/*` endpoints (Bearer / x-api-key)
- **Key Masking**: API keys are auto-masked in management endpoints
- **Debug Mode**: `X-Debug-Mode` header returns the full request/response chain, with SSE live streaming

## Quick Start

CLIProxyAPI is included as a git submodule — clone with submodules:

```bash
git clone --recursive <repo-url>      # or run `git submodule update --init` after cloning
cargo build --release
./target/release/llm-wrapper           # listens on 0.0.0.0:3000 by default
```

CLI arguments (take precedence over environment variables):

```bash
llm-wrapper -c config.yaml -a 0.0.0.0:3000
#   -c, --config <PATH>   Config file path (default: config.yaml)
#   -a, --addr <ADDR>     Bind address (default: 0.0.0.0:3000)
```

On first visit to the WebUI (`http://localhost:3000`) you'll be prompted to set an admin password.

### Docker

```bash
docker run -d --name llm-wrapper \
  -p 3000:3000 -p 8317:8317 \
  -v $(pwd)/config:/app/config \
  -v llm-wrapper-data:/app/.llm-wrapper \
  -e CONFIG_PATH=/app/config/config.yaml \
  sczhengyabin/llm-wrapper:latest
```

- Ports: `3000` main API & WebUI, `8317` CLIProxyAPI (Claude/Codex OAuth)
- Volumes: `/app/config` config directory, `/app/.llm-wrapper` token cache & account data
- Or use `docker-compose up -d`

## Configuration

Copy `config.yaml.example` to `config.yaml`. Core structure:

```yaml
upstreams:
  - name: vllm                       # unique upstream identifier
    base_url: http://127.0.0.1:30002
    auth:
      type: api_key                  # api_key / anthropic_oauth / codex_oauth
      key: null                      # set the key for api_key; omit for OAuth
    enabled: true
    support_chat_completions: true
    support_responses: false
    support_anthropic_messages: false

aliases:
  - alias: qwen                      # the request `model` matches only this field
    target_model: Qwen/Qwen3-...     # real model name used when forwarding (not routed)
    upstream: vllm
    param_overrides:
      - key: temperature
        value: 0.7
        mode: default                # default or override
    source: manual                   # manual, or auto (created by clicking model tags)

# Optional: auto-convert when an upstream doesn't support the entry protocol
# allow_protocol_conversion: true

# Optional: protect /v1/* — clients must send Authorization: Bearer <key>
# client_api_keys:
#   - name: "local"
#     key: "your-client-api-key"
```

**Auth types**: `api_key` (static key), `anthropic_oauth`, `codex_oauth` (the latter two are managed by CLIProxyAPI — log in via the WebUI, tokens auto-refresh).

**Routing**: the request `model` matches only the `alias` field; `target_model` is used solely to replace the model name when forwarding and is not routed. To call by an upstream's raw model name, create an auto alias by clicking the model tag in the WebUI.

## Usage

```bash
# Chat Completions (OpenAI-compatible)
curl http://localhost:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model": "qwen", "messages": [{"role": "user", "content": "Hello"}]}'

# Model list / Responses / Anthropic Messages
curl http://localhost:3000/v1/models
# POST /v1/responses          requires upstream Responses support
# POST /v1/messages           requires upstream Anthropic support
```

Entry points: `/v1/*` client API, `/api/*` management API (admin login required), `/` WebUI.

Debug: add the `X-Debug-Mode: true` header to get the full chain in the response (client request, upstream URL, injected upstream request, upstream response, etc.), or watch the live SSE stream in the WebUI debug panel.
