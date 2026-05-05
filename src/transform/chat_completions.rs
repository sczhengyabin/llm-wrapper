//! OpenAI Chat Completions <-> Canonical 转换
//! Phase 3 实现

use super::canonical::*;
use anyhow::Result;
use serde_json::json;

/// 解析 OpenAI message 的 content 为 CanonicalContentBlock 数组
fn parse_content_blocks(content: &serde_json::Value) -> Vec<CanonicalContentBlock> {
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

/// 解析单个 content block
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
        "image_url" => {
            let image_url_val = block.get("image_url");
            match image_url_val {
                Some(img) => {
                    let url = img
                        .get("url")
                        .and_then(|u| u.as_str())
                        .unwrap_or("")
                        .to_string();
                    parse_image_url(&url)
                }
                None => vec![],
            }
        }
        _ => vec![],
    }
}

/// 解析 image_url 字符串，区分 data URL 和普通 URL
fn parse_image_url(url: &str) -> Vec<CanonicalContentBlock> {
    if let Some((media_type, data)) = parse_data_url(url) {
        vec![CanonicalContentBlock::Image {
            source: CanonicalImageSource::Base64 { media_type, data },
        }]
    } else {
        vec![CanonicalContentBlock::Image {
            source: CanonicalImageSource::Url {
                url: url.to_string(),
            },
        }]
    }
}

/// 解析 data:image/png;base64,ABCDEF 格式
fn parse_data_url(url: &str) -> Option<(String, String)> {
    if !url.starts_with("data:") {
        return None;
    }
    let comma_pos = url.find(',')?;
    let header = &url[5..comma_pos]; // skip "data:"
    let data = url[comma_pos + 1..].to_string();

    // header like "image/png;base64" or "image/png"
    let media_type = if let Some(semi) = header.find(';') {
        header[..semi].to_string()
    } else {
        header.to_string()
    };

    Some((media_type, data))
}

/// 解析 OpenAI message 为 CanonicalMessage
fn parse_message(msg: &serde_json::Value) -> Option<CanonicalMessage> {
    let role_str = msg.get("role").and_then(|r| r.as_str())?;

    // Handle tool role messages first - they become User messages with ToolResult content
    if role_str == "tool" {
        let content = msg
            .get("content")
            .map(parse_content_blocks)
            .unwrap_or_default();
        let tool_call_id = msg
            .get("tool_call_id")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        return Some(CanonicalMessage {
            role: CanonicalRole::User, // tool results go to User
            content: vec![CanonicalContentBlock::ToolResult {
                tool_use_id: tool_call_id,
                content,
            }],
        });
    }

    let role = match role_str {
        "system" => CanonicalRole::System,
        "developer" => CanonicalRole::System,
        "user" => CanonicalRole::User,
        "assistant" => CanonicalRole::Assistant,
        _ => return None,
    };

    let mut content = msg
        .get("content")
        .map(parse_content_blocks)
        .unwrap_or_default();

    // Handle assistant tool_calls in requests
    if role == CanonicalRole::Assistant {
        if let Some(tool_calls) = msg.get("tool_calls").and_then(|tc| tc.as_array()) {
            for tc in tool_calls {
                let id = tc
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let func = tc.get("function");
                if let Some(func_val) = func {
                    let name = func_val
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments_raw = func_val.get("arguments").unwrap_or(&json!({})).clone();
                    let input = serde_json::from_str(arguments_raw.to_string().as_str())
                        .unwrap_or_else(|_| arguments_raw);

                    content.push(CanonicalContentBlock::ToolUse { id, name, input });
                }
            }
        }
    }

    Some(CanonicalMessage { role, content })
}

