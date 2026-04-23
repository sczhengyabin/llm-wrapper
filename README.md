# LLM Wrapper

A lightweight OpenAI protocol aggregation wrapper, similar to litellm but more minimal.

[中文文档](README_zh.md)

## Features

- **Multi-upstream Aggregation**: Configure multiple upstream OpenAI-compatible APIs
- **Model Aliases**: Define local aliases for upstream models
- **Parameter Settings**: Supports `override` (force) and `default` (fallback) modes
- **Hot Config Reload**: WebUI config changes take effect immediately without restart
- **YAML Configuration**: Persistent config file support
- **Single-file WebUI**: Management interface built with pure HTML + JS
- **API Key Masking**: Auto-masks API keys in management endpoints
- **Auto Alias**: One-click passthrough alias creation by clicking upstream model tags

## Quick Start

### Build

```bash
cargo build --release
```

### Run

```bash
./target/release/llm-wrapper
```

### Docker Deployment

**Run with Docker (Recommended):**

```bash
docker run -d \
  --name llm-wrapper \
  -p 3000:3000 \
  -v $(pwd)/config:/app/config \
  -e BIND_ADDR=0.0.0.0:3000 \
  -e CONFIG_PATH=/app/config/config.yaml \
  sczhengyabin/llm-wrapper:latest
```

**With docker-compose:**

```bash
# Start
docker-compose up -d

# View logs
docker-compose logs -f

# Stop
docker-compose down
```

**Build image locally (optional):**

```bash
docker build -t llm-wrapper:latest .
```

### Environment Variables

- `CONFIG_PATH` - Config file path (default: config.yaml)
- `BIND_ADDR` - Bind address (default: 0.0.0.0:3000)

## Configuration Example

```yaml
# Upstream config (name as unique identifier)
upstreams:
  - name: qwen-test
    base_url: http://192.168.100.7:30002
    api_key: null  # or "your-api-key"
    enabled: true
    support_openai: true      # Supports OpenAI protocol (chat/completions, responses)
    support_anthropic: false   # Does not support Anthropic protocol (messages)
    # models_url: http://192.168.100.7:30002/v1/models  # Optional, defaults to {base_url}/v1/models

# Model alias config
aliases:
  - alias: qwen
    target_model: Qwen/Qwen3.5-122B-A10B-GPTQ-Int4
    upstream: qwen-test
    param_overrides:
      - key: temperature
        value: 0.7
        mode: default  # or override
      # extra_body configured separately
      - key: extra_body
        value:
          chat_template_kwargs:
            enable_thinking: false
        mode: default
    source: manual  # manually created alias
```

## Routing Rules

- **Alias Matching**: The `model` parameter in requests only matches the `alias` field
- **Target Model is Not Routed**: `target_model` is only used to replace the model name when forwarding, not for routing
- **Direct Upstream Call**: If no alias match is found and `model` matches an enabled upstream `name`, use that upstream directly

This means:
- With `alias: my-model -> target_model: gpt-4`, you must call with `model: "my-model"`
- To support `model: "gpt-4"`, create an `alias: gpt-4 -> target_model: gpt-4` auto alias

## API Endpoints

### Config Management

- `GET /api/config` - Get current config
- `PUT /api/config` - Update config (saves to YAML file)

### Upstream Model Management

- `GET /api/upstream-models` - Get model list from all upstreams
- `POST /api/upstream-models/alias` - Create auto alias for upstream model

### OpenAI Compatible API

- `POST /v1/chat/completions` - Chat completions
- `POST /v1/responses` - Responses API (upstream support required)
- `POST /v1/messages` - Anthropic Messages API (upstream support required)
- `GET /v1/models` - Model list (returns all aliases)

### Debug Endpoints

- `GET /api/debug` - Get latest debug info
- `DELETE /api/debug` - Clear debug info
- `GET /api/debug/stream` - SSE streaming debug info

### WebUI

- `GET /` - WebUI management interface

## WebUI Features

### Aggregated Model List

Displays all model aliases accessible via `/v1/models` at the top of the page, grouped by upstream.

### Upstream Model Tags

- **Blue dashed border**: Available model, click to create auto alias
- **Green solid border**: Enabled auto alias, click to delete
- **Red background**: Alias name conflict, cannot create

### Auto Alias

Auto alias is a passthrough alias: `alias = target_model = upstream model name`, with no parameter overrides.

**Create:**
- WebUI: Click model tags in upstream config cards
- API: `POST /api/upstream-models/alias`

**Delete:**
- WebUI: Click enabled green model tags
- Manually delete alias

## Usage Examples

### Chat Completions

```bash
curl -X POST http://localhost:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen",
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

### List Models

```bash
curl http://localhost:3000/v1/models
```

### Create Auto Alias

```bash
curl -X POST http://localhost:3000/api/upstream-models/alias \
  -H "Content-Type: application/json" \
  -d '{
    "upstream": "qwen-test",
    "model": "Qwen/Qwen3.5-122B-A10B-GPTQ-Int4"
  }'
```

### Responses API

```bash
curl -X POST http://localhost:3000/v1/responses \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen",
    "input": "Hello"
  }'
```

> Note: Responses API requires upstream support. If the upstream only supports Chat Completions, the response format may not comply with the Responses API spec.

### Anthropic Messages API

```bash
curl -X POST http://localhost:3000/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: your-anthropic-api-key" \
  -d '{
    "model": "claude-sonnet-4",
    "max_tokens": 1024,
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

> Note: Messages API requires upstream support for the Anthropic protocol (e.g., Anthropic API). If unsupported, it will return 404/405 errors.

## Debug Mode

Enable debug mode with the `X-Debug-Mode: true` header to get full request/response debug info:

```bash
curl -X POST http://localhost:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Debug-Mode: true" \
  -d '{
    "model": "qwen",
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

Response includes:
- `client_request`: Original request sent to the Wrapper
- `client_ip`: Client source IP
- `client_url`: Client request URL
- `endpoint`: Called endpoint
- `upstream_url`: Upstream request URL
- `upstream_request`: Request sent to upstream (with param overrides applied)
- `upstream_response`: Response from upstream
