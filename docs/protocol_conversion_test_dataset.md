# Protocol Conversion Test Dataset

This dataset is for agents testing the protocol-conversion change in
`llm-wrapper`. It is intentionally implementation-oriented: each case defines
the client entry endpoint, mock upstream capabilities, request payload, upstream
response, and expected assertions.

## Global Rules To Verify

- Client entry endpoints:
  - Chat Completions: `POST /v1/chat/completions`
  - Responses: `POST /v1/responses`
  - Anthropic Messages: `POST /v1/messages`
- Client response format must always match the entry endpoint.
- If the upstream supports the entry protocol, no conversion is required.
- If the upstream does not support the entry protocol:
  - `allow_protocol_conversion=false` must return HTTP `422`.
  - `allow_protocol_conversion=true` must convert through the first supported
    protocol by priority:
    - chat entry: chat -> responses -> anthropic
    - responses entry: responses -> chat -> anthropic
    - messages entry: anthropic -> chat -> responses
- Unsupported or lossy fields must return HTTP `422`; do not silently drop them.
- Codex upstreams must be treated as responses-only.

## Mock Upstream Matrix

Use these upstreams in config fixtures. Each upstream should be backed by a mock
HTTP server that records the received path/body and returns the case-specific
response.

```yaml
allow_protocol_conversion: true
upstreams:
  - name: chat-only
    base_url: http://mock-chat-only
    api_type: open_ai
    auth: { type: api_key, key: null }
    enabled: true
    support_chat_completions: true
    support_responses: false
    support_anthropic_messages: false

  - name: responses-only
    base_url: http://mock-responses-only
    api_type: open_ai
    auth: { type: api_key, key: null }
    enabled: true
    support_chat_completions: false
    support_responses: true
    support_anthropic_messages: false

  - name: anthropic-only
    base_url: http://mock-anthropic-only
    api_type: open_ai
    auth: { type: api_key, key: null }
    enabled: true
    support_chat_completions: false
    support_responses: false
    support_anthropic_messages: true

  - name: codex-responses-only
    base_url: http://mock-codex
    api_type: chatgpt_codex
    auth: { type: api_key, key: null }
    enabled: true
    support_chat_completions: false
    support_responses: true
    support_anthropic_messages: false

aliases:
  - alias: chat-model
    target_model: upstream-chat-model
    upstream: chat-only
    param_overrides: []
    source: manual
  - alias: responses-model
    target_model: upstream-responses-model
    upstream: responses-only
    param_overrides: []
    source: manual
  - alias: anthropic-model
    target_model: upstream-anthropic-model
    upstream: anthropic-only
    param_overrides: []
    source: manual
  - alias: codex-model
    target_model: upstream-codex-model
    upstream: codex-responses-only
    param_overrides: []
    source: manual
```

For compatibility tests, also use this legacy fixture and assert it loads into
the new capability fields:

```yaml
allow_protocol_conversion: false
upstreams:
  - name: legacy-openai
    base_url: http://mock-legacy
    api_type: open_ai
    api_key: null
    enabled: true
    support_openai: true
    support_anthropic: true
aliases: []
```

Expected legacy migration:

```json
{
  "support_chat_completions": true,
  "support_responses": true,
  "support_anthropic_messages": true
}
```

## Case PC-001: Chat Entry To Responses Upstream

Purpose: verify chat completions request converts to responses-only upstream
and returns chat completions format to the client.

Client request:

```http
POST /v1/chat/completions
```

```json
{
  "model": "responses-model",
  "messages": [
    { "role": "system", "content": "Be concise." },
    { "role": "user", "content": "Say hello." }
  ],
  "temperature": 0.2,
  "top_p": 0.9,
  "max_tokens": 64,
  "stop": ["END"]
}
```

Expected upstream request:

```http
POST /v1/responses
```

Assert body contains:

```json
{
  "model": "upstream-responses-model",
  "instructions": "Be concise.",
  "input": [
    { "role": "user", "content": "Say hello." }
  ],
  "temperature": 0.2,
  "top_p": 0.9,
  "max_output_tokens": 64,
  "stop": ["END"]
}
```

Mock upstream response:

```json
{
  "id": "resp_001",
  "object": "response",
  "created_at": 1710000000,
  "model": "upstream-responses-model",
  "output": [
    {
      "id": "msg_001",
      "type": "message",
      "role": "assistant",
      "content": [
        { "type": "output_text", "text": "Hello." }
      ]
    }
  ],
  "usage": {
    "input_tokens": 12,
    "output_tokens": 3,
    "total_tokens": 15
  }
}
```

Expected client response assertions:

```json
{
  "object": "chat.completion",
  "model": "upstream-responses-model",
  "choices": [
    {
      "index": 0,
      "message": { "role": "assistant", "content": "Hello." },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 12,
    "completion_tokens": 3,
    "total_tokens": 15
  }
}
```

