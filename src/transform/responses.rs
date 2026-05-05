//! OpenAI Responses API ↔ Canonical 转换
//! Phase 4 实现

use super::canonical::*;
use anyhow::Result;
use serde_json::json;

fn normalize_tool_choice_for_responses(
    tool_choice: &serde_json::Value,
) -> Option<serde_json::Value> {
    match tool_choice {
        serde_json::Value::String(s) => match s.as_str() {
            "auto" => Some(json!("auto")),
            "none" => Some(json!("none")),
            "required" => Some(json!("required")),
            _ => None,
        },
        serde_json::Value::Object(obj) => {
            let tc_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or_default();
            match tc_type {
                "auto" => Some(json!("auto")),
                "none" => Some(json!("none")),
                "any" => Some(json!("required")),
                "tool" => obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|name| json!({"type":"function","name":name})),
                "function" => {
                    if obj.get("name").and_then(|v| v.as_str()).is_some() {
                        Some(tool_choice.clone())
                    } else {
                        obj.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .map(|name| json!({"type":"function","name":name}))
                    }
                }
                _ => None,
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

/// 解析 Responses API 的 input content block 为 CanonicalContentBlock
#[allow(unused_variables)]
fn parse_responses_content_block(block: &serde_json::Value) -> Option<CanonicalContentBlock> {
    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match block_type {
        "text" | "input_text" | "output_text" => {
            let text = block
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                None
            } else {
                Some(CanonicalContentBlock::Text { text })
            }
        }
        "reasoning_text" | "summary_text" => {
            let text = block
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                None
            } else {
                Some(CanonicalContentBlock::Reasoning { text })
            }
        }
        "image_url" => {
            let image_url = block
                .get("image_url")
                .and_then(|i| i.get("url"))?
                .as_str()?;
            Some(CanonicalContentBlock::Image {
                source: CanonicalImageSource::Url {
                    url: image_url.to_string(),
                },
            })
        }
        "input_image" => {
            // OpenAI style: {"type": "input_image", "image_url": "https://..." or "data": "...", "format": "..."}
            if let Some(image_url) = block.get("image_url").and_then(|i| i.as_str()) {
                return Some(CanonicalContentBlock::Image {
                    source: CanonicalImageSource::Url {
                        url: image_url.to_string(),
                    },
                });
            }
            if let Some(data) = block.get("data").and_then(|d| d.as_str()) {
                let media_type = block
                    .get("format")
                    .and_then(|f| f.as_str())
                    .unwrap_or("png");
                return Some(CanonicalContentBlock::Image {
                    source: CanonicalImageSource::Base64 {
                        media_type: format!("image/{}", media_type),
                        data: data.to_string(),
                    },
                });
            }
            None
        }
        _ => None,
    }
}

/// 解析 Responses API 的一个 input item 为 CanonicalMessage 列表
#[allow(unused_variables)]
fn parse_responses_input_item(item: &serde_json::Value) -> Vec<CanonicalMessage> {
    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match item_type {
        "message" => {
            let role_str = item.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let role = match role_str {
                "system" => CanonicalRole::System,
                "assistant" => CanonicalRole::Assistant,
                _ => CanonicalRole::User,
            };

            let content = item.get("content");
            let blocks = if let Some(content) = content {
                if let Some(text) = content.as_str() {
                    if text.is_empty() {
                        vec![]
                    } else {
                        vec![CanonicalContentBlock::Text {
                            text: text.to_string(),
                        }]
                    }
                } else if let Some(arr) = content.as_array() {
                    arr.iter()
                        .filter_map(|b| parse_responses_content_block(b))
                        .collect()
                } else {
                    vec![]
                }
            } else {
                vec![]
            };

            vec![CanonicalMessage {
                role,
                content: blocks,
            }]
        }
        "function_call_output" => {
            // Function call result from user providing tool output
            let call_id = item
                .get("call_id")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let output = item.get("output").and_then(|o| o.as_str()).unwrap_or("");

            if call_id.is_empty() {
                return vec![];
            }

            let content_blocks = if output.is_empty() {
                vec![]
            } else {
                vec![CanonicalContentBlock::Text {
                    text: output.to_string(),
                }]
            };

            vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![CanonicalContentBlock::ToolResult {
                    tool_use_id: call_id,
                    content: content_blocks,
                }],
            }]
        }
        "file" => {
            // Skip file items in input, they don't map cleanly
            vec![]
        }
        _ => vec![],
    }
}