/// 解析 OpenAI tools 为 CanonicalTool
fn parse_tools(tools: &serde_json::Value) -> Option<Vec<CanonicalTool>> {
    let tools_arr = tools.as_array()?;
    let mut result = vec![];

    for tool in tools_arr {
        let tool_type = tool.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if tool_type == "function" {
            let func = tool.get("function")?;
            let name = func
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let description = func
                .get("description")
                .and_then(|d| d.as_str())
                .map(|s| s.to_string());
            let parameters = func.get("parameters").cloned().unwrap_or(json!({}));

            result.push(CanonicalTool {
                name,
                description,
                input_schema: parameters,
            });
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// 解析 stop 参数，支持 string 或 array
fn parse_stop(stop_val: &serde_json::Value) -> Option<Vec<String>> {
    match stop_val {
        serde_json::Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(vec![s.clone()])
            }
        }
        serde_json::Value::Array(arr) => {
            let seqs: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            if seqs.is_empty() {
                None
            } else {
                Some(seqs)
            }
        }
        _ => None,
    }
}

fn parse_reasoning_effort(body: &serde_json::Value) -> Option<String> {
    if let Some(effort) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        return Some(effort.to_string());
    }
    body.get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn to_canonical_request(body: &serde_json::Value) -> Result<CanonicalRequest> {
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'model' in request body"))?;

    // Parse messages, separating system messages
    let empty_vec: Vec<serde_json::Value> = vec![];
    let messages_vals: &[serde_json::Value] = body
        .get("messages")
        .and_then(|m| m.as_array())
        .unwrap_or(&empty_vec);
    let mut system_blocks = vec![];
    let mut messages = vec![];

    for msg in messages_vals {
        let role_str = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role_str == "system" || role_str == "developer" {
            // Extract content as system blocks
            if let Some(content) = msg.get("content") {
                system_blocks.extend(parse_content_blocks(content));
            }
        } else {
            if let Some(canonical_msg) = parse_message(msg) {
                messages.push(canonical_msg);
            }
        }
    }

    let system = if system_blocks.is_empty() {
        None
    } else {
        Some(system_blocks)
    };

    // Parse parameters
    let temperature = body.get("temperature").and_then(|t| t.as_f64());
    let top_p = body.get("top_p").and_then(|t| t.as_f64());
    let max_tokens = body
        .get("max_tokens")
        .and_then(|m| m.as_u64())
        .or_else(|| body.get("max_completion_tokens").and_then(|m| m.as_u64()));
    let stop_sequences = body.get("stop").and_then(|s| parse_stop(s));
    let stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);
    let tools = body.get("tools").and_then(parse_tools);
    let tool_choice = body.get("tool_choice").cloned();
    let reasoning_effort = parse_reasoning_effort(body);

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
        reasoning_effort,
        unmapped: vec![],
    })
}

/// 将 CanonicalContentBlock 转为 OpenAI content block JSON
fn content_block_to_openai(block: &CanonicalContentBlock) -> serde_json::Value {
    match block {
        CanonicalContentBlock::Text { text } => {
            json!({"type": "text", "text": text})
        }
        CanonicalContentBlock::Reasoning { text } => {
            json!({"type": "text", "text": format!("<think>\n{}\n</think>", text)})
        }
        CanonicalContentBlock::Image { source } => match source {
            CanonicalImageSource::Base64 { media_type, data } => {
                json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", media_type, data)
                    }
                })
            }
            CanonicalImageSource::Url { url } => {
                json!({
                    "type": "image_url",
                    "image_url": {
                        "url": url
                    }
                })
            }
        },
        CanonicalContentBlock::ToolUse { id, name, input } => {
            json!({
                "type": "tool_call",
                "id": id,
                "function": {
                    "name": name,
                    "arguments": input.to_string()
                }
            })
        }
        CanonicalContentBlock::ToolResult {
            tool_use_id: _,
            content: _,
        } => {
            // ToolResult should not appear in OpenAI content blocks directly
            // This is handled at the message level
            json!({"type": "text", "text": ""})
        }
    }
}

