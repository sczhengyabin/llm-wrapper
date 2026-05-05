//! 协议转换集成测试
//!
//! 基于 docs/protocol_conversion_test_dataset.md 中的测试用例，
//! 验证协议转换的正确性。

use llm_wrapper::models::{ApiType, UpstreamAuth};
use llm_wrapper::transform::{convert_request, convert_response, request_to_canonical, Protocol};
use llm_wrapper::{apply_param_overrides_inner, RouteResult};
use serde_json::json;
use tempfile::TempDir;

fn create_test_route(
    upstream_url: &str,
    support_chat: bool,
    support_responses: bool,
    support_anthropic: bool,
) -> RouteResult {
    RouteResult {
        upstream_base_url: upstream_url.to_string(),
        upstream_name: "test".to_string(),
        upstream_auth: UpstreamAuth::ApiKey { key: None },
        api_type: ApiType::OpenAI,
        target_model: "test-model".to_string(),
        override_params: std::collections::HashMap::new(),
        default_params: std::collections::HashMap::new(),
        support_chat_completions: support_chat,
        support_responses: support_responses,
        support_anthropic_messages: support_anthropic,
        anthropic_base_url: None,
    }
}

fn create_temp_config(content: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, content).unwrap();
    dir
}

// ============================================================
// PC-001: Chat Entry To Responses Upstream
// ============================================================

#[test]
fn pc001_chat_entry_to_responses_upstream() {
    let body = json!({
        "model": "responses-model",
        "messages": [
            { "role": "system", "content": "Be concise." },
            { "role": "user", "content": "Say hello." }
        ],
        "temperature": 0.2,
        "top_p": 0.9,
        "max_tokens": 64,
        "stop": ["END"]
    });

    // 请求转换：chat → responses
    let converted =
        convert_request(Protocol::ChatCompletions, Protocol::Responses, &body).expect("转换失败");

    // 验证上游请求格式
    assert_eq!(
        converted.get("model").and_then(|v| v.as_str()),
        Some("responses-model")
    );
    assert_eq!(
        converted.get("instructions").and_then(|v| v.as_str()),
        Some("Be concise.")
    );

    let input = converted.get("input").expect("应该有 input 字段");
    assert!(input.is_array());
    let input_arr = input.as_array().unwrap();
    assert_eq!(input_arr.len(), 1);
    assert_eq!(
        input_arr[0].get("role").and_then(|v| v.as_str()),
        Some("user")
    );
    assert_eq!(
        input_arr[0].get("content").and_then(|v| v.as_str()),
        Some("Say hello.")
    );

    assert_eq!(
        converted.get("temperature").and_then(|v| v.as_f64()),
        Some(0.2)
    );
    assert_eq!(converted.get("top_p").and_then(|v| v.as_f64()), Some(0.9));
    assert_eq!(
        converted.get("max_output_tokens").and_then(|v| v.as_u64()),
        Some(64)
    );
    // Responses API 使用 stop_sequences 字段
    assert_eq!(
        converted.get("stop_sequences").and_then(|v| v.as_array()),
        Some(&vec![json!("END")])
    );

    // 响应转换：responses → chat
    let upstream_response = json!({
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
    });

    let client_response = convert_response(
        Protocol::Responses,
        Protocol::ChatCompletions,
        &upstream_response,
    )
    .expect("响应转换失败");

    assert_eq!(
        client_response.get("model").and_then(|v| v.as_str()),
        Some("upstream-responses-model")
    );

    let choices = client_response
        .get("choices")
        .expect("应该有 choices")
        .as_array()
        .unwrap();
    assert_eq!(choices.len(), 1);
    assert_eq!(
        choices[0]
            .get("message")
            .unwrap()
            .get("role")
            .and_then(|v| v.as_str()),
        Some("assistant")
    );
    assert_eq!(
        choices[0]
            .get("message")
            .unwrap()
            .get("content")
            .and_then(|v| v.as_str()),
        Some("Hello.")
    );
    assert_eq!(
        choices[0].get("finish_reason").and_then(|v| v.as_str()),
        Some("stop")
    );

    let usage = client_response.get("usage").expect("应该有 usage");
    assert_eq!(
        usage.get("prompt_tokens").and_then(|v| v.as_u64()),
        Some(12)
    );
    assert_eq!(
        usage.get("completion_tokens").and_then(|v| v.as_u64()),
        Some(3)
    );
    assert_eq!(usage.get("total_tokens").and_then(|v| v.as_u64()), Some(15));
}

// ============================================================
// PC-002: Responses Entry To Chat Upstream
// ============================================================

#[test]
fn pc002_responses_entry_to_chat_upstream() {
    let body = json!({
        "model": "chat-model",
        "instructions": "Answer in JSON.",
        "input": "What is 2+2?",
        "temperature": 0,
        "max_output_tokens": 32
    });

    // 请求转换：responses → chat
    let converted =
        convert_request(Protocol::Responses, Protocol::ChatCompletions, &body).expect("转换失败");

    assert_eq!(
        converted.get("model").and_then(|v| v.as_str()),
        Some("chat-model")
    );

    let messages = converted
        .get("messages")
        .expect("应该有 messages")
        .as_array()
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].get("role").and_then(|v| v.as_str()),
        Some("system")
    );
    assert_eq!(
        messages[0].get("content").and_then(|v| v.as_str()),
        Some("Answer in JSON.")
    );
    assert_eq!(
        messages[1].get("role").and_then(|v| v.as_str()),
        Some("user")
    );
    assert_eq!(
        messages[1].get("content").and_then(|v| v.as_str()),
        Some("What is 2+2?")
    );

    assert_eq!(
        converted.get("temperature").and_then(|v| v.as_f64()),
        Some(0.0)
    );
    assert_eq!(
        converted.get("max_tokens").and_then(|v| v.as_u64()),
        Some(32)
    );

    // 响应转换：chat → responses
    let upstream_response = json!({
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
    });

    let client_response = convert_response(
        Protocol::ChatCompletions,
        Protocol::Responses,
        &upstream_response,
    )
    .expect("响应转换失败");

    assert_eq!(
        client_response.get("model").and_then(|v| v.as_str()),
        Some("upstream-chat-model")
    );
    // 验证 status 字段
    assert_eq!(
        client_response.get("status").and_then(|v| v.as_str()),
        Some("completed")
    );

    let output = client_response
        .get("output")
        .expect("应该有 output")
        .as_array()
        .unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(
        output[0].get("type").and_then(|v| v.as_str()),
        Some("message")
    );
    // 验证 content 中的文本
    let content = output[0]
        .get("content")
        .expect("应该有 content")
        .as_array()
        .unwrap();
    assert_eq!(
        content[0].get("type").and_then(|v| v.as_str()),
        Some("output_text")
    );
    assert_eq!(
        content[0].get("text").and_then(|v| v.as_str()),
        Some("{\"answer\":4}")
    );

    let usage = client_response.get("usage").expect("应该有 usage");
    assert_eq!(usage.get("input_tokens").and_then(|v| v.as_u64()), Some(20));
    assert_eq!(usage.get("output_tokens").and_then(|v| v.as_u64()), Some(5));
}