## Case PC-002: Responses Entry To Chat Upstream

Purpose: verify responses request converts to chat-only upstream and returns
responses format to the client.

Client request:

```http
POST /v1/responses
```

```json
{
  "model": "chat-model",
  "instructions": "Answer in JSON.",
  "input": "What is 2+2?",
  "temperature": 0,
  "max_output_tokens": 32
}
```

Expected upstream request:

```http
POST /v1/chat/completions
```

Assert body contains:

```json
{
  "model": "upstream-chat-model",
  "messages": [
    { "role": "system", "content": "Answer in JSON." },
    { "role": "user", "content": "What is 2+2?" }
  ],
  "temperature": 0,
  "max_tokens": 32
}
```

Mock upstream response:

```json
{
  "id": "chatcmpl_002",
  "object": "chat.completion",
  "created": 1710000001,
  "model": "upstream-chat-model",
  "choices": [
    {
      "index": 0,
      "message": { "role": "assistant", "content": "{\"answer\":4}" },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 20,
    "completion_tokens": 5,
    "total_tokens": 25
  }
}
```

Expected client response assertions:

```json
{
  "object": "response",
  "model": "upstream-chat-model",
  "output": [
    {
      "type": "message",
      "role": "assistant",
      "content": [
        { "type": "output_text", "text": "{\"answer\":4}" }
      ]
    }
  ],
  "usage": {
    "input_tokens": 20,
    "output_tokens": 5,
    "total_tokens": 25
  }
}
```

## Case PC-003: Anthropic Entry To Chat Upstream

Purpose: verify `/v1/messages` converts to chat-only upstream and returns
Anthropic Messages format.

Client request:

```http
POST /v1/messages
```

```json
{
  "model": "chat-model",
  "system": "Use short sentences.",
  "max_tokens": 64,
  "messages": [
    {
      "role": "user",
      "content": [
        { "type": "text", "text": "Write a greeting." }
      ]
    }
  ]
}
```

Expected upstream request:

```http
POST /v1/chat/completions
```

Assert body contains:

```json
{
  "model": "upstream-chat-model",
  "messages": [
    { "role": "system", "content": "Use short sentences." },
    { "role": "user", "content": "Write a greeting." }
  ],
  "max_tokens": 64
}
```

Mock upstream response:

```json
{
  "id": "chatcmpl_003",
  "object": "chat.completion",
  "created": 1710000002,
  "model": "upstream-chat-model",
  "choices": [
    {
      "index": 0,
      "message": { "role": "assistant", "content": "Hello there." },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 18,
    "completion_tokens": 4,
    "total_tokens": 22
  }
}
```

Expected client response assertions:

```json
{
  "type": "message",
  "role": "assistant",
  "model": "upstream-chat-model",
  "content": [
    { "type": "text", "text": "Hello there." }
  ],
  "stop_reason": "end_turn",
  "usage": {
    "input_tokens": 18,
    "output_tokens": 4
  }
}
```

## Case PC-004: Chat Tools To Anthropic Upstream

Purpose: verify OpenAI tool definitions and assistant tool calls map to
Anthropic `tools`, `tool_use`, and `tool_result`.

Client request:

```http
POST /v1/chat/completions
```

```json
{
  "model": "anthropic-model",
  "messages": [
    { "role": "user", "content": "What is the weather in Shanghai?" }
  ],
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "get_weather",
        "description": "Get weather by city.",
        "parameters": {
          "type": "object",
          "properties": {
            "city": { "type": "string" }
          },
          "required": ["city"]
        }
      }
    }
  ],
  "tool_choice": "auto",
  "max_tokens": 128
}
```

Expected upstream request:

```http
POST /v1/messages
```

Assert body contains:

```json
{
  "model": "upstream-anthropic-model",
  "messages": [
    {
      "role": "user",
      "content": [
        { "type": "text", "text": "What is the weather in Shanghai?" }
      ]
    }
  ],
  "tools": [
    {
      "name": "get_weather",
      "description": "Get weather by city.",
      "input_schema": {
        "type": "object",
        "properties": {
          "city": { "type": "string" }
        },
        "required": ["city"]
      }
    }
  ],
  "tool_choice": { "type": "auto" },
  "max_tokens": 128
}
```

Mock upstream response:

```json
{
  "id": "msg_004",
  "type": "message",
  "role": "assistant",
  "model": "upstream-anthropic-model",
  "content": [
    {
      "type": "tool_use",
      "id": "toolu_004",
      "name": "get_weather",
      "input": { "city": "Shanghai" }
    }
  ],
  "stop_reason": "tool_use",
  "usage": {
    "input_tokens": 30,
    "output_tokens": 12
  }
}
```

Expected client response assertions:

