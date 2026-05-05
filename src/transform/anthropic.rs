//! Anthropic Messages <-> Canonical 转换
//! Phase 3 实现

use super::canonical::*;
use anyhow::Result;
use serde_json::json;

/// 解析 Anthropic content block 为 CanonicalContentBlock
fn parse_content_block(block: &serde_json::Value) -> Vec<CanonicalContentBlock> {
    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match block_type {
        "text" => {
            let text = block
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                vec![]
            } else {
                vec![CanonicalContentBlock::Text { text }]
            }
        }
        "image" => {
            let source = block.get("source");
            match source {
                Some(src) => parse_image_source(src),
                None => vec![],
            }
        }
        "tool_use" => {
            let id = block
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            let name = block
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let input = block.get("input").cloned().unwrap_or(json!({}));

            vec![CanonicalContentBlock::ToolUse { id, name, input }]
        }
        "thinking" => {
            // vllm thinking block: {"type": "thinking", "thinking": "...", "signature": "..."}
            if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    return vec![CanonicalContentBlock::Reasoning {
                        text: text.to_string(),
                    }];
                }
            }
            vec![]
        }
        "tool_result" => {
            let tool_use_id = block
                .get("tool_use_id")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let content_val = block.get("content");

            let content = match content_val {
                Some(serde_json::Value::String(s)) => {
                    if s.is_empty() {
                        vec![]
                    } else {
                        vec![CanonicalContentBlock::Text { text: s.clone() }]
                    }
                }
                Some(serde_json::Value::Array(blocks)) => {
                    let mut result = vec![];
                    for b in blocks {
                        result.extend(parse_content_block(b));
                    }
                    result
                }
                _ => vec![],
            };

            vec![CanonicalContentBlock::ToolResult {
                tool_use_id,
                content,
            }]
        }
        _ => vec![],
    }
}

/// 解析 Anthropic image source
fn parse_image_source(source: &serde_json::Value) -> Vec<CanonicalContentBlock> {
    let source_type = source.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match source_type {
        "base64" => {
            let media_type = source
                .get("media_type")
                .and_then(|m| m.as_str())
                .unwrap_or("image/png")
                .to_string();
            let data = source
                .get("data")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();

            vec![CanonicalContentBlock::Image {
                source: CanonicalImageSource::Base64 { media_type, data },
            }]
        }
        "url" => {
            let url = source
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();

            vec![CanonicalContentBlock::Image {
                source: CanonicalImageSource::Url { url },
            }]
        }
        _ => vec![],
    }
}

/// 解析 Anthropic message 的 content（string 或 array）
fn parse_message_content(content: &serde_json::Value) -> Vec<CanonicalContentBlock> {
    match content {
        serde_json::Value::String(text) => {
            if text.is_empty() {
                vec![]
            } else {
                vec![CanonicalContentBlock::Text { text: text.clone() }]
            }
        }
        serde_json::Value::Array(blocks) => {
            let mut result = vec![];
            for block in blocks {
                result.extend(parse_content_block(block));
            }
            result
        }
        _ => vec![],
    }
}

/// 解析 Anthropic system 字段（string 或 content block array）
fn parse_system(system: &serde_json::Value) -> Option<Vec<CanonicalContentBlock>> {
    match system {
        serde_json::Value::String(text) => {
            if text.is_empty() {
                None
            } else {
                Some(vec![CanonicalContentBlock::Text { text: text.clone() }])
            }
        }
        serde_json::Value::Array(blocks) => {
            let mut result = vec![];
            for block in blocks {
                result.extend(parse_content_block(block));
            }
            if result.is_empty() {
                None
            } else {
                Some(result)
            }
        }
        _ => None,
    }
}

/// 解析 Anthropic role 字符串
fn parse_anthropic_role(role_str: &str) -> CanonicalRole {
    match role_str {
        "user" => CanonicalRole::User,
        "assistant" => CanonicalRole::Assistant,
        _ => CanonicalRole::User,
    }
}