/// 将 CanonicalMessage 转为 OpenAI message JSON
fn message_to_openai(msg: &CanonicalMessage) -> Option<serde_json::Value> {
    let role = match &msg.role {
        CanonicalRole::System => return None, // system is handled separately
        CanonicalRole::User => "user",
        CanonicalRole::Assistant => "assistant",
    };

    // Check if this message contains ToolResult blocks
    let has_tool_results = msg
        .content
        .iter()
        .any(|b| matches!(b, CanonicalContentBlock::ToolResult { .. }));

    if has_tool_results {
        // Extract tool results and convert them
        let mut result = vec![];
        for block in &msg.content {
            if let CanonicalContentBlock::ToolResult {
                tool_use_id,
                content,
            } = block
            {
                let content_blocks: Vec<serde_json::Value> =
                    content.iter().map(content_block_to_openai).collect();

                let content_val = if content_blocks.len() == 1 {
                    content_blocks[0]["text"].clone()
                } else {
                    json!(content_blocks)
                };

                result.push(json!({
                    "role": "tool",
                    "content": content_val,
                    "tool_call_id": tool_use_id
                }));
            } else {
                // Non-tool-result content in a user message with tool results
                // wrap as regular user message content
                let cb = content_block_to_openai(block);
                result.push(json!({
                    "role": "user",
                    "content": cb
                }));
            }
        }
        // Return as array if multiple, or single value
        if result.len() == 1 {
            return Some(result[0].clone());
        }
        return Some(json!(result));
    }

    // Handle ToolUse blocks for assistant messages
    if role == "assistant" {
        let tool_uses: Vec<_> = msg
            .content
            .iter()
            .filter_map(|b| {
                if let CanonicalContentBlock::ToolUse { id, name, input } = b {
                    Some(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": input.to_string()
                        }
                    }))
                } else {
                    None
                }
            })
            .collect();

        let text_blocks: Vec<_> = msg
            .content
            .iter()
            .filter(|b| !matches!(b, CanonicalContentBlock::ToolUse { .. }))
            .map(content_block_to_openai)
            .collect();

        let mut msg_json = json!({"role": "assistant"});

        if !tool_uses.is_empty() {
            msg_json["tool_calls"] = json!(tool_uses);
        }

        if text_blocks.len() == 1 {
            let block = &text_blocks[0];
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                msg_json["content"] = block["text"].clone();
            } else {
                msg_json["content"] = json!(text_blocks);
            }
        } else if text_blocks.len() > 1 {
            msg_json["content"] = json!(text_blocks);
        }

        return Some(msg_json);
    }

    // User message
    let content_blocks: Vec<serde_json::Value> =
        msg.content.iter().map(content_block_to_openai).collect();

    let mut msg_json = json!({"role": role});

    if content_blocks.is_empty() {
        msg_json["content"] = json!("");
    } else if content_blocks.len() == 1 {
        let block = &content_blocks[0];
        // If single text block, use string content
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            msg_json["content"] = block["text"].clone();
        } else {
            msg_json["content"] = json!(content_blocks);
        }
    } else {
        msg_json["content"] = json!(content_blocks);
    }

    Some(msg_json)
}