```json
{
  "object": "chat.completion",
  "choices": [
    {
      "message": {
        "role": "assistant",
        "content": null,
        "tool_calls": [
          {
            "id": "toolu_004",
            "type": "function",
            "function": {
              "name": "get_weather",
              "arguments": "{\"city\":\"Shanghai\"}"
            }
          }
        ]
      },
      "finish_reason": "tool_calls"
    }
  ],
  "usage": {
    "prompt_tokens": 30,
    "completion_tokens": 12,
    "total_tokens": 42
  }
}
```

## Case PC-005: Chat Multimodal Image URL To Anthropic Upstream

Purpose: verify image URL is downloaded, validated, converted to base64, and
sent to Anthropic.

Client request:

```http
POST /v1/chat/completions
```

```json
{
  "model": "anthropic-model",
  "messages": [
    {
      "role": "user",
      "content": [
        { "type": "text", "text": "Describe this image." },
        {
          "type": "image_url",
          "image_url": {
            "url": "https://example.test/assets/red-dot.png"
          }
        }
      ]
    }
  ],
  "max_tokens": 64
}
```

Image fixture server response:

```http
HTTP/1.1 200 OK
Content-Type: image/png
Content-Length: 68
```

Body should be a valid tiny PNG. The exact bytes are not important; assert that
the outbound base64 decodes to the served bytes.

Expected upstream request:

```http
POST /v1/messages
```

Assert body contains an image block:

```json
{
  "type": "image",
  "source": {
    "type": "base64",
    "media_type": "image/png",
    "data": "<base64 of served image>"
  }
}
```

Mock upstream response:

```json
{
  "id": "msg_005",
  "type": "message",
  "role": "assistant",
  "model": "upstream-anthropic-model",
  "content": [
    { "type": "text", "text": "A small red dot." }
  ],
  "stop_reason": "end_turn",
  "usage": {
    "input_tokens": 40,
    "output_tokens": 6
  }
}
```

Expected client response assertion:

```json
{
  "choices": [
    {
      "message": {
        "role": "assistant",
        "content": "A small red dot."
      }
    }
  ]
}
```

## Case PC-006: Unsupported Private Image URL Returns 422

Purpose: verify SSRF protection for image downloads.

Client request:

```http
POST /v1/chat/completions
```

```json
{
  "model": "anthropic-model",
  "messages": [
    {
      "role": "user",
      "content": [
        {
          "type": "image_url",
          "image_url": { "url": "http://127.0.0.1/private.png" }
        }
      ]
    }
  ]
}
```

Expected response:

```json
{
  "status": 422,
  "error_contains": ["image", "private", "127.0.0.1"]
}
```

No upstream request should be sent.

## Case PC-007: Conversion Disabled Returns 422

Purpose: verify the global switch prevents implicit conversion.

Config override:

```yaml
allow_protocol_conversion: false
```

Client request:

```http
POST /v1/chat/completions
```

```json
{
  "model": "responses-model",
  "messages": [
    { "role": "user", "content": "Hello" }
  ]
}
```

Expected response:

```json
{
  "status": 422,
  "error_contains": [
    "protocol conversion",
    "disabled",
    "chat_completions",
    "responses"
  ]
}
```

No upstream request should be sent.

## Case PC-008: Unsupported Field Returns 422

Purpose: verify unmappable fields do not get silently dropped.

Client request:

```http
POST /v1/chat/completions
```

```json
{
  "model": "anthropic-model",
  "messages": [
    { "role": "user", "content": "Return one token." }
  ],
  "logprobs": true,
  "top_logprobs": 3
}
```

Expected response:

```json
{
  "status": 422,
  "error_contains": ["logprobs", "top_logprobs", "anthropic"]
}
```

No upstream request should be sent.

## Case PC-009: Codex Responses-Only Upstream From Chat Entry

Purpose: verify Codex is responses-only and chat entry converts to Codex
responses path.

Client request:

```http
POST /v1/chat/completions
```

```json
{
  "model": "codex-model",
  "messages": [
    { "role": "system", "content": "Do not store this." },
    { "role": "user", "content": "Ping" }
  ],
  "max_tokens": 16
}
```

Expected upstream request:

```http
POST /codex/responses
```

Assert body contains:

```json
{
  "model": "upstream-codex-model",
  "instructions": "Do not store this.",
  "input": [
    { "role": "user", "content": "Ping" }
  ],
  "max_output_tokens": 16,
  "store": false
}
```

Mock upstream response:

```json
{
  "id": "resp_codex_009",
  "object": "response",
  "created_at": 1710000009,
  "model": "upstream-codex-model",
  "output": [
    {
      "id": "msg_codex_009",
      "type": "message",
      "role": "assistant",
      "content": [
        { "type": "output_text", "text": "pong" }
      ]
    }
  ],
  "usage": {
    "input_tokens": 10,
    "output_tokens": 1,
    "total_tokens": 11
  }
}
```