/// 解析 Anthropic tools 为 CanonicalTool
fn parse_tools(tools: &serde_json::Value) -> Option<Vec<CanonicalTool>> {
    let tools_arr = tools.as_array()?;
    let mut result = vec![];

    for tool in tools_arr {
        let name = tool
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let description = tool
            .get("description")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string());
        let input_schema = tool.get("input_schema").cloned().unwrap_or(json!({}));

        result.push(CanonicalTool {
            name,
            description,
            input_schema,
        });
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

pub fn to_canonical_request(body: &serde_json::Value) -> Result<CanonicalRequest> {
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'model' in request body"))?;

    // Parse system field
    let system = body.get("system").and_then(parse_system);

    // Parse messages
    let empty_vec: Vec<serde_json::Value> = vec![];
    let messages_vals: &[serde_json::Value] = body
        .get("messages")
        .and_then(|m| m.as_array())
        .unwrap_or(&empty_vec);
    let mut messages = vec![];

    for msg in messages_vals {
        let role_str = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let role = parse_anthropic_role(role_str);

        let content = msg
            .get("content")
            .map(parse_message_content)
            .unwrap_or_default();

        messages.push(CanonicalMessage { role, content });
    }

    // Parse parameters
    let max_tokens = body.get("max_tokens").and_then(|m| m.as_u64());
    let temperature = body.get("temperature").and_then(|t| t.as_f64());
    let top_p = body.get("top_p").and_then(|t| t.as_f64());
    let stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    // stop_sequences is an array in Anthropic
    let stop_sequences: Option<Vec<String>> = body
        .get("stop_sequences")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .and_then(|v: Vec<String>| if v.is_empty() { None } else { Some(v) });

    let tools = body.get("tools").and_then(parse_tools);
    let tool_choice = body.get("tool_choice").cloned();

    Ok(CanonicalRequest {
        model: model.to_string(),
        messages,
        system,
        temperature,
        top_p,
        max_tokens,
        stop_sequences,
        stream,
        tools,
        tool_choice,
        unmapped: vec![],
    })
}

/// 将 CanonicalContentBlock 转为 Anthropic content block JSON
fn content_block_to_anthropic(block: &CanonicalContentBlock) -> serde_json::Value {
    match block {
        CanonicalContentBlock::Text { text } => {
            json!({"type": "text", "text": text})
        }
        CanonicalContentBlock::Reasoning { text } => {
            json!({"type": "thinking", "thinking": text})
        }
        CanonicalContentBlock::Image { source } => match source {
            CanonicalImageSource::Base64 { media_type, data } => {
                json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": data
                    }
                })
            }
            CanonicalImageSource::Url { url } => {
                json!({
                    "type": "image",
                    "source": {
                        "type": "url",
                        "url": url
                    }
                })
            }
        },
        CanonicalContentBlock::ToolUse { id, name, input } => {
            json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input
            })
        }
        CanonicalContentBlock::ToolResult {
            tool_use_id,
            content,
        } => {
            let content_blocks: Vec<serde_json::Value> =
                content.iter().map(content_block_to_anthropic).collect();

            // If single text block, use string content for simplicity
            let content_val = if content_blocks.len() == 1
                && content_blocks[0].get("type").and_then(|t| t.as_str()) == Some("text")
            {
                json!(content_blocks[0]["text"])
            } else {
                json!(content_blocks)
            };

            json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content_val
            })
        }
    }
}

/// 将 CanonicalMessage 转为 Anthropic message JSON
fn message_to_anthropic(msg: &CanonicalMessage) -> serde_json::Value {
    let role = match &msg.role {
        CanonicalRole::System => "user", // should not appear at message level
        CanonicalRole::User => "user",
        CanonicalRole::Assistant => "assistant",
    };

    let content_blocks: Vec<serde_json::Value> =
        msg.content.iter().map(content_block_to_anthropic).collect();

    // If single text block, use string content
    let content_val = if content_blocks.len() == 1
        && content_blocks[0].get("type").and_then(|t| t.as_str()) == Some("text")
    {
        json!(content_blocks[0]["text"])
    } else {
        json!(content_blocks)
    };

    json!({
        "role": role,
        "content": content_val
    })
}