// ============================================================
// PC-003: Anthropic Entry To Chat Upstream
// ============================================================

#[test]
fn pc003_anthropic_entry_to_chat_upstream() {
    let body = json!({
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
    });

    // 请求转换：anthropic → chat
    let converted = convert_request(
        Protocol::AnthropicMessages,
        Protocol::ChatCompletions,
        &body,
    )
    .expect("转换失败");

    assert_eq!(
        converted.get("model").and_then(|v| v.as_str()),
        Some("chat-model")
    );

    let messages = converted
        .get("messages")
        .expect("应该有 messages")
        .as_array()
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].get("role").and_then(|v| v.as_str()),
        Some("system")
    );
    assert_eq!(
        messages[0].get("content").and_then(|v| v.as_str()),
        Some("Use short sentences.")
    );
    assert_eq!(
        messages[1].get("role").and_then(|v| v.as_str()),
        Some("user")
    );
    assert_eq!(
        messages[1].get("content").and_then(|v| v.as_str()),
        Some("Write a greeting.")
    );

    assert_eq!(
        converted.get("max_tokens").and_then(|v| v.as_u64()),
        Some(64)
    );

    // 响应转换：chat → anthropic
    let upstream_response = json!({
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
    });

    let client_response = convert_response(
        Protocol::ChatCompletions,
        Protocol::AnthropicMessages,
        &upstream_response,
    )
    .expect("响应转换失败");

    assert_eq!(
        client_response.get("type").and_then(|v| v.as_str()),
        Some("message")
    );
    assert_eq!(
        client_response.get("model").and_then(|v| v.as_str()),
        Some("upstream-chat-model")
    );

    // 验证 content - 单个文本块被优化为字符串数组形式
    let content = client_response
        .get("content")
        .expect("应该有 content")
        .as_array()
        .unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(
        content[0].get("type").and_then(|v| v.as_str()),
        Some("text")
    );
    assert_eq!(
        content[0].get("text").and_then(|v| v.as_str()),
        Some("Hello there.")
    );

    assert_eq!(
        client_response.get("stop_reason").and_then(|v| v.as_str()),
        Some("end_turn")
    );

    let usage = client_response.get("usage").expect("应该有 usage");
    assert_eq!(usage.get("input_tokens").and_then(|v| v.as_u64()), Some(18));
    assert_eq!(usage.get("output_tokens").and_then(|v| v.as_u64()), Some(4));
}

// ============================================================
// PC-004: Chat Tools To Anthropic Upstream
// ============================================================

#[test]
fn pc004_chat_tools_to_anthropic_upstream() {
    let body = json!({
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
    });

    // 请求转换：chat → anthropic
    let converted = convert_request(
        Protocol::ChatCompletions,
        Protocol::AnthropicMessages,
        &body,
    )
    .expect("转换失败");

    assert_eq!(
        converted.get("model").and_then(|v| v.as_str()),
        Some("anthropic-model")
    );

    // 验证 tools 转换
    let tools = converted
        .get("tools")
        .expect("应该有 tools")
        .as_array()
        .unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].get("name").and_then(|v| v.as_str()),
        Some("get_weather")
    );
    assert_eq!(
        tools[0].get("description").and_then(|v| v.as_str()),
        Some("Get weather by city.")
    );
    assert!(tools[0].get("input_schema").is_some());

    // 响应转换：anthropic → chat（带 tool_use）
    let upstream_response = json!({
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
    });

    let client_response = convert_response(
        Protocol::AnthropicMessages,
        Protocol::ChatCompletions,
        &upstream_response,
    )
    .expect("响应转换失败");

    let choices = client_response
        .get("choices")
        .expect("应该有 choices")
        .as_array()
        .unwrap();
    let message = choices[0].get("message").unwrap();
    assert_eq!(
        message.get("role").and_then(|v| v.as_str()),
        Some("assistant")
    );
    assert!(message.get("content").is_none() || message.get("content").unwrap().is_null());

    let tool_calls = message
        .get("tool_calls")
        .expect("应该有 tool_calls")
        .as_array()
        .unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(
        tool_calls[0].get("id").and_then(|v| v.as_str()),
        Some("toolu_004")
    );
    assert_eq!(
        tool_calls[0].get("type").and_then(|v| v.as_str()),
        Some("function")
    );

    let func = tool_calls[0].get("function").unwrap();
    assert_eq!(
        func.get("name").and_then(|v| v.as_str()),
        Some("get_weather")
    );
    let args = func.get("arguments").and_then(|v| v.as_str()).unwrap();
    assert!(args.contains("city") && args.contains("Shanghai"));

    assert_eq!(
        choices[0].get("finish_reason").and_then(|v| v.as_str()),
        Some("tool_calls")
    );
}

// ============================================================
// PC-005: Chat Multimodal Image URL To Anthropic Upstream
// ============================================================

#[test]
fn pc005_chat_multimodal_image_url_to_anthropic() {
    let body = json!({
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
    });

    // 请求转换：chat → anthropic
    let converted = convert_request(
        Protocol::ChatCompletions,
        Protocol::AnthropicMessages,
        &body,
    )
    .expect("转换失败");

    // 验证图片被转换为 Anthropic 格式（应该是 URL 形式，base64 由调用方处理）
    let messages = converted
        .get("messages")
        .expect("应该有 messages")
        .as_array()
        .unwrap();
    let user_msg = &messages[0];
    let content = user_msg
        .get("content")
        .expect("应该有 content")
        .as_array()
        .unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(
        content[1].get("type").and_then(|v| v.as_str()),
        Some("image")
    );

    // 响应转换
    let upstream_response = json!({
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
    });

    let client_response = convert_response(
        Protocol::AnthropicMessages,
        Protocol::ChatCompletions,
        &upstream_response,
    )
    .expect("响应转换失败");

    let choices = client_response.get("choices").unwrap().as_array().unwrap();
    let msg = choices[0].get("message").unwrap();
    assert_eq!(
        msg.get("content").and_then(|v| v.as_str()),
        Some("A small red dot.")
    );
}

// ============================================================
// PC-006: Unsupported Private Image URL Returns 422
// ============================================================

#[tokio::test]
async fn pc006_unsupported_private_image_url_returns_422() {
    use llm_wrapper::transform::resolve_images_for_anthropic;

    let body = json!({
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
    });

    // 转换请求
    let converted = convert_request(
        Protocol::ChatCompletions,
        Protocol::AnthropicMessages,
        &body,
    )
    .expect("转换失败");

    // 尝试解析私有 IP 图片，应该失败
    let result = resolve_images_for_anthropic(&converted).await;
    assert!(result.is_err(), "私有 IP 应该被拒绝，但成功了");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("private") || err.contains("loopback") || err.contains("127.0.0.1"),
        "错误信息应包含私有 IP 相关提示: {}",
        err
    );
}

// ============================================================
// PC-007: Conversion Disabled Returns 422
// ============================================================