pub fn from_canonical_request(canonical: &CanonicalRequest) -> Result<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = vec![];

    // Prepend system message if present
    if let Some(system_blocks) = &canonical.system {
        let system_text: Vec<&str> = system_blocks
            .iter()
            .filter_map(|b| {
                if let CanonicalContentBlock::Text { text } = b {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();

        if !system_text.is_empty() {
            messages.push(json!({
                "role": "system",
                "content": system_text.join("\n\n")
            }));
        }
    }

    // Convert canonical messages
    for msg in &canonical.messages {
        if let Some(openai_msg) = message_to_openai(msg) {
            if let Some(arr) = openai_msg.as_array() {
                messages.extend(arr.clone());
            } else {
                messages.push(openai_msg);
            }
        }
    }

    let mut body = json!({
        "model": canonical.model,
        "messages": messages,
    });

    if let Some(temp) = canonical.temperature {
        body["temperature"] = json!(temp);
    }
    if let Some(tp) = canonical.top_p {
        body["top_p"] = json!(tp);
    }
    if let Some(mt) = canonical.max_tokens {
        body["max_tokens"] = json!(mt);
    }
    if let Some(seqs) = &canonical.stop_sequences {
        if seqs.len() == 1 {
            body["stop"] = json!(seqs[0]);
        } else {
            body["stop"] = json!(seqs);
        }
    }
    if canonical.stream {
        body["stream"] = json!(true);
    }
    if let Some(tools) = &canonical.tools {
        let openai_tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema
                    }
                })
            })
            .collect();
        body["tools"] = json!(openai_tools);
    }
    if let Some(tool_choice) = &canonical.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }
    if let Some(effort) = &canonical.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }

    Ok(body)
}

/// 解析 finish_reason 字符串
fn parse_finish_reason(reason: &serde_json::Value) -> Option<CanonicalStopReason> {
    let reason_str = reason.as_str()?;
    match reason_str {
        "stop" => Some(CanonicalStopReason::EndTurn),
        "length" => Some(CanonicalStopReason::MaxTokens),
        "tool_calls" => Some(CanonicalStopReason::ToolUse),
        "content_filter" => Some(CanonicalStopReason::EndTurn),
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

    // Extract content from choices[0].message
    let mut content = vec![];
    if let Some(choices) = body.get("choices").and_then(|c| c.as_array()) {
        if let Some(first_choice) = choices.first() {
            if let Some(message) = first_choice.get("message") {
                // Check for tool_calls first
                if let Some(tool_calls) = message.get("tool_calls").and_then(|tc| tc.as_array()) {
                    for tc in tool_calls {
                        let tc_id = tc
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let func = tc.get("function");
                        if let Some(func_val) = func {
                            let name = func_val
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments_raw =
                                func_val.get("arguments").unwrap_or(&json!({})).clone();
                            let input = serde_json::from_str(arguments_raw.to_string().as_str())
                                .unwrap_or_else(|_| arguments_raw.clone());

                            content.push(CanonicalContentBlock::ToolUse {
                                id: tc_id,
                                name,
                                input,
                            });
                        }
                    }
                }

                for key in ["reasoning_content", "reasoning", "thinking"] {
                    if let Some(text) = message.get(key).and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            content.push(CanonicalContentBlock::Reasoning {
                                text: text.to_string(),
                            });
                            break;
                        }
                    }
                }

                // Check for text content
                if let Some(msg_content) = message.get("content") {
                    if let Some(text) = msg_content.as_str() {
                        if !text.is_empty() {
                            content.push(CanonicalContentBlock::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                }

                // finish_reason
                let stop_reason = first_choice
                    .get("finish_reason")
                    .and_then(parse_finish_reason);

                // usage
                let usage = body.get("usage").map(|u| CanonicalUsage {
                    input_tokens: u.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
                    output_tokens: u
                        .get("completion_tokens")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0),
                    total_tokens: u.get("total_tokens").and_then(|t| t.as_u64()),
                });

                return Ok(CanonicalResponse {
                    id,
                    model,
                    content,
                    stop_reason,
                    usage,
                });
            }
        }
    }

    Ok(CanonicalResponse {
        id,
        model,
        content,
        stop_reason: None,
        usage: None,
    })
}

/// 将 canonical stop reason 转为 OpenAI finish_reason 字符串
fn stop_reason_to_openai(reason: &CanonicalStopReason) -> &'static str {
    match reason {
        CanonicalStopReason::EndTurn => "stop",
        CanonicalStopReason::StopSequence => "stop",
        CanonicalStopReason::MaxTokens => "length",
        CanonicalStopReason::ToolUse => "tool_calls",
    }
}