pub fn to_canonical_request(body: &serde_json::Value) -> Result<CanonicalRequest> {
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    if model.is_empty() {
        anyhow::bail!("Responses request must include 'model'");
    }

    // Parse input into messages
    let mut messages: Vec<CanonicalMessage> = vec![];
    let input = body.get("input");

    if let Some(input) = input {
        if let Some(text) = input.as_str() {
            // Single string input → single user message
            if !text.is_empty() {
                messages.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![CanonicalContentBlock::Text {
                        text: text.to_string(),
                    }],
                });
            }
        } else if let Some(arr) = input.as_array() {
            // Array of input items
            for item in arr {
                let parsed = parse_responses_input_item(item);
                messages.extend(parsed);
            }
        }
    }

    // Parse instructions → system
    let system = body
        .get("instructions")
        .and_then(|s| s.as_str())
        .and_then(|text| {
            if text.is_empty() {
                None
            } else {
                Some(vec![CanonicalContentBlock::Text {
                    text: text.to_string(),
                }])
            }
        });

    // Parse tools
    let tools = body.get("tools").and_then(|t| t.as_array()).map(|arr| {
        let mut canonical_tools = vec![];

        for tool in arr {
            let tool_type = tool.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if tool_type == "function" {
                let name = tool
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let description = tool
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string());
                let input_schema = tool.get("parameters").cloned().unwrap_or_else(|| json!({}));

                canonical_tools.push(CanonicalTool {
                    name,
                    description,
                    input_schema,
                });
            }
            // Other tool types (web_search_preview, file_search, etc.) are not mapped to canonical
        }

        canonical_tools
    });

    Ok(CanonicalRequest {
        model,
        messages,
        system,
        temperature: body.get("temperature").and_then(|t| t.as_f64()),
        top_p: body.get("top_p").and_then(|t| t.as_f64()),
        max_tokens: body.get("max_output_tokens").and_then(|m| m.as_u64()),
        stop_sequences: body
            .get("stop_sequences")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            }),
        stream: body
            .get("stream")
            .and_then(|s| s.as_bool())
            .unwrap_or(false),
        tool_choice: body.get("tool_choice").cloned(),
        reasoning_effort: parse_reasoning_effort(body),
        tools: if tools.as_ref().map_or(true, |t| t.is_empty()) {
            None
        } else {
            tools
        },
        unmapped: vec![],
    })
}