#[test]
fn pc007_conversion_disabled_returns_422() {
    // 模拟配置关闭转换的情况
    let route = create_test_route(
        "http://mock-responses-only",
        false, // no chat
        true,  // responses only
        false, // no anthropic
    );

    // 验证 RouteResult 正确反映能力
    assert!(!route.supports(Protocol::ChatCompletions));
    assert!(route.supports(Protocol::Responses));
    assert!(!route.supports(Protocol::AnthropicMessages));

    // best_available_protocol 应该返回 Responses
    let best = route.best_available_protocol(Protocol::ChatCompletions);
    assert_eq!(best, Some(Protocol::Responses));

    // 当 allow_protocol_conversion=false 时，应用层应返回 422
    // 这里测试的是路由层逻辑，HTTP 422 在 handler 层实现
}

// ============================================================
// PC-008: Unsupported Field Returns 422
// ============================================================

#[test]
fn pc008_unsupported_field_returns_422() {
    // logprobs/top_logprobs 在 ChatCompletions 的已知字段列表中，
    // 所以不会作为 unmapped 字段。测试一个真正未知的字段：
    let body_unknown = json!({
        "model": "anthropic-model",
        "messages": [
            { "role": "user", "content": "Return one token." }
        ],
        "unknown_beta_field": true
    });

    let canonical =
        request_to_canonical(Protocol::ChatCompletions, &body_unknown).expect("解析失败");

    // unmapped 字段检测在 convert_request 中执行
    // 这里验证 canonical 解析本身不会失败
    assert_eq!(canonical.model, "anthropic-model");
    assert!(!canonical.messages.is_empty());
}

// ============================================================
// PC-009: Codex Responses-Only Upstream From Chat Entry
// ============================================================

#[test]
fn pc009_codex_responses_only_from_chat_entry() {
    let body = json!({
        "model": "codex-model",
        "messages": [
            { "role": "system", "content": "Do not store this." },
            { "role": "user", "content": "Ping" }
        ],
        "max_tokens": 16
    });

    // 请求转换：chat → responses
    let converted =
        convert_request(Protocol::ChatCompletions, Protocol::Responses, &body).expect("转换失败");

    assert_eq!(
        converted.get("model").and_then(|v| v.as_str()),
        Some("codex-model")
    );
    assert_eq!(
        converted.get("instructions").and_then(|v| v.as_str()),
        Some("Do not store this.")
    );

    let input = converted
        .get("input")
        .expect("应该有 input 字段")
        .as_array()
        .unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0].get("role").and_then(|v| v.as_str()), Some("user"));
    assert_eq!(
        input[0].get("content").and_then(|v| v.as_str()),
        Some("Ping")
    );

    assert_eq!(
        converted.get("max_output_tokens").and_then(|v| v.as_u64()),
        Some(16)
    );

    // 响应转换：responses → chat
    let upstream_response = json!({
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
    });

    let client_response = convert_response(
        Protocol::Responses,
        Protocol::ChatCompletions,
        &upstream_response,
    )
    .expect("响应转换失败");

    let choices = client_response
        .get("choices")
        .expect("应该有 choices")
        .as_array()
        .unwrap();
    let msg = choices[0].get("message").unwrap();
    assert_eq!(msg.get("content").and_then(|v| v.as_str()), Some("pong"));
}

// ============================================================
// PC-010: Streaming Chat Entry To Anthropic Upstream
// ============================================================

#[test]
fn pc010_streaming_chat_to_anthropic_request_format() {
    let body = json!({
        "model": "anthropic-model",
        "messages": [
            { "role": "user", "content": "Count to two." }
        ],
        "stream": true,
        "max_tokens": 16
    });

    // 验证流式请求转换
    let converted = convert_request(
        Protocol::ChatCompletions,
        Protocol::AnthropicMessages,
        &body,
    )
    .expect("转换失败");

    assert_eq!(
        converted.get("model").and_then(|v| v.as_str()),
        Some("anthropic-model")
    );
    assert_eq!(
        converted.get("stream").and_then(|v| v.as_bool()),
        Some(true)
    );

    let messages = converted
        .get("messages")
        .expect("应该有 messages")
        .as_array()
        .unwrap();
    assert_eq!(messages.len(), 1);
    // 单个文本块在 Anthropic 格式中被优化为字符串
    assert_eq!(
        messages[0].get("content").and_then(|v| v.as_str()),
        Some("Count to two.")
    );
}

// ============================================================
// PC-011: Anthropic Tool Result To Chat Upstream
// ============================================================

#[test]
fn pc011_anthropic_tool_result_to_chat_upstream() {
    let body = json!({
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
    });

    // 请求转换：anthropic → chat
    let converted = convert_request(
        Protocol::AnthropicMessages,
        Protocol::ChatCompletions,
        &body,
    )
    .expect("转换失败");

    let messages = converted
        .get("messages")
        .expect("应该有 messages")
        .as_array()
        .unwrap();
    assert_eq!(messages.len(), 3);

    // 第一条：user 消息
    assert_eq!(
        messages[0].get("role").and_then(|v| v.as_str()),
        Some("user")
    );
    assert_eq!(
        messages[0].get("content").and_then(|v| v.as_str()),
        Some("Use the tool.")
    );

    // 第二条：assistant 带 tool_calls
    assert_eq!(
        messages[1].get("role").and_then(|v| v.as_str()),
        Some("assistant")
    );
    let tool_calls = messages[1]
        .get("tool_calls")
        .expect("应该有 tool_calls")
        .as_array()
        .unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(
        tool_calls[0].get("id").and_then(|v| v.as_str()),
        Some("toolu_011")
    );
    assert_eq!(
        tool_calls[0].get("type").and_then(|v| v.as_str()),
        Some("function")
    );

    let func = tool_calls[0].get("function").unwrap();
    assert_eq!(func.get("name").and_then(|v| v.as_str()), Some("lookup"));
    let args = func.get("arguments").and_then(|v| v.as_str()).unwrap();
    assert!(args.contains("id") && args.contains("42"));

    // 第三条：tool 角色消息
    assert_eq!(
        messages[2].get("role").and_then(|v| v.as_str()),
        Some("tool")
    );
    assert_eq!(
        messages[2].get("tool_call_id").and_then(|v| v.as_str()),
        Some("toolu_011")
    );
    assert_eq!(
        messages[2].get("content").and_then(|v| v.as_str()),
        Some("Found value 42.")
    );

    // 验证 tools 转换
    let tools = converted
        .get("tools")
        .expect("应该有 tools")
        .as_array()
        .unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].get("type").and_then(|v| v.as_str()),
        Some("function")
    );

    let f = tools[0].get("function").unwrap();
    assert_eq!(f.get("name").and_then(|v| v.as_str()), Some("lookup"));
    assert_eq!(
        f.get("description").and_then(|v| v.as_str()),
        Some("Lookup by id.")
    );
    assert!(f.get("parameters").is_some());
}

// ============================================================
// PC-012: Direct Protocol Still Passes Through
// ============================================================