Expected client response assertion:

```json
{
  "object": "chat.completion",
  "choices": [
    {
      "message": { "role": "assistant", "content": "pong" }
    }
  ]
}
```

## Case PC-010: Streaming Chat Entry To Anthropic Upstream

Purpose: verify converted streams remain valid SSE in the client entry format.

Client request:

```http
POST /v1/chat/completions
```

```json
{
  "model": "anthropic-model",
  "messages": [
    { "role": "user", "content": "Count to two." }
  ],
  "stream": true,
  "max_tokens": 16
}
```

Expected upstream request:

```http
POST /v1/messages
```

Assert body contains:

```json
{
  "model": "upstream-anthropic-model",
  "stream": true,
  "messages": [
    {
      "role": "user",
      "content": [
        { "type": "text", "text": "Count to two." }
      ]
    }
  ]
}
```

Mock upstream SSE:

```text
event: message_start
data: {"type":"message_start","message":{"id":"msg_stream_010","type":"message","role":"assistant","model":"upstream-anthropic-model","content":[],"stop_reason":null,"usage":{"input_tokens":8,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"One"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":", two."}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}

event: message_stop
data: {"type":"message_stop"}
```

Expected client SSE assertions:

```text
data: {"object":"chat.completion.chunk", ... "delta":{"role":"assistant"} ...}
data: {"object":"chat.completion.chunk", ... "delta":{"content":"One"} ...}
data: {"object":"chat.completion.chunk", ... "delta":{"content":", two."} ...}
data: {"object":"chat.completion.chunk", ... "finish_reason":"stop" ...}
data: [DONE]
```

## Case PC-011: Anthropic Tool Result To Chat Upstream

Purpose: verify Anthropic `tool_result` history maps to OpenAI `tool` role
messages.

Client request:

```http
POST /v1/messages
```

```json
{
  "model": "chat-model",
  "max_tokens": 64,
  "messages": [
    {
      "role": "user",
      "content": [{ "type": "text", "text": "Use the tool." }]
    },
    {
      "role": "assistant",
      "content": [
        {
          "type": "tool_use",
          "id": "toolu_011",
          "name": "lookup",
          "input": { "id": "42" }
        }
      ]
    },
    {
      "role": "user",
      "content": [
        {
          "type": "tool_result",
          "tool_use_id": "toolu_011",
          "content": [{ "type": "text", "text": "Found value 42." }]
        }
      ]
    }
  ],
  "tools": [
    {
      "name": "lookup",
      "description": "Lookup by id.",
      "input_schema": {
        "type": "object",
        "properties": { "id": { "type": "string" } },
        "required": ["id"]
      }
    }
  ]
}
```

Expected upstream request:

```http
POST /v1/chat/completions
```

Assert body contains messages equivalent to:

```json
[
  { "role": "user", "content": "Use the tool." },
  {
    "role": "assistant",
    "content": null,
    "tool_calls": [
      {
        "id": "toolu_011",
        "type": "function",
        "function": {
          "name": "lookup",
          "arguments": "{\"id\":\"42\"}"
        }
      }
    ]
  },
  {
    "role": "tool",
    "tool_call_id": "toolu_011",
    "content": "Found value 42."
  }
]
```

Expected upstream tools contain:

```json
[
  {
    "type": "function",
    "function": {
      "name": "lookup",
      "description": "Lookup by id.",
      "parameters": {
        "type": "object",
        "properties": { "id": { "type": "string" } },
        "required": ["id"]
      }
    }
  }
]
```

## Case PC-012: Direct Protocol Still Passes Through

Purpose: verify supported entry protocol does not get unnecessarily converted.

Client request:

```http
POST /v1/chat/completions
```

```json
{
  "model": "chat-model",
  "messages": [
    { "role": "user", "content": "No conversion." }
  ]
}
```

Expected upstream request:

```http
POST /v1/chat/completions
```

Assert:

- upstream path is unchanged
- `model` is replaced with `upstream-chat-model`
- request body is otherwise not reshaped into responses or anthropic format
- client response is the upstream chat response, except any existing wrapper
  debug behavior remains unchanged

## Agent Execution Checklist

For each case:

1. Start a mock upstream server that records path, headers, and JSON body.
2. Start `llm-wrapper` with a temp config based on the matrix above.
3. Send the client request to the wrapper entry endpoint.
4. Assert HTTP status.
5. Assert the mock upstream received exactly zero or one request as specified.
6. Assert upstream path and body fragments.
7. Return the mock upstream response.
8. Assert client response format and key fields.
9. For streaming cases, parse SSE lines and assert ordered chunks, not raw byte
   equality.
10. Repeat representative cases with `X-Debug-Mode: true` and assert debug data
    shows client request, converted upstream request, and upstream response.