pub fn from_canonical_request(canonical: &CanonicalRequest) -> Result<serde_json::Value> {
    // Build instructions from system
    let instructions = canonical.system.as_ref().and_then(|blocks| {
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|b| {
                if let CanonicalContentBlock::Text { text } = b {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n\n"))
        }
    });

    // Build input array from messages
    let mut input_items = vec![];

    for msg in &canonical.messages {
        match msg.role {
            CanonicalRole::System => {
                // System messages in messages array are merged into instructions
                // but we handle them here just in case
                continue;
            }
            _ => {}
        }

        // Check for tool-related content blocks
        let has_tool_use = msg
            .content
            .iter()
            .any(|b| matches!(b, CanonicalContentBlock::ToolUse { .. }));
        let has_tool_result = msg
            .content
            .iter()
            .any(|b| matches!(b, CanonicalContentBlock::ToolResult { .. }));

        if has_tool_use {
            // Assistant message with tool use → function_call items
            for block in &msg.content {
                match block {
                    CanonicalContentBlock::ToolUse { id, name, input } => {
                        input_items.push(json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": serde_json::to_string(input).unwrap_or_default()
                        }));
                    }
                    CanonicalContentBlock::Text { text } => {
                        if !text.is_empty() {
                            input_items.push(json!({
                                "type": "message",
                                "role": "assistant",
                                "content": text
                            }));
                        }
                    }
                    _ => {}
                }
            }
        } else if has_tool_result {
            // User message with tool results → function_call_output items
            for block in &msg.content {
                match block {
                    CanonicalContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    } => {
                        let output = content
                            .iter()
                            .filter_map(|b| {
                                if let CanonicalContentBlock::Text { text } = b {
                                    Some(text.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let output = if output.trim()
                            == "[Tool result missing due to internal error]"
                        {
                            "Error: Tool runtime internal error (result missing). Please retry the same tool call with identical parameters.".to_string()
                        } else {
                            output
                        };

                        input_items.push(json!({
                            "type": "function_call_output",
                            "call_id": tool_use_id,
                            "output": output
                        }));
                    }
                    CanonicalContentBlock::Text { text } => {
                        if !text.is_empty() {
                            input_items.push(json!({
                                "type": "message",
                                "role": "user",
                                "content": text
                            }));
                        }
                    }
                    _ => {}
                }
            }
        } else {
            // Regular message
            let role = match msg.role {
                CanonicalRole::User => "user",
                CanonicalRole::Assistant => "assistant",
                CanonicalRole::System => "system",
            };

            if msg.content.is_empty() {
                input_items.push(json!({
                    "type": "message",
                    "role": role,
                    "content": ""
                }));
            } else if msg.content.len() == 1 {
                match &msg.content[0] {
                    CanonicalContentBlock::Text { text } => {
                        input_items.push(json!({
                            "type": "message",
                            "role": role,
                            "content": text
                        }));
                    }
                    CanonicalContentBlock::Image { source } => {
                        let file_url = match source {
                            CanonicalImageSource::Url { url } => url.clone(),
                            CanonicalImageSource::Base64 { .. } => String::new(),
                        };
                        if !file_url.is_empty() {
                            input_items.push(json!({
                                "type": "file",
                                "file_url": file_url
                            }));
                        }
                    }
                    _ => {}
                }
            } else {
                // Multiple content blocks → array
                let content_blocks: Vec<serde_json::Value> = msg
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        CanonicalContentBlock::Text { text } => {
                            Some(json!({"type": "input_text", "text": text}))
                        }
                        CanonicalContentBlock::Image { source } => match source {
                            CanonicalImageSource::Url { url } => {
                                Some(json!({"type": "image_url", "image_url": {"url": url}}))
                            }
                            CanonicalImageSource::Base64 {
                                media_type: _,
                                data,
                            } => Some(
                                json!({"type": "input_image", "image_url": data, "format": "png"}),
                            ),
                        },
                        _ => None,
                    })
                    .collect();

                if !content_blocks.is_empty() {
                    input_items.push(json!({
                        "type": "message",
                        "role": role,
                        "content": content_blocks
                    }));
                }
            }
        }
    }

    // Build tools
    let tools = canonical.tools.as_ref().map(|t| {
        t.iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    // Anthropic tools often include optional fields; keep non-strict behavior
                    // so Codex can emit partial argument objects that still pass client validation.
                    "strict": false,
                    "parameters": tool.input_schema
                })
            })
            .collect::<Vec<_>>()
    });

    let mut result = json!({
        "model": canonical.model,
        "input": if input_items.is_empty() {
            // If no input items but we had messages, use empty array
            // Otherwise omit
            if !canonical.messages.is_empty() {
                json!([])
            } else {
                serde_json::Value::Null
            }
        } else {
            json!(input_items)
        },
    });

    // Remove null/empty input when there are no messages
    if canonical.messages.is_empty() {
        if let Some(obj) = result.as_object_mut() {
            let should_remove = match obj.get("input") {
                Some(serde_json::Value::Null) => true,
                Some(serde_json::Value::Array(a)) => a.is_empty(),
                _ => false,
            };
            if should_remove {
                obj.remove("input");
            }
        }
    }

    // Add optional fields
    if let Some(obj) = result.as_object_mut() {
        if let Some(instr) = instructions {
            obj.insert("instructions".to_string(), json!(instr));
        }
        if let Some(max_tokens) = canonical.max_tokens {
            obj.insert("max_output_tokens".to_string(), json!(max_tokens));
        }
        if let Some(temp) = canonical.temperature {
            obj.insert("temperature".to_string(), json!(temp));
        }
        if let Some(top_p) = canonical.top_p {
            obj.insert("top_p".to_string(), json!(top_p));
        }
        if let Some(stop_seqs) = &canonical.stop_sequences {
            obj.insert("stop_sequences".to_string(), json!(stop_seqs));
        }
        if canonical.stream {
            obj.insert("stream".to_string(), json!(true));
        }
        if let Some(t) = tools {
            obj.insert("tools".to_string(), json!(t));
        }
        if let Some(tc) = canonical
            .tool_choice
            .as_ref()
            .and_then(normalize_tool_choice_for_responses)
        {
            obj.insert("tool_choice".to_string(), tc);
        }
        if let Some(effort) = &canonical.reasoning_effort {
            obj.insert("reasoning".to_string(), json!({ "effort": effort }));
        }
    }

    Ok(result)
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

    // Parse output items into content blocks
    let mut content: Vec<CanonicalContentBlock> = vec![];
    let mut has_function_call_output = false;

    if let Some(output) = body.get("output").and_then(|o| o.as_array()) {
        for item in output {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match item_type {
                "message" => {
                    if let Some(content_arr) = item.get("content").and_then(|c| c.as_array()) {
                        for block in content_arr {
                            if let Some(parsed) = parse_responses_content_block(block) {
                                content.push(parsed);
                            }
                        }
                    }
                }
                "function_call" => {
                    has_function_call_output = true;
                    let id = item
                        .get("call_id")
                        .and_then(|i| i.as_str())
                        .or_else(|| item.get("id").and_then(|i| i.as_str()))
                        .unwrap_or("");
                    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let arguments_str = item
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("{}");

                    let call_id = if !id.is_empty() {
                        id.to_string()
                    } else {
                        item.get("id")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string()
                    };

                    if !call_id.is_empty() {
                        let input: serde_json::Value = serde_json::from_str(arguments_str)
                            .unwrap_or_else(|_| json!({"raw": arguments_str}));

                        content.push(CanonicalContentBlock::ToolUse {
                            id: call_id,
                            name: name.to_string(),
                            input,
                        });
                    }
                }
                "reasoning" => {
                    if let Some(summary) = item.get("summary").and_then(|s| s.as_array()) {
                        for block in summary {
                            if let Some(parsed) = parse_responses_content_block(block) {
                                content.push(parsed);
                            }
                        }
                    }
                    if let Some(content_arr) = item.get("content").and_then(|c| c.as_array()) {
                        for block in content_arr {
                            if let Some(parsed) = parse_responses_content_block(block) {
                                content.push(parsed);
                            }
                        }
                    }
                }
                "file_search" | "web_search" | "code_interpreter" | _ => {
                    // Skip unmapped output types
                }
            }
        }
    }

    // Determine stop reason from status and stop_reason
    let status = body
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("completed");
    let stop_reason_str = body
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .unwrap_or("");

    let stop_reason = if status == "in_progress" {
        None
    } else if status == "incomplete" {
        Some(CanonicalStopReason::MaxTokens)
    } else {
        match stop_reason_str {
            "end_turn" => Some(CanonicalStopReason::EndTurn),
            "" if has_function_call_output => Some(CanonicalStopReason::ToolUse),
            "" => Some(CanonicalStopReason::EndTurn),
            "max_output_tokens" | "max_tokens" => Some(CanonicalStopReason::MaxTokens),
            "stop_sequence" => Some(CanonicalStopReason::StopSequence),
            "tool_calls" | "function_call" => Some(CanonicalStopReason::ToolUse),
            _ => Some(CanonicalStopReason::EndTurn),
        }
    };

    // Parse usage
    let usage = body.get("usage").and_then(|u| {
        let input_tokens = u.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
        let output_tokens = u.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
        let total_tokens = u.get("total_tokens").and_then(|t| t.as_u64());

        Some(CanonicalUsage {
            input_tokens,
            output_tokens,
            total_tokens,
        })
    });

    Ok(CanonicalResponse {
        id,
        model,
        content,
        stop_reason,
        usage,
    })
}