pub fn from_canonical_response(canonical: &CanonicalResponse) -> Result<serde_json::Value> {
    // Determine if we have tool calls or text
    let tool_uses: Vec<_> = canonical
        .content
        .iter()
        .filter_map(|b| {
            if let CanonicalContentBlock::ToolUse { id, name, input } = b {
                Some(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": input.to_string()
                    }
                }))
            } else {
                None
            }
        })
        .collect();

    let visible_blocks: Vec<String> = canonical
        .content
        .iter()
        .filter(|b| !matches!(b, CanonicalContentBlock::ToolUse { .. }))
        .filter_map(|b| match b {
            CanonicalContentBlock::Text { text } => Some(text.clone()),
            CanonicalContentBlock::Reasoning { text } => {
                Some(format!("<think>\n{}\n</think>", text))
            }
            _ => None,
        })
        .collect();

    let mut message = json!({"role": "assistant"});
    let mut has_content = false;

    if !visible_blocks.is_empty() {
        message["content"] = json!(visible_blocks.join("\n\n"));
        has_content = true;
    }

    if !tool_uses.is_empty() {
        message["tool_calls"] = json!(tool_uses);
        has_content = true;
    }

    if !has_content {
        message["content"] = if tool_uses.is_empty() {
            json!("")
        } else {
            json!(null)
        };
    }

    let mut body = json!({
        "id": canonical.id,
        "model": canonical.model,
        "choices": [{
            "index": 0,
            "message": message,
        }],
    });

    if let Some(reason) = &canonical.stop_reason {
        body["choices"][0]["finish_reason"] = json!(stop_reason_to_openai(reason));
    }

    if let Some(usage) = &canonical.usage {
        body["usage"] = json!({
            "prompt_tokens": usage.input_tokens,
            "completion_tokens": usage.output_tokens,
            "total_tokens": usage.total_tokens,
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
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert_eq!(canonical.model, "gpt-4");
        assert_eq!(canonical.messages.len(), 1);
        assert_eq!(canonical.messages[0].role, CanonicalRole::User);
        assert!(matches!(&canonical.messages[0].content[0],
            CanonicalContentBlock::Text { text } if text == "Hello"));
    }

    #[test]
    fn test_system_message_extraction() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "Be helpful"},
                {"role": "user", "content": "Hi"}
            ]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert!(canonical.system.is_some());
        assert_eq!(canonical.messages.len(), 1);
        assert_eq!(canonical.messages[0].role, CanonicalRole::User);
    }

    #[test]
    fn test_developer_message_extraction() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "developer", "content": "Use concise style"},
                {"role": "user", "content": "Hi"}
            ]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert!(canonical.system.is_some());
        let sys = canonical.system.unwrap();
        assert_eq!(sys.len(), 1);
        assert!(
            matches!(&sys[0], CanonicalContentBlock::Text { text } if text == "Use concise style")
        );
        assert_eq!(canonical.messages.len(), 1);
        assert_eq!(canonical.messages[0].role, CanonicalRole::User);
    }

    #[test]
    fn test_roundtrip_simple() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "temperature": 0.7,
            "max_tokens": 100
        });
        let canonical = to_canonical_request(&body).unwrap();
        let back = from_canonical_request(&canonical).unwrap();
        assert_eq!(back["model"], "gpt-4");
        assert_eq!(back["temperature"], 0.7);
        assert_eq!(back["max_tokens"], 100);
        assert_eq!(back["messages"][0]["role"], "user");
        assert_eq!(back["messages"][0]["content"], "Hello");
    }

    #[test]
    fn test_to_canonical_request_parses_reasoning_effort() {
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "reasoning": {"effort": "high"}
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert_eq!(canonical.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn test_from_canonical_request_emits_reasoning_effort() {
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "reasoning_effort": "low"
        });
        let canonical = to_canonical_request(&body).unwrap();
        let back = from_canonical_request(&canonical).unwrap();
        assert_eq!(back["reasoning_effort"], "low");
    }

    #[test]
    fn test_data_url_parsing() {
        let result = parse_data_url("data:image/png;base64,ABCDEF");
        assert!(result.is_some());
        let (media_type, data) = result.unwrap();
        assert_eq!(media_type, "image/png");
        assert_eq!(data, "ABCDEF");
    }

    #[test]
    fn test_stop_as_string() {
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "stop": "\n"
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert_eq!(canonical.stop_sequences, Some(vec!["\n".to_string()]));
    }

    #[test]
    fn test_stop_as_array() {
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "stop": ["\n", "END"]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert_eq!(
            canonical.stop_sequences,
            Some(vec!["\n".to_string(), "END".to_string()])
        );
    }

    #[test]
    fn test_max_completion_tokens_maps_to_canonical_max_tokens() {
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_completion_tokens": 123
        });

        let canonical = to_canonical_request(&body).unwrap();
        assert_eq!(canonical.max_tokens, Some(123));
    }

    #[test]
    fn test_tools_parsing() {
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "search",
                    "description": "Search the web",
                    "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
                }
            }]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert!(canonical.tools.is_some());
        let tools = canonical.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "search");
    }

    #[test]
    fn test_response_with_tool_calls() {
        let body = json!({
            "id": "chatcmpl-123",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "search",
                            "arguments": "{\"query\":\"test\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let canonical = to_canonical_response(&body).unwrap();
        assert_eq!(canonical.id, "chatcmpl-123");
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
            "id": "chatcmpl-123",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello there"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        });
        let canonical = to_canonical_response(&body).unwrap();
        let back = from_canonical_response(&canonical).unwrap();
        assert_eq!(back["id"], "chatcmpl-123");
        assert_eq!(back["choices"][0]["message"]["content"], "Hello there");
        assert_eq!(back["choices"][0]["finish_reason"], "stop");
        assert_eq!(back["usage"]["prompt_tokens"], 10);
    }

    #[test]
    fn test_response_reasoning_field_is_preserved_for_openwebui() {
        let body = json!({
            "id": "chatcmpl-reasoning",
            "model": "qwen",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "OK",
                    "reasoning": "hidden thinking"
                },
                "finish_reason": "stop"
            }]
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

        let chat = from_canonical_response(&canonical).unwrap();
        assert_eq!(
            chat["choices"][0]["message"]["content"],
            "<think>\nhidden thinking\n</think>\n\nOK"
        );
    }

    #[test]
    fn test_image_content_blocks() {
        let body = json!({
            "model": "gpt-4-vision",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What is this?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,ABC"}}
                ]
            }]
        });
        let canonical = to_canonical_request(&body).unwrap();
        assert_eq!(canonical.messages[0].content.len(), 2);
        assert!(matches!(
            &canonical.messages[0].content[0],
            CanonicalContentBlock::Text { .. }
        ));
        assert!(matches!(
            &canonical.messages[0].content[1],
            CanonicalContentBlock::Image { .. }
        ));
    }

    #[test]
    fn test_tool_message() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "search", "arguments": "{\"q\":\"test\"}"}
                }]},
                {"role": "tool", "content": "Results found", "tool_call_id": "call_1"}
            ]
        });
        let canonical = to_canonical_request(&body).unwrap();
        // Assistant message with tool use
        assert_eq!(canonical.messages[0].role, CanonicalRole::Assistant);
        assert!(matches!(
            &canonical.messages[0].content[0],
            CanonicalContentBlock::ToolUse { .. }
        ));
        // Tool result as user message with ToolResult block
        assert_eq!(canonical.messages[1].role, CanonicalRole::User);
        assert!(matches!(
            &canonical.messages[1].content[0],
            CanonicalContentBlock::ToolResult { .. }
        ));
    }
}