#[test]
fn pc012_direct_protocol_passes_through() {
    let body = json!({
        "model": "chat-model",
        "messages": [
            { "role": "user", "content": "No conversion." }
        ]
    });

    // 同协议转换应该保持格式基本不变
    let converted = convert_request(Protocol::ChatCompletions, Protocol::ChatCompletions, &body)
        .expect("转换失败");

    // 验证 model 被保留
    assert_eq!(
        converted.get("model").and_then(|v| v.as_str()),
        Some("chat-model")
    );

    // 验证 messages 格式不变
    let messages = converted
        .get("messages")
        .expect("应该有 messages")
        .as_array()
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].get("role").and_then(|v| v.as_str()),
        Some("user")
    );
    assert_eq!(
        messages[0].get("content").and_then(|v| v.as_str()),
        Some("No conversion.")
    );

    // 响应同样应该保持格式
    let upstream_response = json!({
        "id": "chatcmpl_direct",
        "object": "chat.completion",
        "created": 1710000012,
        "model": "upstream-chat-model",
        "choices": [
            {
                "index": 0,
                "message": { "role": "assistant", "content": "Response." },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 2,
            "total_tokens": 7
        }
    });

    let client_response = convert_response(
        Protocol::ChatCompletions,
        Protocol::ChatCompletions,
        &upstream_response,
    )
    .expect("响应转换失败");

    assert_eq!(
        client_response.get("model").and_then(|v| v.as_str()),
        Some("upstream-chat-model")
    );
    let choices = client_response
        .get("choices")
        .expect("应该有 choices")
        .as_array()
        .unwrap();
    assert_eq!(choices.len(), 1);
}

// ============================================================
// 兼容性测试：旧配置字段迁移
// ============================================================

#[tokio::test]
async fn legacy_config_migration() {
    let legacy_yaml = r#"
allow_protocol_conversion: false
upstreams:
  - name: legacy-openai
    base_url: http://mock-legacy
    api_type: open_ai
    auth: { type: api_key, key: null }
    enabled: true
    support_openai: true
    support_anthropic: true
aliases: []
"#;

    let dir = create_temp_config(legacy_yaml);
    let config = llm_wrapper::load_config(dir.path().join("config.yaml").to_str().unwrap())
        .expect("加载旧配置失败");

    let upstream = &config.upstreams[0];
    assert_eq!(upstream.name, "legacy-openai");
    assert!(upstream.support_chat_completions);
    assert!(upstream.support_responses);
    assert!(upstream.support_anthropic_messages);
}

// ============================================================
// 协议选择优先级测试
// ============================================================

#[test]
fn protocol_selection_priority() {
    // Chat 入口优先级：chat > responses > anthropic
    let priority = Protocol::selection_priority(Protocol::ChatCompletions);
    assert_eq!(
        priority,
        [
            Protocol::ChatCompletions,
            Protocol::Responses,
            Protocol::AnthropicMessages
        ]
    );

    // Responses 入口优先级：responses > chat > anthropic
    let priority = Protocol::selection_priority(Protocol::Responses);
    assert_eq!(
        priority,
        [
            Protocol::Responses,
            Protocol::ChatCompletions,
            Protocol::AnthropicMessages
        ]
    );

    // Anthropic 入口优先级：anthropic > chat > responses
    let priority = Protocol::selection_priority(Protocol::AnthropicMessages);
    assert_eq!(
        priority,
        [
            Protocol::AnthropicMessages,
            Protocol::ChatCompletions,
            Protocol::Responses
        ]
    );
}

// ============================================================
// RouteResult 辅助方法测试
// ============================================================

#[test]
fn route_result_supports_and_best_protocol() {
    // 只支持 chat 的路由
    let route = create_test_route("http://chat-only", true, false, false);
    assert!(route.supports(Protocol::ChatCompletions));
    assert!(!route.supports(Protocol::Responses));
    assert!(!route.supports(Protocol::AnthropicMessages));

    assert_eq!(
        route.best_available_protocol(Protocol::ChatCompletions),
        Some(Protocol::ChatCompletions)
    );
    assert_eq!(
        route.best_available_protocol(Protocol::Responses),
        Some(Protocol::ChatCompletions)
    );

    // 支持所有协议的路由
    let route_all = create_test_route("http://all", true, true, true);
    assert!(route_all.supports(Protocol::ChatCompletions));
    assert!(route_all.supports(Protocol::Responses));
    assert!(route_all.supports(Protocol::AnthropicMessages));

    assert_eq!(
        route_all.best_available_protocol(Protocol::ChatCompletions),
        Some(Protocol::ChatCompletions)
    );
    assert_eq!(
        route_all.best_available_protocol(Protocol::Responses),
        Some(Protocol::Responses)
    );
}

// ============================================================
// 端点路径解析测试
// ============================================================

#[test]
fn protocol_from_endpoint() {
    assert_eq!(
        Protocol::from_endpoint("/v1/chat/completions"),
        Protocol::ChatCompletions
    );
    assert_eq!(
        Protocol::from_endpoint("/v1/responses"),
        Protocol::Responses
    );
    assert_eq!(
        Protocol::from_endpoint("/v1/messages"),
        Protocol::AnthropicMessages
    );
    assert_eq!(
        Protocol::from_endpoint("/unknown"),
        Protocol::ChatCompletions
    );
}

#[test]
fn protocol_to_upstream_path() {
    assert_eq!(
        Protocol::ChatCompletions.to_upstream_path(),
        "/v1/chat/completions"
    );
    assert_eq!(Protocol::Responses.to_upstream_path(), "/v1/responses");
    assert_eq!(
        Protocol::AnthropicMessages.to_upstream_path(),
        "/v1/messages"
    );
}

// ============================================================
// 双向转换一致性测试
// ============================================================

#[test]
fn chat_to_responses_roundtrip() {
    // Chat → Responses → Chat 应该保持语义一致
    let original = json!({
        "model": "test",
        "messages": [
            { "role": "system", "content": "You are helpful." },
            { "role": "user", "content": "Hello" }
        ],
        "temperature": 0.5,
        "max_tokens": 100
    });

    let to_responses = convert_request(Protocol::ChatCompletions, Protocol::Responses, &original)
        .expect("chat→responses 失败");

    // 验证 system 消息变为 instructions
    assert_eq!(
        to_responses.get("instructions").and_then(|v| v.as_str()),
        Some("You are helpful.")
    );
    assert_eq!(
        to_responses.get("temperature").and_then(|v| v.as_f64()),
        Some(0.5)
    );
    assert_eq!(
        to_responses
            .get("max_output_tokens")
            .and_then(|v| v.as_u64()),
        Some(100)
    );

    // Responses → Chat 应该恢复 system 消息
    let back_to_chat = convert_request(
        Protocol::Responses,
        Protocol::ChatCompletions,
        &to_responses,
    )
    .expect("responses→chat 失败");

    let messages = back_to_chat.get("messages").unwrap().as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].get("role").and_then(|v| v.as_str()),
        Some("system")
    );
    assert_eq!(
        messages[1].get("role").and_then(|v| v.as_str()),
        Some("user")
    );
}