pub fn from_canonical_response(canonical: &CanonicalResponse) -> Result<serde_json::Value> {
    // Build output items from content blocks
    let mut output_items = vec![];

    let mut text_blocks = vec![];
    let mut function_calls = vec![];

    for block in &canonical.content {
        match block {
            CanonicalContentBlock::Text { text } => {
                text_blocks.push(json!({
                    "type": "output_text",
                    "text": text
                }));
            }
            CanonicalContentBlock::Reasoning { text } => {
                output_items.push(json!({
                    "type": "reasoning",
                    "summary": [{
                        "type": "reasoning_text",
                        "text": text
                    }]
                }));
            }
            CanonicalContentBlock::ToolUse { id, name, input } => {
                function_calls.push(json!({
                    "type": "function_call",
                    "id": id,
                    "call_id": id,
                    "name": name,
                    "arguments": serde_json::to_string(input).unwrap_or_default()
                }));
            }
            CanonicalContentBlock::Image { source } => {
                let (url, format_name) = match source {
                    CanonicalImageSource::Url { url } => (url.clone(), "png"),
                    CanonicalImageSource::Base64 {
                        media_type: _,
                        data,
                    } => (data.clone(), "png"),
                };
                output_items.push(json!({
                    "type": "message",
                    "content": [{
                        "type": "input_image",
                        "image_url": url,
                        "format": format_name
                    }]
                }));
            }
            _ => {}
        }
    }

    // If we have text blocks, wrap them in a message item
    if !text_blocks.is_empty() {
        output_items.push(json!({
            "type": "message",
            "content": text_blocks
        }));
    }

    // Add function calls
    output_items.extend(function_calls);

    // Build status
    let status = match canonical.stop_reason {
        Some(CanonicalStopReason::ToolUse) => "completed",
        Some(_) => "completed",
        None => "in_progress",
    };

    // Determine stop_reason string
    let stop_reason = match canonical.stop_reason {
        Some(CanonicalStopReason::EndTurn) => "end_turn",
        Some(CanonicalStopReason::MaxTokens) => "max_output_tokens",
        Some(CanonicalStopReason::StopSequence) => "stop_sequence",
        Some(CanonicalStopReason::ToolUse) => "tool_calls",
        None => "",
    };

    let mut result = json!({
        "id": canonical.id,
        "model": canonical.model,
        "output": output_items,
        "status": status,
    });

    if !stop_reason.is_empty() {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("stop_reason".to_string(), json!(stop_reason));
        }
    }

    if let Some(usage) = &canonical.usage {
        let mut usage_obj = json!({
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
        });
        if let Some(total) = usage.total_tokens {
            if let Some(obj) = usage_obj.as_object_mut() {
                obj.insert("total_tokens".to_string(), json!(total));
            }
        }
        if let Some(obj) = result.as_object_mut() {
            obj.insert("usage".to_string(), usage_obj);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_input_to_canonical() {
        let body = json!({
            "model": "gpt-4",
            "input": "Hello, world!"
        });
        let req = to_canonical_request(&body).unwrap();
        assert_eq!(req.model, "gpt-4");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, CanonicalRole::User);
        assert_eq!(req.messages[0].content.len(), 1);
        if let CanonicalContentBlock::Text { text } = &req.messages[0].content[0] {
            assert_eq!(text, "Hello, world!");
        } else {
            panic!("expected text block");
        }
    }

    #[test]
    fn test_array_input_with_messages() {
        let body = json!({
            "model": "gpt-4",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": "What is the weather?"
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": "Let me check."
                }
            ]
        });
        let req = to_canonical_request(&body).unwrap();
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, CanonicalRole::User);
        assert_eq!(req.messages[1].role, CanonicalRole::Assistant);
    }

    #[test]
    fn test_instructions_to_system() {
        let body = json!({
            "model": "gpt-4",
            "input": "Hello",
            "instructions": "You are a helpful assistant."
        });
        let req = to_canonical_request(&body).unwrap();
        assert!(req.system.is_some());
        let sys = req.system.unwrap();
        assert_eq!(sys.len(), 1);
        if let CanonicalContentBlock::Text { text } = &sys[0] {
            assert_eq!(text, "You are a helpful assistant.");
        } else {
            panic!("expected text block");
        }
    }

    #[test]
    fn test_max_output_tokens_mapping() {
        let body = json!({
            "model": "gpt-4",
            "input": "Hello",
            "max_output_tokens": 100
        });
        let req = to_canonical_request(&body).unwrap();
        assert_eq!(req.max_tokens, Some(100));
    }

    #[test]
    fn test_tools_mapping() {
        let body = json!({
            "model": "gpt-4",
            "input": "Hello",
            "tools": [
                {
                    "type": "function",
                    "name": "get_weather",
                    "description": "Get weather info",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "location": {"type": "string"}
                        }
                    }
                }
            ]
        });
        let req = to_canonical_request(&body).unwrap();
        assert!(req.tools.is_some());
        let tools = req.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(tools[0].description, Some("Get weather info".to_string()));
    }

    #[test]
    fn test_function_call_output_to_tool_result() {
        let body = json!({
            "model": "gpt-4",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": "What is 2+2?"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_123",
                    "output": "4"
                }
            ]
        });
        let req = to_canonical_request(&body).unwrap();
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[1].role, CanonicalRole::User);
        assert_eq!(req.messages[1].content.len(), 1);
        if let CanonicalContentBlock::ToolResult { tool_use_id, .. } = &req.messages[1].content[0] {
            assert_eq!(tool_use_id, "call_123");
        } else {
            panic!("expected tool result block");
        }
    }

    #[test]
    fn test_from_canonical_request_rewrites_missing_tool_result_placeholder() {
        let canonical = CanonicalRequest {
            model: "gpt-4".to_string(),
            messages: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![CanonicalContentBlock::ToolResult {
                    tool_use_id: "call_abc".to_string(),
                    content: vec![CanonicalContentBlock::Text {
                        text: "[Tool result missing due to internal error]".to_string(),
                    }],
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
            reasoning_effort: None,
            unmapped: vec![],
        };

        let result = from_canonical_request(&canonical).unwrap();
        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "call_abc");
        assert!(input[0]["output"]
            .as_str()
            .unwrap()
            .contains("Please retry the same tool call"));
    }

    #[test]
    fn test_from_canonical_request_basic() {
        let canonical = CanonicalRequest {
            model: "gpt-4".to_string(),
            messages: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![CanonicalContentBlock::Text {
                    text: "Hello".to_string(),
                }],
            }],
            system: Some(vec![CanonicalContentBlock::Text {
                text: "You are helpful".to_string(),
            }]),
            temperature: Some(0.7),
            top_p: None,
            max_tokens: Some(100),
            stop_sequences: None,
            stream: false,
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            unmapped: vec![],
        };

        let result = from_canonical_request(&canonical).unwrap();
        assert_eq!(result["model"], "gpt-4");
        assert_eq!(result["instructions"], "You are helpful");
        assert_eq!(result["max_output_tokens"], 100);
        assert_eq!(result["temperature"], 0.7);
    }

    #[test]
    fn test_from_canonical_request_with_tool_use() {
        let canonical = CanonicalRequest {
            model: "gpt-4".to_string(),
            messages: vec![
                CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![CanonicalContentBlock::Text {
                        text: "What is the weather?".to_string(),
                    }],
                },
                CanonicalMessage {
                    role: CanonicalRole::Assistant,
                    content: vec![CanonicalContentBlock::ToolUse {
                        id: "call_abc".to_string(),
                        name: "get_weather".to_string(),
                        input: json!({"location": "NYC"}),
                    }],
                },
            ],
            system: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            stream: false,
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            unmapped: vec![],
        };

        let result = from_canonical_request(&canonical).unwrap();
        let input = result["input"].as_array().unwrap();
        assert!(input.len() >= 2);
        // Last item should be a function_call
        let last = input.last().unwrap();
        assert_eq!(last["type"], "function_call");
        assert_eq!(last["call_id"], "call_abc");
        assert!(last.get("id").is_none());
        assert_eq!(last["name"], "get_weather");
    }

    #[test]
    fn test_to_canonical_response_with_text() {
        let body = json!({
            "id": "resp_123",
            "model": "gpt-4",
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type": "text", "text": "Hello there!"}
                    ]
                }
            ],
            "status": "completed",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20
            }
        });
        let resp = to_canonical_response(&body).unwrap();
        assert_eq!(resp.id, "resp_123");
        assert_eq!(resp.model, "gpt-4");
        assert_eq!(resp.content.len(), 1);
        if let CanonicalContentBlock::Text { text } = &resp.content[0] {
            assert_eq!(text, "Hello there!");
        } else {
            panic!("expected text block");
        }
        assert_eq!(resp.stop_reason, Some(CanonicalStopReason::EndTurn));
        assert!(resp.usage.is_some());
        let usage = resp.usage.unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 20);
    }

    #[test]
    fn test_to_canonical_response_preserves_reasoning_blocks() {
        let body = json!({
            "id": "resp_reasoning",
            "model": "qwen",
            "output": [
                {
                    "type": "reasoning",
                    "content": [
                        {
                            "type": "reasoning_text",
                            "text": "hidden thinking"
                        }
                    ]
                },
                {
                    "type": "message",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "OK"
                        }
                    ]
                }
            ],
            "status": "completed",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 2
            }
        });

        let resp = to_canonical_response(&body).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert!(matches!(
            &resp.content[0],
            CanonicalContentBlock::Reasoning { text } if text == "hidden thinking"
        ));
        assert!(matches!(
            &resp.content[1],
            CanonicalContentBlock::Text { text } if text == "OK"
        ));
    }

    #[test]
    fn test_to_canonical_response_with_function_call() {
        let body = json!({
            "id": "resp_456",
            "model": "gpt-4",
            "output": [
                {
                    "type": "function_call",
                    "id": "call_xyz",
                    "name": "search",
                    "arguments": "{\"query\":\"test\"}"
                }
            ],
            "status": "completed",
            "stop_reason": "tool_calls"
        });
        let resp = to_canonical_response(&body).unwrap();
        assert_eq!(resp.content.len(), 1);
        if let CanonicalContentBlock::ToolUse { id, name, input } = &resp.content[0] {
            assert_eq!(id, "call_xyz");
            assert_eq!(name, "search");
            assert_eq!(input["query"], "test");
        } else {
            panic!("expected tool use block");
        }
        assert_eq!(resp.stop_reason, Some(CanonicalStopReason::ToolUse));
    }

    #[test]
    fn test_to_canonical_response_with_function_call_prefers_call_id() {
        let body = json!({
            "id": "resp_456",
            "model": "gpt-4",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc_internal_1",
                    "call_id": "call_public_1",
                    "name": "search",
                    "arguments": "{\"query\":\"test\"}"
                }
            ],
            "status": "completed",
            "stop_reason": "tool_calls"
        });
        let resp = to_canonical_response(&body).unwrap();
        assert_eq!(resp.content.len(), 1);
        if let CanonicalContentBlock::ToolUse { id, name, input } = &resp.content[0] {
            assert_eq!(id, "call_public_1");
            assert_eq!(name, "search");
            assert_eq!(input["query"], "test");
        } else {
            panic!("expected tool use block");
        }
    }

    #[test]
    fn test_from_canonical_response_with_text() {
        let canonical = CanonicalResponse {
            id: "resp_789".to_string(),
            model: "gpt-4".to_string(),
            content: vec![CanonicalContentBlock::Text {
                text: "Here is the answer.".to_string(),
            }],
            stop_reason: Some(CanonicalStopReason::EndTurn),
            usage: Some(CanonicalUsage {
                input_tokens: 50,
                output_tokens: 30,
                total_tokens: Some(80),
            }),
        };

        let result = from_canonical_response(&canonical).unwrap();
        assert_eq!(result["id"], "resp_789");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["stop_reason"], "end_turn");
        assert_eq!(result["usage"]["input_tokens"], 50);
        assert_eq!(result["usage"]["total_tokens"], 80);
    }

    #[test]
    fn test_from_canonical_response_with_tool_use() {
        let canonical = CanonicalResponse {
            id: "resp_tool".to_string(),
            model: "gpt-4".to_string(),
            content: vec![
                CanonicalContentBlock::Text {
                    text: "Let me search.".to_string(),
                },
                CanonicalContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "search".to_string(),
                    input: json!({"q": "test"}),
                },
            ],
            stop_reason: Some(CanonicalStopReason::ToolUse),
            usage: None,
        };

        let result = from_canonical_response(&canonical).unwrap();
        let output = result["output"].as_array().unwrap();
        assert_eq!(output.len(), 2);
        // First should be message with text
        assert_eq!(output[0]["type"], "message");
        // Second should be function_call
        assert_eq!(output[1]["type"], "function_call");
        assert_eq!(output[1]["name"], "search");
    }

    #[test]
    fn test_from_canonical_tools_mapping() {
        let canonical = CanonicalRequest {
            model: "gpt-4".to_string(),
            messages: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![CanonicalContentBlock::Text {
                    text: "Hi".to_string(),
                }],
            }],
            system: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            stream: false,
            tools: Some(vec![CanonicalTool {
                name: "calc".to_string(),
                description: Some("Calculate".to_string()),
                input_schema: json!({"type": "object"}),
            }]),
            tool_choice: None,
            reasoning_effort: None,
            unmapped: vec![],
        };

        let result = from_canonical_request(&canonical).unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "calc");
        assert_eq!(tools[0]["description"], "Calculate");
        assert_eq!(tools[0]["strict"], false);
    }

    #[test]
    fn test_from_canonical_tool_choice_any_maps_to_required() {
        let canonical = CanonicalRequest {
            model: "gpt-4".to_string(),
            messages: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![CanonicalContentBlock::Text {
                    text: "Use a tool".to_string(),
                }],
            }],
            system: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            stream: false,
            tools: None,
            tool_choice: Some(json!({"type":"any"})),
            reasoning_effort: None,
            unmapped: vec![],
        };

        let result = from_canonical_request(&canonical).unwrap();
        assert_eq!(result["tool_choice"], "required");
    }

    #[test]
    fn test_from_canonical_tool_choice_tool_maps_to_function() {
        let canonical = CanonicalRequest {
            model: "gpt-4".to_string(),
            messages: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![CanonicalContentBlock::Text {
                    text: "Use weather tool".to_string(),
                }],
            }],
            system: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            stream: false,
            tools: None,
            tool_choice: Some(json!({"type":"tool","name":"get_weather"})),
            reasoning_effort: None,
            unmapped: vec![],
        };

        let result = from_canonical_request(&canonical).unwrap();
        assert_eq!(
            result["tool_choice"],
            json!({"type":"function","name":"get_weather"})
        );
    }

    #[test]
    fn test_to_canonical_request_parses_reasoning_effort() {
        let body = json!({
            "model": "gpt-4.1",
            "input": "hello",
            "reasoning": {"effort": "high"}
        });
        let req = to_canonical_request(&body).unwrap();
        assert_eq!(req.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn test_from_canonical_request_emits_reasoning_effort_to_reasoning() {
        let canonical = CanonicalRequest {
            model: "gpt-5.3-codex".to_string(),
            messages: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![CanonicalContentBlock::Text {
                    text: "hi".to_string(),
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
            reasoning_effort: Some("medium".to_string()),
            unmapped: vec![],
        };

        let result = from_canonical_request(&canonical).unwrap();
        assert_eq!(result["reasoning"]["effort"], "medium");
    }

    #[test]
    fn test_stop_reason_mapping() {
        // Max tokens
        let body = json!({
            "id": "r1", "model": "gpt-4", "output": [],
            "status": "completed", "stop_reason": "max_output_tokens"
        });
        let resp = to_canonical_response(&body).unwrap();
        assert_eq!(resp.stop_reason, Some(CanonicalStopReason::MaxTokens));

        // In progress
        let body = json!({
            "id": "r2", "model": "gpt-4", "output": [],
            "status": "in_progress"
        });
        let resp = to_canonical_response(&body).unwrap();
        assert_eq!(resp.stop_reason, None);

        // Incomplete
        let body = json!({
            "id": "r3", "model": "gpt-4", "output": [],
            "status": "incomplete"
        });
        let resp = to_canonical_response(&body).unwrap();
        assert_eq!(resp.stop_reason, Some(CanonicalStopReason::MaxTokens));
    }

    #[test]
    fn test_stop_reason_infers_tool_use_from_function_call_output() {
        let body = json!({
            "id": "r_tool",
            "model": "gpt-4",
            "status": "completed",
            "output": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "echo_tool",
                    "arguments": "{\"text\":\"hi\"}"
                }
            ]
        });
        let resp = to_canonical_response(&body).unwrap();
        assert_eq!(resp.stop_reason, Some(CanonicalStopReason::ToolUse));
    }

    #[test]
    fn test_reasoning_summary_text_parsing() {
        let body = json!({
            "id": "resp_reasoning",
            "model": "gpt-5.5",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "summary": [
                        {"type": "summary_text", "text": "first"},
                        {"type": "reasoning_text", "text": " second"}
                    ]
                }
            ]
        });

        let resp = to_canonical_response(&body).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert!(matches!(
            &resp.content[0],
            CanonicalContentBlock::Reasoning { text } if text == "first"
        ));
        assert!(matches!(
            &resp.content[1],
            CanonicalContentBlock::Reasoning { text } if text == " second"
        ));
    }

    #[test]
    fn test_input_image_content_block() {
        let body = json!({
            "model": "gpt-4",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Describe this image"},
                        {"type": "input_image", "image_url": "https://example.com/img.png"}
                    ]
                }
            ]
        });
        let req = to_canonical_request(&body).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].content.len(), 2);
        if let CanonicalContentBlock::Image { source } = &req.messages[0].content[1] {
            if let CanonicalImageSource::Url { url } = source {
                assert_eq!(url, "https://example.com/img.png");
            } else {
                panic!("expected url source");
            }
        } else {
            panic!("expected image block");
        }
    }

    #[test]
    fn test_file_input_skipped() {
        let body = json!({
            "model": "gpt-4",
            "input": [
                {"type": "message", "role": "user", "content": "Analyze this file"},
                {"type": "file", "file_url": "https://example.com/doc.pdf"}
            ]
        });
        let req = to_canonical_request(&body).unwrap();
        // File item should be skipped, only the message remains
        assert_eq!(req.messages.len(), 1);
    }
}