pub fn from_canonical_request(canonical: &CanonicalRequest) -> Result<serde_json::Value> {
    let messages: Vec<serde_json::Value> = canonical
        .messages
        .iter()
        .map(message_to_anthropic)
        .collect();

    // Build system field
    let system_val = match &canonical.system {
        Some(blocks) => {
            let text_blocks: Vec<&str> = blocks
                .iter()
                .filter_map(|b| {
                    if let CanonicalContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect();

            if text_blocks.len() == 1 {
                json!(text_blocks[0])
            } else if !text_blocks.is_empty() {
                json!(text_blocks.join("\n\n"))
            } else {
                // Non-text blocks in system
                let blocks_json: Vec<serde_json::Value> =
                    blocks.iter().map(content_block_to_anthropic).collect();
                json!(blocks_json)
            }
        }
        None => json!(null),
    };

    let mut body = json!({
        "model": canonical.model,
        "messages": messages,
    });

    if !system_val.is_null() {
        body["system"] = system_val;
    }
    body["max_tokens"] = json!(canonical.max_tokens.unwrap_or(4096));
    if let Some(temp) = canonical.temperature {
        body["temperature"] = json!(temp);
    }
    if let Some(tp) = canonical.top_p {
        body["top_p"] = json!(tp);
    }
    if let Some(seqs) = &canonical.stop_sequences {
        body["stop_sequences"] = json!(seqs);
    }
    if canonical.stream {
        body["stream"] = json!(true);
    }
    if let Some(tools) = &canonical.tools {
        let anthropic_tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                let mut tool = json!({
                    "name": t.name,
                    "input_schema": t.input_schema
                });
                if let Some(desc) = &t.description {
                    tool["description"] = json!(desc);
                }
                tool
            })
            .collect();
        body["tools"] = json!(anthropic_tools);
    }
    if let Some(tool_choice) = &canonical.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }

    Ok(body)
}

/// 解析 Anthropic stop_reason 字符串
fn parse_stop_reason(reason: &serde_json::Value) -> Option<CanonicalStopReason> {
    let reason_str = reason.as_str()?;
    match reason_str {
        "end_turn" => Some(CanonicalStopReason::EndTurn),
        "max_tokens" => Some(CanonicalStopReason::MaxTokens),
        "stop_sequence" => Some(CanonicalStopReason::StopSequence),
        "tool_use" => Some(CanonicalStopReason::ToolUse),
        _ => None,
    }
}

pub fn to_canonical_response(body: &serde_json::Value) -> Result<CanonicalResponse> {
    let id = body
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or("")
        .to_string();
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();

    // Extract content blocks from response
    let mut content = vec![];
    if let Some(content_arr) = body.get("content").and_then(|c| c.as_array()) {
        for block in content_arr {
            content.extend(parse_content_block(block));
        }
    }

    // stop_reason
    let stop_reason = body.get("stop_reason").and_then(parse_stop_reason);

    // usage
    let usage = body.get("usage").map(|u| CanonicalUsage {
        input_tokens: u.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
        output_tokens: u.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
        total_tokens: None,
    });

    Ok(CanonicalResponse {
        id,
        model,
        content,
        stop_reason,
        usage,
    })
}

/// 将 canonical stop reason 转为 Anthropic stop_reason 字符串
fn stop_reason_to_anthropic(reason: &CanonicalStopReason) -> &'static str {
    match reason {
        CanonicalStopReason::EndTurn => "end_turn",
        CanonicalStopReason::StopSequence => "stop_sequence",
        CanonicalStopReason::MaxTokens => "max_tokens",
        CanonicalStopReason::ToolUse => "tool_use",
    }
}