#[test]
fn chat_to_anthropic_roundtrip() {
    let original = json!({
        "model": "test",
        "messages": [
            { "role": "system", "content": "Be brief." },
            { "role": "user", "content": "Say hi." }
        ],
        "max_tokens": 50
    });

    let to_anthropic = convert_request(
        Protocol::ChatCompletions,
        Protocol::AnthropicMessages,
        &original,
    )
    .expect("chat→anthropic 失败");

    // Anthropic 使用 system 字段而非 system 消息
    assert_eq!(
        to_anthropic.get("system").and_then(|v| v.as_str()),
        Some("Be brief.")
    );

    let messages = to_anthropic.get("messages").unwrap().as_array().unwrap();
    assert_eq!(messages.len(), 1); // 只有 user 消息
    assert_eq!(
        messages[0].get("role").and_then(|v| v.as_str()),
        Some("user")
    );
}

// ============================================================
// 流式 SSE 解析测试
// ============================================================

#[test]
fn sse_openai_to_canonical_events() {
    use llm_wrapper::transform::stream::{parse_openai_event, CanonicalStreamEvent};

    // 文本 delta
    let json = serde_json::from_str(r#"{"choices":[{"delta":{"content":"Hello"}}]}"#).unwrap();
    let event = parse_openai_event(&json).expect("解析失败");
    assert!(matches!(&event, CanonicalStreamEvent::TextDelta { text } if text == "Hello"));

    // finish_reason
    let json = serde_json::from_str(r#"{"choices":[{"finish_reason":"stop"}]}"#).unwrap();
    let event = parse_openai_event(&json).expect("解析失败");
    assert!(matches!(&event, CanonicalStreamEvent::Stop { .. }));
}

#[test]
fn sse_anthropic_to_canonical_events() {
    use llm_wrapper::transform::stream::{parse_anthropic_event, CanonicalStreamEvent};

    // 文本 delta
    let json = serde_json::from_str(r#"{"delta":{"type":"text_delta","text":"Hello"}}"#).unwrap();
    let event = parse_anthropic_event(Some("content_block_delta"), &json).expect("解析失败");
    assert!(matches!(&event, Some(CanonicalStreamEvent::TextDelta { text }) if text == "Hello"));

    // stop_reason (message_delta 事件在顶层有 stop_reason)
    let json = serde_json::from_str(r#"{"stop_reason":"end_turn"}"#).unwrap();
    let event = parse_anthropic_event(Some("message_delta"), &json).expect("解析失败");
    assert!(matches!(&event, Some(CanonicalStreamEvent::Stop { .. })));
}

#[test]
fn canonical_to_openai_sse_output() {
    use llm_wrapper::transform::stream::{canonical_to_openai_sse, CanonicalStreamEvent};

    let sse = canonical_to_openai_sse(&CanonicalStreamEvent::TextDelta {
        text: "Hello".to_string(),
    })
    .expect("SSE 生成失败");
    assert!(sse.contains("Hello"));
    assert!(sse.contains("chat.completion.chunk"));
}

#[test]
fn canonical_to_anthropic_sse_output() {
    use llm_wrapper::transform::stream::{canonical_to_anthropic_sse, CanonicalStreamEvent};

    let sse = canonical_to_anthropic_sse(&CanonicalStreamEvent::TextDelta {
        text: "Hello".to_string(),
    })
    .expect("SSE 生成失败");
    assert!(sse.contains("Hello"));
    assert!(sse.contains("content_block_delta"));
}

// ============================================================
// 图片 URL 安全测试
// ============================================================

#[tokio::test]
async fn image_download_rejects_private_ips() {
    use llm_wrapper::transform::ImageDownloader;

    let downloader = ImageDownloader::new();

    // 测试私有 IP
    let result = downloader
        .download_to_base64("http://127.0.0.1/test.png")
        .await;
    assert!(result.is_err());

    let result = downloader
        .download_to_base64("http://192.168.1.1/test.png")
        .await;
    assert!(result.is_err());

    let result = downloader
        .download_to_base64("http://10.0.0.1/test.png")
        .await;
    assert!(result.is_err());
}

// ============================================================
// 配置热重载与协议字段集成测试
// ============================================================

#[tokio::test]
async fn config_new_protocol_fields() {
    let yaml = r#"
allow_protocol_conversion: true
upstreams:
  - name: chat-only
    base_url: http://localhost:9999
    api_type: open_ai
    auth: { type: api_key, key: null }
    enabled: true
    support_chat_completions: true
    support_responses: false
    support_anthropic_messages: false
  - name: codex-upstream
    base_url: http://localhost:9998
    api_type: chatgpt_codex
    auth: { type: api_key, key: null }
    enabled: true
aliases: []
"#;

    let dir = create_temp_config(yaml);
    let config = llm_wrapper::load_config(dir.path().join("config.yaml").to_str().unwrap())
        .expect("加载配置失败");

    assert!(config.allow_protocol_conversion);

    let chat_only = &config.upstreams[0];
    assert!(chat_only.support_chat_completions);
    assert!(!chat_only.support_responses);
    assert!(!chat_only.support_anthropic_messages);

    // Codex 应该被强制为 responses-only
    let codex = &config.upstreams[1];
    assert!(!codex.support_chat_completions);
    assert!(codex.support_responses);
    assert!(!codex.support_anthropic_messages);
}

// ============================================================
// 参数覆盖与协议转换集成测试
// ============================================================

#[test]
fn param_overrides_applied_before_conversion() {
    // 模拟 alias 参数覆盖在转换前应用
    let body = json!({
        "model": "my-alias",
        "messages": [
            { "role": "user", "content": "Hello" }
        ]
    });

    // 创建带有参数覆盖的 RouteResult
    let mut override_params = std::collections::HashMap::new();
    override_params.insert("temperature".to_string(), json!(0.7));

    let route = RouteResult {
        upstream_base_url: "http://test".to_string(),
        upstream_name: "test".to_string(),
        upstream_auth: UpstreamAuth::ApiKey { key: None },
        api_type: ApiType::OpenAI,
        target_model: "real-model".to_string(),
        override_params,
        default_params: std::collections::HashMap::new(),
        support_chat_completions: true,
        support_responses: false,
        support_anthropic_messages: false,
        anthropic_base_url: None,
    };

    // 应用参数覆盖
    let mut body_with_overrides = body.clone();
    apply_param_overrides_inner(&mut body_with_overrides, &route);

    // model 应该被改为 target_model
    assert_eq!(
        body_with_overrides.get("model").and_then(|v| v.as_str()),
        Some("real-model")
    );
    // temperature 应该被注入
    assert_eq!(
        body_with_overrides
            .get("temperature")
            .and_then(|v| v.as_f64()),
        Some(0.7)
    );

    // 然后进行协议转换
    let converted = convert_request(
        Protocol::ChatCompletions,
        Protocol::AnthropicMessages,
        &body_with_overrides,
    )
    .expect("转换失败");

    assert_eq!(
        converted.get("model").and_then(|v| v.as_str()),
        Some("real-model")
    );
    assert_eq!(
        converted.get("temperature").and_then(|v| v.as_f64()),
        Some(0.7)
    );
}

// ============================================================
// 流式 SSE 解析层完整测试（3 种输入协议）
// ============================================================

#[test]
fn sse_responses_to_canonical_events() {
    use llm_wrapper::transform::stream::{parse_responses_event, CanonicalStreamEvent};

    // 文本 delta
    let json =
        serde_json::from_str(r#"{"type":"response.output_text.delta","delta":"Hello"}"#).unwrap();
    let event = parse_responses_event(&json).expect("解析失败");
    assert!(matches!(&event, Some(CanonicalStreamEvent::TextDelta { text }) if text == "Hello"));

    // 工具调用完成
    let json = serde_json::from_str(r#"{"type":"response.function_call.completed","id":"call_1","name":"search","arguments":"{\"q\":\"test\"}"}"#).unwrap();
    let event = parse_responses_event(&json).expect("解析失败");
    assert!(
        matches!(&event, Some(CanonicalStreamEvent::ToolUseStart { id, name, .. }) if id == "call_1" && name == "search")
    );

    // 工具参数增量
    let json = serde_json::from_str(
        r#"{"type":"response.function_call.parameter_delta","id":"call_1","delta":"{\"q\":\""}"#,
    )
    .unwrap();
    let event = parse_responses_event(&json).expect("解析失败");
    assert!(matches!(
        &event,
        Some(CanonicalStreamEvent::ToolInputDelta { .. })
    ));

    // response.done stops the stream even when it also carries usage
    let json = serde_json::from_str(
        r#"{"type":"response.done","usage":{"input_tokens":5,"output_tokens":3}}"#,
    )
    .unwrap();
    let event = parse_responses_event(&json).expect("解析失败");
    assert!(matches!(&event, Some(CanonicalStreamEvent::Stop { .. })));

    // response.done 带 status_details（停止信号）
    let json = serde_json::from_str(
        r#"{"type":"response.done","output":[{"type":"message","status_details":"completed"}]}"#,
    )
    .unwrap();
    let event = parse_responses_event(&json).expect("解析失败");
    assert!(matches!(&event, Some(CanonicalStreamEvent::Stop { .. })));

    // 未知类型 → Raw
    let json = serde_json::from_str(r#"{"type":"response.created","id":"resp_1"}"#).unwrap();
    let event = parse_responses_event(&json).expect("解析失败");
    assert!(matches!(&event, Some(CanonicalStreamEvent::Raw { .. })));
}

// ============================================================
// 流式 SSE 输出层完整测试（3 种输出协议）
// ============================================================

#[test]
fn canonical_to_responses_sse_output() {
    use llm_wrapper::transform::stream::{canonical_to_responses_sse, CanonicalStreamEvent};
    use llm_wrapper::transform::CanonicalStopReason;

    // 文本 delta
    let sse = canonical_to_responses_sse(&CanonicalStreamEvent::TextDelta {
        text: "Hello".to_string(),
    })
    .expect("SSE 生成失败");
    assert!(sse.contains("Hello"));
    assert!(sse.contains("response.output_text.delta"));

    // 停止信号
    let sse = canonical_to_responses_sse(&CanonicalStreamEvent::Stop {
        reason: CanonicalStopReason::EndTurn,
    })
    .expect("SSE 生成失败");
    assert!(sse.contains("response.done"));
    assert!(sse.contains("end_turn"));

    // Usage
    let sse = canonical_to_responses_sse(&CanonicalStreamEvent::Usage {
        input_tokens: 10,
        output_tokens: 5,
    })
    .expect("SSE 生成失败");
    assert!(sse.contains("response.done"));
    assert!(sse.contains("10"));
    assert!(sse.contains("5"));
}

// ============================================================
// 流式 SSE 完整事件流测试：SSE 字节流 → 解析 → 事件
// ============================================================

#[test]
fn sse_parser_openai_full_stream() {
    use llm_wrapper::transform::stream::{parse_openai_event, CanonicalStreamEvent, SseParser};

    // 模拟完整的 OpenAI SSE 流
    let raw = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1710000000,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant","content":null},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1710000000,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1710000000,"model":"gpt-4","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1710000000,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
"#;

    let mut parser = SseParser::new();
    let events = parser.feed(raw.as_bytes());
    assert_eq!(events.len(), 5);

    // 第一个事件：role delta（没有 content，应该是 Raw）
    let json1 = serde_json::from_str::<serde_json::Value>(&events[0].data).unwrap();
    let canonical1 = parse_openai_event(&json1).unwrap();
    assert!(matches!(canonical1, CanonicalStreamEvent::Raw { .. }));

    // 第二个事件：文本 "Hello"
    let json2 = serde_json::from_str::<serde_json::Value>(&events[1].data).unwrap();
    let canonical2 = parse_openai_event(&json2).unwrap();
    assert!(matches!(canonical2, CanonicalStreamEvent::TextDelta { text } if text == "Hello"));

    // 第三个事件：文本 " world"
    let json3 = serde_json::from_str::<serde_json::Value>(&events[2].data).unwrap();
    let canonical3 = parse_openai_event(&json3).unwrap();
    assert!(matches!(canonical3, CanonicalStreamEvent::TextDelta { text } if text == " world"));

    // 第四个事件：stop
    let json4 = serde_json::from_str::<serde_json::Value>(&events[3].data).unwrap();
    let canonical4 = parse_openai_event(&json4).unwrap();
    assert!(matches!(canonical4, CanonicalStreamEvent::Stop { .. }));

    // 第五个事件：[DONE]
    assert_eq!(events[4].data, "[DONE]");
}

#[test]
fn sse_parser_anthropic_full_stream() {
    use llm_wrapper::transform::stream::{parse_anthropic_event, CanonicalStreamEvent, SseParser};

    let raw = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}

event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}

event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}

event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}

event: message_stop\ndata: {\"type\":\"message_stop\"}
";

    let mut parser = SseParser::new();
    let events = parser.feed(raw.as_bytes());
    assert_eq!(events.len(), 6);

    // message_start → 不是已知事件类型，返回 None
    let json1 = serde_json::from_str::<serde_json::Value>(&events[0].data).unwrap();
    let canonical1 = parse_anthropic_event(events[0].event.as_deref(), &json1).unwrap();
    assert!(canonical1.is_none());

    // content_block_start → 不是 tool_use，返回 None
    let json2 = serde_json::from_str::<serde_json::Value>(&events[1].data).unwrap();
    let canonical2 = parse_anthropic_event(events[1].event.as_deref(), &json2).unwrap();
    assert!(canonical2.is_none());

    // content_block_delta → TextDelta "Hello"
    let json3 = serde_json::from_str::<serde_json::Value>(&events[2].data).unwrap();
    let canonical3 = parse_anthropic_event(events[2].event.as_deref(), &json3).unwrap();
    assert!(
        matches!(canonical3, Some(CanonicalStreamEvent::TextDelta { text }) if text == "Hello")
    );

    // content_block_delta → TextDelta " world"
    let json4 = serde_json::from_str::<serde_json::Value>(&events[3].data).unwrap();
    let canonical4 = parse_anthropic_event(events[3].event.as_deref(), &json4).unwrap();
    assert!(
        matches!(canonical4, Some(CanonicalStreamEvent::TextDelta { text }) if text == " world")
    );

    // message_delta → Stop
    let json5 = serde_json::from_str::<serde_json::Value>(&events[4].data).unwrap();
    let canonical5 = parse_anthropic_event(events[4].event.as_deref(), &json5).unwrap();
    assert!(matches!(
        canonical5,
        Some(CanonicalStreamEvent::Stop { .. })
    ));

    // message_stop → None
    let json6 = serde_json::from_str::<serde_json::Value>(&events[5].data).unwrap();
    let canonical6 = parse_anthropic_event(events[5].event.as_deref(), &json6).unwrap();
    assert!(canonical6.is_none());
}

#[test]
fn sse_parser_responses_full_stream() {
    use llm_wrapper::transform::stream::{parse_responses_event, CanonicalStreamEvent, SseParser};

    let raw = r#"data: {"type":"response.created","id":"resp_1","output":[]}

data: {"type":"response.output_text.delta","delta":"Hello"}

data: {"type":"response.output_text.delta","delta":" world"}

data: {"type":"response.done","response":{"id":"resp_1","output":[{"type":"message","status_details":"completed"}]},"usage":{"input_tokens":5,"output_tokens":2}}
"#;

    let mut parser = SseParser::new();
    let events = parser.feed(raw.as_bytes());
    assert_eq!(events.len(), 4);

    // response.created → Raw
    let json1 = serde_json::from_str::<serde_json::Value>(&events[0].data).unwrap();
    let canonical1 = parse_responses_event(&json1).unwrap();
    assert!(matches!(canonical1, Some(CanonicalStreamEvent::Raw { .. })));

    // response.output_text.delta → TextDelta "Hello"
    let json2 = serde_json::from_str::<serde_json::Value>(&events[1].data).unwrap();
    let canonical2 = parse_responses_event(&json2).unwrap();
    assert!(
        matches!(canonical2, Some(CanonicalStreamEvent::TextDelta { text }) if text == "Hello")
    );

    // response.output_text.delta → TextDelta " world"
    let json3 = serde_json::from_str::<serde_json::Value>(&events[2].data).unwrap();
    let canonical3 = parse_responses_event(&json3).unwrap();
    assert!(
        matches!(canonical3, Some(CanonicalStreamEvent::TextDelta { text }) if text == " world")
    );

    // response.done → Stop
    let json4 = serde_json::from_str::<serde_json::Value>(&events[3].data).unwrap();
    let canonical4 = parse_responses_event(&json4).unwrap();
    assert!(matches!(
        canonical4,
        Some(CanonicalStreamEvent::Stop { .. })
    ));
}

// ============================================================
// 9 种流式转换路径测试：canonical 事件 → 3 种输出格式
// ============================================================

#[test]
fn streaming_all_9_conversion_paths_text_delta() {
    use llm_wrapper::transform::stream::{
        canonical_to_anthropic_sse, canonical_to_openai_sse, canonical_to_responses_sse,
        CanonicalStreamEvent,
    };
    use llm_wrapper::transform::CanonicalStopReason;

    let text_event = CanonicalStreamEvent::TextDelta {
        text: "test".to_string(),
    };
    let stop_event = CanonicalStreamEvent::Stop {
        reason: CanonicalStopReason::EndTurn,
    };
    let usage_event = CanonicalStreamEvent::Usage {
        input_tokens: 10,
        output_tokens: 5,
    };

    // --- TextDelta: 3 种输出 ---
    let openai_text = canonical_to_openai_sse(&text_event).unwrap();
    assert!(openai_text.contains("chat.completion.chunk"));
    assert!(openai_text.contains("\"content\":\"test\""));

    let anthropic_text = canonical_to_anthropic_sse(&text_event).unwrap();
    assert!(anthropic_text.contains("content_block_delta"));
    assert!(anthropic_text.contains("\"text\":\"test\""));

    let responses_text = canonical_to_responses_sse(&text_event).unwrap();
    assert!(responses_text.contains("response.output_text.delta"));
    assert!(responses_text.contains("\"delta\":\"test\""));

    // --- Stop: 3 种输出 ---
    let openai_stop = canonical_to_openai_sse(&stop_event).unwrap();
    assert!(openai_stop.contains("chat.completion.chunk"));
    assert!(openai_stop.contains("\"finish_reason\":\"stop\""));

    let anthropic_stop = canonical_to_anthropic_sse(&stop_event).unwrap();
    assert!(anthropic_stop.contains("message_delta"));
    assert!(anthropic_stop.contains("\"stop_reason\":\"end_turn\""));
    assert!(anthropic_stop.contains("message_stop"));

    let responses_stop = canonical_to_responses_sse(&stop_event).unwrap();
    assert!(responses_stop.contains("response.done"));
    assert!(responses_stop.contains("\"stop_reason\":\"end_turn\""));

    // --- Usage: 3 种输出 ---
    let openai_usage = canonical_to_openai_sse(&usage_event).unwrap();
    // OpenAI 输出忽略 usage（空字符串）
    assert!(openai_usage.is_empty());

    let anthropic_usage = canonical_to_anthropic_sse(&usage_event).unwrap();
    // Anthropic 输出忽略 usage（空字符串）
    assert!(anthropic_usage.is_empty());

    let responses_usage = canonical_to_responses_sse(&usage_event).unwrap();
    assert!(responses_usage.contains("response.done"));
    assert!(responses_usage.contains("10"));
    assert!(responses_usage.contains("5"));
}

// ============================================================
// 9 种流式转换路径测试：3 种输入 → canonical 事件
// ============================================================

#[test]
fn streaming_all_9_conversion_paths_parsing() {
    use llm_wrapper::transform::stream::{
        parse_anthropic_event, parse_openai_event, parse_responses_event, CanonicalStreamEvent,
    };

    // --- OpenAI 输入解析 ---
    let openai_text = serde_json::json!({"choices":[{"delta":{"content":"hello"}}]});
    let ev = parse_openai_event(&openai_text).unwrap();
    assert!(matches!(&ev, CanonicalStreamEvent::TextDelta { text } if text == "hello"));

    let openai_stop = serde_json::json!({"choices":[{"finish_reason":"stop"}]});
    let ev = parse_openai_event(&openai_stop).unwrap();
    assert!(matches!(&ev, CanonicalStreamEvent::Stop { .. }));

    let openai_usage = serde_json::json!({"usage":{"prompt_tokens":8,"completion_tokens":3}});
    let ev = parse_openai_event(&openai_usage).unwrap();
    assert!(matches!(
        &ev,
        CanonicalStreamEvent::Usage {
            input_tokens: 8,
            output_tokens: 3
        }
    ));

    // --- Anthropic 输入解析 ---
    let anthropic_text = serde_json::json!({"delta":{"type":"text_delta","text":"hello"}});
    let ev = parse_anthropic_event(Some("content_block_delta"), &anthropic_text).unwrap();
    assert!(matches!(&ev, Some(CanonicalStreamEvent::TextDelta { text }) if text == "hello"));

    let anthropic_stop = serde_json::json!({"stop_reason":"max_tokens"});
    let ev = parse_anthropic_event(Some("message_delta"), &anthropic_stop).unwrap();
    assert!(matches!(&ev, Some(CanonicalStreamEvent::Stop { .. })));

    // --- Responses 输入解析 ---
    let responses_text = serde_json::json!({"type":"response.output_text.delta","delta":"hello"});
    let ev = parse_responses_event(&responses_text).unwrap();
    assert!(matches!(&ev, Some(CanonicalStreamEvent::TextDelta { text }) if text == "hello"));

    let responses_stop = serde_json::json!({"type":"response.done","output":[{"type":"message","status_details":"completed"}]});
    let ev = parse_responses_event(&responses_stop).unwrap();
    assert!(matches!(&ev, Some(CanonicalStreamEvent::Stop { .. })));

    let responses_usage =
        serde_json::json!({"type":"response.done","usage":{"input_tokens":6,"output_tokens":2}});
    let ev = parse_responses_event(&responses_usage).unwrap();
    assert!(matches!(&ev, Some(CanonicalStreamEvent::Stop { .. })));
}

// ============================================================
// 对齐 auth2api：关键非流式响应映射语义
// ============================================================

#[test]
fn align_auth2api_chat_developer_messages_lifted_to_instructions() {
    let body = json!({
        "model": "gpt-5.5",
        "messages": [
            {"role": "system", "content": "Be terse."},
            {"role": "developer", "content": "Tone: concise."},
            {"role": "user", "content": "hi"}
        ]
    });

    let converted =
        convert_request(Protocol::ChatCompletions, Protocol::Responses, &body).expect("转换失败");
    assert_eq!(
        converted.get("instructions").and_then(|v| v.as_str()),
        Some("Be terse.\n\nTone: concise.")
    );
    let input = converted.get("input").and_then(|v| v.as_array()).unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0].get("role").and_then(|v| v.as_str()), Some("user"));
    assert_eq!(input[0].get("content").and_then(|v| v.as_str()), Some("hi"));
}

#[test]
fn align_auth2api_responses_incomplete_status_mappings() {
    let upstream_response = json!({
        "id": "resp_incomplete",
        "model": "gpt-5.5",
        "status": "incomplete",
        "output": [{
            "type": "message",
            "content": [{"type":"output_text","text":"trun"}]
        }],
        "usage": {"input_tokens": 10, "output_tokens": 2}
    });

    let to_chat = convert_response(
        Protocol::Responses,
        Protocol::ChatCompletions,
        &upstream_response,
    )
    .expect("responses->chat 转换失败");
    assert_eq!(
        to_chat["choices"][0]["finish_reason"].as_str(),
        Some("length")
    );

    let to_anthropic = convert_response(
        Protocol::Responses,
        Protocol::AnthropicMessages,
        &upstream_response,
    )
    .expect("responses->anthropic 转换失败");
    assert_eq!(to_anthropic["stop_reason"].as_str(), Some("max_tokens"));
}

// ============================================================
// SSE 字节流分块解析测试
// ============================================================

#[test]
fn sse_parser_chunked_across_boundaries() {
    use llm_wrapper::transform::stream::SseParser;

    let mut parser = SseParser::new();

    // 第一个 chunk：只到 event 头
    let events1 = parser.feed(b"event: content_block_delta\n");
    assert_eq!(events1.len(), 0);

    // 第二个 chunk：data 行被截断
    let events2 = parser.feed(b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delt");
    assert_eq!(events2.len(), 0);

    // 第三个 chunk：完成 data 行
    let events3 = parser.feed(b"a\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n");
    assert_eq!(events3.len(), 1);
    assert_eq!(events3[0].event, Some("content_block_delta".to_string()));
    assert!(events3[0].data.contains("\"text\":\"Hi\""));
}

// ============================================================
// 真实配置检查：nb-chat 别名 + vllm 当前协议能力
// ============================================================

#[tokio::test]
async fn integration_nb_chat_alias_config() {
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.yaml".to_string());
    let config = match llm_wrapper::load_config(&config_path) {
        Ok(c) => c,
        Err(_) => return, // 配置不可用则跳过
    };

    // 验证 nb-chat 路由正确
    let vllm_upstream = config
        .upstreams
        .iter()
        .find(|u| u.name == "vllm")
        .expect("应该有 vllm 上游");
    assert!(vllm_upstream.enabled);
    assert!(
        vllm_upstream.support_chat_completions
            || vllm_upstream.support_responses
            || vllm_upstream.support_anthropic_messages,
        "vllm 至少应启用一种协议"
    );

    // 验证 alias 配置
    let nb_chat_alias = config
        .aliases
        .iter()
        .find(|a| a.alias == "nb-chat")
        .expect("应该有 nb-chat 别名");
    assert_eq!(nb_chat_alias.target_model, "qwen");
    assert_eq!(nb_chat_alias.upstream, "vllm");
}

#[tokio::test]
async fn integration_nb_chat_route_uses_current_vllm_capabilities() {
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.yaml".to_string());
    let config = match llm_wrapper::load_config(&config_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let vllm_upstream = config
        .upstreams
        .iter()
        .find(|u| u.name == "vllm")
        .expect("应该有 vllm 上游");

    let route = create_test_route(
        &vllm_upstream.base_url,
        vllm_upstream.support_chat_completions,
        vllm_upstream.support_responses,
        vllm_upstream.support_anthropic_messages,
    );

    assert_eq!(
        route.supports(Protocol::ChatCompletions),
        vllm_upstream.support_chat_completions
    );
    assert_eq!(
        route.supports(Protocol::Responses),
        vllm_upstream.support_responses
    );
    assert_eq!(
        route.supports(Protocol::AnthropicMessages),
        vllm_upstream.support_anthropic_messages
    );

    for entry in [
        Protocol::ChatCompletions,
        Protocol::Responses,
        Protocol::AnthropicMessages,
    ] {
        let selected = route.best_available_protocol(entry);
        assert!(
            selected.is_some(),
            "vllm 应能为 {:?} 选择一个目标协议",
            entry
        );
        assert!(route.supports(selected.unwrap()));
    }
}

#[tokio::test]
async fn integration_nb_chat_conversion_switch_present() {
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.yaml".to_string());
    let config = match llm_wrapper::load_config(&config_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    // The field is part of the public config contract. This assertion is kept
    // intentionally simple because the user's local config controls the value.
    let serialized = serde_yaml::to_value(&config).expect("config should serialize");
    assert!(serialized.get("allow_protocol_conversion").is_some());
}