pub fn from_canonical_response(canonical: &CanonicalResponse) -> Result<serde_json::Value> {
    let content_blocks: Vec<serde_json::Value> = canonical
        .content
        .iter()
        .map(content_block_to_anthropic)
        .collect();

    let mut body = json!({
        "id": canonical.id,
        "model": canonical.model,
        "content": content_blocks,
        "type": "message",
    });

    if let Some(reason) = &canonical.stop_reason {
        body["stop_reason"] = json!(stop_reason_to_anthropic(reason));
    }

    if let Some(usage) = &canonical.usage {
        body["usage"] = json!({
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
        });
    }

    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_text_request() {
        let body = json!({
            "model": "claude-3",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert_eq!(canonical.model, "claude-3");
        assert_eq!(canonical.messages.len(), 1);
        assert_eq!(canonical.messages[0].role, CanonicalRole::User);
        assert!(matches!(&canonical.messages[0].content[0],
            CanonicalContentBlock::Text { text } if text == "Hello"));
    }

    #[test]
    fn test_system_as_string() {
        let body = json!({
            "model": "claude-3",
            "system": "Be helpful",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert!(canonical.system.is_some());
        let sys = canonical.system.unwrap();
        assert_eq!(sys.len(), 1);
        assert!(matches!(&sys[0], CanonicalContentBlock::Text { text } if text == "Be helpful"));
    }

    #[test]
    fn test_roundtrip_simple() {
        let body = json!({
            "model": "claude-3",
            "system": "You are helpful",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 100,
            "temperature": 0.5
        });
        let canonical = to_canonical_request(&body).unwrap();
        let back = from_canonical_request(&canonical).unwrap();
        assert_eq!(back["model"], "claude-3");
        assert_eq!(back["system"], "You are helpful");
        assert_eq!(back["max_tokens"], 100);
        assert_eq!(back["temperature"], 0.5);
        assert!(back.get("anthropic_version").is_none());
    }

    #[test]
    fn test_from_canonical_request_omits_null_system_and_defaults_max_tokens() {
        let canonical = CanonicalRequest {
            model: "claude-3".to_string(),
            messages: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![CanonicalContentBlock::Text {
                    text: "Hello".to_string(),
                }],
            }],
            system: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            stream: false,
            tools: None,
            tool_choice: None,
            unmapped: vec![],
        };

        let body = from_canonical_request(&canonical).unwrap();
        assert!(body.get("system").is_none());
        assert_eq!(body["max_tokens"], 4096);
        assert!(body.get("anthropic_version").is_none());
    }

    #[test]
    fn test_tool_use_request() {
        let body = json!({
            "model": "claude-3",
            "messages": [
                {"role": "user", "content": "Search for something"},
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "tool_1",
                            "name": "search",
                            "input": {"query": "test"}
                        }
                    ]
                }
            ]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert_eq!(canonical.messages.len(), 2);
        assert!(matches!(
            &canonical.messages[1].content[0],
            CanonicalContentBlock::ToolUse { .. }
        ));
        let tool_use = &canonical.messages[1].content[0];
        if let CanonicalContentBlock::ToolUse { id, name, .. } = tool_use {
            assert_eq!(id, "tool_1");
            assert_eq!(name, "search");
        }
    }

    #[test]
    fn test_tool_result_parsing() {
        let body = json!({
            "model": "claude-3",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "tool_1",
                            "content": "Here are the results"
                        }
                    ]
                }
            ]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert!(matches!(
            &canonical.messages[0].content[0],
            CanonicalContentBlock::ToolResult { .. }
        ));
        if let CanonicalContentBlock::ToolResult {
            tool_use_id,
            content,
        } = &canonical.messages[0].content[0]
        {
            assert_eq!(tool_use_id, "tool_1");
            assert_eq!(content.len(), 1);
        }
    }

    #[test]
    fn test_image_base64() {
        let body = json!({
            "model": "claude-3",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What is this?"},
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": "iVBORw0KGgo="
                        }
                    }
                ]
            }]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert_eq!(canonical.messages[0].content.len(), 2);
        assert!(matches!(
            &canonical.messages[0].content[1],
            CanonicalContentBlock::Image { .. }
        ));
    }

    #[test]
    fn test_tools_parsing() {
        let body = json!({
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Hi"}],
            "tools": [{
                "name": "search",
                "description": "Search the web",
                "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}}
            }]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert!(canonical.tools.is_some());
        let tools = canonical.tools.unwrap();
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].description, Some("Search the web".to_string()));
    }

    #[test]
    fn test_response_with_tool_use() {
        let body = json!({
            "id": "msg_123",
            "model": "claude-3",
            "content": [
                {
                    "type": "tool_use",
                    "id": "tool_1",
                    "name": "search",
                    "input": {"query": "test"}
                }
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 50,
                "output_tokens": 30
            }
        });
        let canonical = to_canonical_response(&body).unwrap();
        assert_eq!(canonical.id, "msg_123");
        assert_eq!(canonical.content.len(), 1);
        assert!(matches!(
            &canonical.content[0],
            CanonicalContentBlock::ToolUse { .. }
        ));
        assert_eq!(canonical.stop_reason, Some(CanonicalStopReason::ToolUse));
    }

    #[test]
    fn test_response_roundtrip() {
        let body = json!({
            "id": "msg_123",
            "model": "claude-3",
            "content": [
                {"type": "text", "text": "Hello there"}
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20
            }
        });
        let canonical = to_canonical_response(&body).unwrap();
        let back = from_canonical_response(&canonical).unwrap();
        assert_eq!(back["id"], "msg_123");
        assert_eq!(back["content"][0]["text"], "Hello there");
        assert_eq!(back["stop_reason"], "end_turn");
        assert_eq!(back["type"], "message");
    }

    #[test]
    fn test_response_preserves_thinking_block() {
        let body = json!({
            "id": "msg_thinking",
            "model": "claude-3",
            "content": [
                {"type": "thinking", "thinking": "hidden thinking"},
                {"type": "text", "text": "OK"}
            ],
            "stop_reason": "end_turn"
        });
        let canonical = to_canonical_response(&body).unwrap();
        assert_eq!(canonical.content.len(), 2);
        assert!(matches!(
            &canonical.content[0],
            CanonicalContentBlock::Reasoning { text } if text == "hidden thinking"
        ));
        assert!(matches!(
            &canonical.content[1],
            CanonicalContentBlock::Text { text } if text == "OK"
        ));
    }

    #[test]
    fn test_tool_result_with_array_content() {
        let body = json!({
            "model": "claude-3",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tool_1",
                    "content": [
                        {"type": "text", "text": "Result 1"},
                        {"type": "text", "text": "Result 2"}
                    ]
                }]
            }]
        });
        let canonical = to_canonical_request(&body).unwrap();
        if let CanonicalContentBlock::ToolResult {
            tool_use_id,
            content,
        } = &canonical.messages[0].content[0]
        {
            assert_eq!(tool_use_id, "tool_1");
            assert_eq!(content.len(), 2);
        } else {
            panic!("expected ToolResult");
        }
    }

    #[test]
    fn test_system_as_content_blocks() {
        let body = json!({
            "model": "claude-3",
            "system": [
                {"type": "text", "text": "Rule 1"},
                {"type": "text", "text": "Rule 2"}
            ],
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let canonical = to_canonical_request(&body).unwrap();
        let sys = canonical.system.unwrap();
        assert_eq!(sys.len(), 2);
    }

    #[test]
    fn test_image_url_source() {
        let body = json!({
            "model": "claude-3",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image",
                    "source": {
                        "type": "url",
                        "url": "https://example.com/img.png"
                    }
                }]
            }]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert!(matches!(&canonical.messages[0].content[0],
            CanonicalContentBlock::Image { source } if matches!(source, CanonicalImageSource::Url { .. })));
    }
}
