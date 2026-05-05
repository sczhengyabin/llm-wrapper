//! 流式 SSE 事件转换

use anyhow::Result;
use futures::Stream;
use futures::StreamExt;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::vec::IntoIter;

use super::{CanonicalStopReason, Protocol};

/// SSE 事件
#[derive(Debug)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum CanonicalStreamEvent {
    /// 文本增量
    TextDelta { text: String },
    /// 推理/思考增量
    ReasoningDelta { text: String },
    /// 工具调用开始
    ToolUseStart {
        id: String,
        index: Option<u64>,
        name: String,
        input: serde_json::Value,
    },
    /// 工具调用增量（部分参数）
    ToolInputDelta {
        tool_use_id: String,
        index: Option<u64>,
        input_chunk: String,
    },
    /// 停止信号
    Stop { reason: CanonicalStopReason },
    /// Token 用量
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    /// 原始事件（无法分类）
    Raw { event: Option<String>, data: String },
}

/// SSE 解析器
pub struct SseParser {
    buffer: String,
    current_event: Option<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            current_event: None,
        }
    }

    /// 从数据中解析 SSE 事件，返回完整的事件
    ///
    /// 只处理以换行符结尾的完整行；不完整的行保留在 buffer 中等待下一个 chunk。
    pub fn feed(&mut self, data: &[u8]) -> Vec<SseEvent> {
        let text = match std::str::from_utf8(data) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        self.buffer.push_str(text);
        let mut events = Vec::new();

        // 只处理有换行符的完整行，不完整行留在 buffer 中
        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = self.buffer[..newline_pos].to_string();
            self.buffer.drain(..=newline_pos);

            if line.starts_with("event:") {
                self.current_event = Some(line[6..].trim().to_string());
            } else if line.starts_with("data:") {
                let data_str = line[5..].trim().to_string();
                if !data_str.is_empty() {
                    let event = self.current_event.take();
                    events.push(SseEvent {
                        event,
                        data: data_str,
                    });
                }
            } else if line.is_empty() {
                self.current_event = None;
            }
        }

        events
    }
}

#[allow(dead_code)]
/// 将上游 SSE 流转换为规范流事件
pub struct StreamConverter<F, T> {
    parser: SseParser,
    inner: F,
    _phantom: std::marker::PhantomData<T>,
}

impl<F, T> StreamConverter<F, T>
where
    F: Stream<Item = Result<Vec<u8>, std::io::Error>> + Unpin,
    T: Send + 'static,
{
    pub fn new(stream: F) -> Self {
        Self {
            parser: SseParser::new(),
            inner: stream,
            _phantom: std::marker::PhantomData,
        }
    }
}

/// 协议选择器，决定 SSE 事件的解析策略
#[derive(Clone, Copy)]
enum EventParser {
    OpenAI,
    Anthropic,
    Responses,
}

/// 通用协议流：从字节流中提取 SSE 事件，按协议转换为 CanonicalStreamEvent
struct ProtocolStream<F> {
    sse_parser: SseParser,
    inner: F,
    pending: IntoIter<SseEvent>,
    parser_kind: EventParser,
}

impl<F> ProtocolStream<F>
where
    F: Stream<Item = Result<Vec<u8>, std::io::Error>> + Unpin + Send + 'static,
{
    fn new(stream: F, parser_kind: EventParser) -> Self {
        Self {
            sse_parser: SseParser::new(),
            inner: stream,
            pending: Vec::new().into_iter(),
            parser_kind,
        }
    }

    /// 将 SSE 事件转换为 CanonicalStreamEvent，策略取决于 parser_kind
    fn convert_event(&self, sse: &SseEvent) -> Result<CanonicalStreamEvent, String> {
        match self.parser_kind {
            EventParser::OpenAI => convert_openai_event(sse),
            EventParser::Anthropic => {
                match convert_anthropic_event(sse) {
                    Some(r) => r,
                    None => {
                        // Anthropic parser chose to skip; emit Raw if data is non-trivial
                        if sse.event.is_some() && !sse.data.is_empty() {
                            Ok(CanonicalStreamEvent::Raw {
                                event: sse.event.clone(),
                                data: sse.data.clone(),
                            })
                        } else if !sse.data.is_empty() && sse.event.is_none() {
                            Ok(CanonicalStreamEvent::Raw {
                                event: None,
                                data: sse.data.clone(),
                            })
                        } else {
                            // This shouldn't happen because we skip None in poll
                            Err("unexpected skipped event".to_string())
                        }
                    }
                }
            }
            EventParser::Responses => convert_responses_event(sse),
        }
    }
}

impl<F> Stream for ProtocolStream<F>
where
    F: Stream<Item = Result<Vec<u8>, std::io::Error>> + Unpin + Send + 'static,
{
    type Item = Result<CanonicalStreamEvent, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            // First, drain pending events
            if let Some(event) = self.pending.next() {
                // For Anthropic, the converter may choose to skip certain events
                if matches!(self.parser_kind, EventParser::Anthropic) {
                    // Check if this event should be skipped
                    match event.event.as_deref() {
                        Some("ping") | Some("content_block_stop") | Some("message_stop") => {
                            continue
                        }
                        _ => {}
                    }
                }

                return Poll::Ready(Some(self.convert_event(&event)));
            }

            // No pending events - poll the inner stream for more bytes
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let events = self.sse_parser.feed(&bytes);
                    self.pending = events.into_iter();
                    // Continue loop; pending now has events
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(e.to_string())));
                }
                Poll::Ready(None) => {
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// 将上游 SSE 流按协议转换为规范流事件
pub fn parse_stream_events<F>(
    protocol: Protocol,
    stream: F,
) -> Pin<Box<dyn Stream<Item = Result<CanonicalStreamEvent, String>> + Send + 'static>>
where
    F: Stream<Item = Result<Vec<u8>, std::io::Error>> + Unpin + Send + 'static,
{
    let parser_kind = match protocol {
        Protocol::ChatCompletions => EventParser::OpenAI,
        Protocol::AnthropicMessages => EventParser::Anthropic,
        Protocol::Responses => EventParser::Responses,
    };
    Box::pin(ProtocolStream::new(stream, parser_kind))
}

/// 解析 OpenAI SSE 流
#[allow(dead_code)]
fn parse_openai_stream<F>(stream: F) -> ProtocolStream<F>
where
    F: Stream<Item = Result<Vec<u8>, std::io::Error>> + Unpin + Send + 'static,
{
    ProtocolStream::new(stream, EventParser::OpenAI)
}

/// 解析 Anthropic SSE 流
#[allow(dead_code)]
fn parse_anthropic_stream<F>(stream: F) -> ProtocolStream<F>
where
    F: Stream<Item = Result<Vec<u8>, std::io::Error>> + Unpin + Send + 'static,
{
    ProtocolStream::new(stream, EventParser::Anthropic)
}

/// 解析 Responses SSE 流
#[allow(dead_code)]
fn parse_responses_stream<F>(stream: F) -> ProtocolStream<F>
where
    F: Stream<Item = Result<Vec<u8>, std::io::Error>> + Unpin + Send + 'static,
{
    ProtocolStream::new(stream, EventParser::Responses)
}

// ---------------------------------------------------------------------------
// Protocol-specific event converters
// ---------------------------------------------------------------------------

fn convert_openai_event(sse: &SseEvent) -> Result<CanonicalStreamEvent, String> {
    // [DONE] marker
    if sse.data == "[DONE]" {
        return Ok(CanonicalStreamEvent::Raw {
            event: sse.event.clone(),
            data: "[DONE]".to_string(),
        });
    }

    match serde_json::from_str::<serde_json::Value>(&sse.data) {
        Ok(json) => parse_openai_event(&json),
        Err(_) => Ok(CanonicalStreamEvent::Raw {
            event: sse.event.clone(),
            data: sse.data.clone(),
        }),
    }
}

/// 解析单个 OpenAI 流式事件 JSON
pub fn parse_openai_event(json: &serde_json::Value) -> Result<CanonicalStreamEvent, String> {
    // Check choices for delta content
    if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
        if let Some(choice) = choices.get(0) {
            // Check finish_reason first
            if let Some(finish) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                let reason = match finish {
                    "stop" | "content_filter" => CanonicalStopReason::EndTurn,
                    "length" => CanonicalStopReason::MaxTokens,
                    "tool_calls" => CanonicalStopReason::ToolUse,
                    _ => CanonicalStopReason::EndTurn,
                };
                return Ok(CanonicalStreamEvent::Stop { reason });
            }

            // Extract delta
            if let Some(delta) = choice.get("delta") {
                // Text content
                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        return Ok(CanonicalStreamEvent::TextDelta {
                            text: content.to_string(),
                        });
                    }
                }

                for key in ["reasoning_content", "reasoning", "thinking"] {
                    if let Some(content) = delta.get(key).and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            return Ok(CanonicalStreamEvent::ReasoningDelta {
                                text: content.to_string(),
                            });
                        }
                    }
                }

                // Tool calls
                if let Some(tool_calls) = delta.get("tool_calls").and_then(|tc| tc.as_array()) {
                    for tc in tool_calls {
                        let tc_index = tc.get("index").and_then(|v| v.as_u64());
                        let mut id = tc
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        if id.is_empty() {
                            if let Some(idx) = tc_index {
                                id = format!("tool_{}", idx);
                            }
                        }
                        let function = tc.get("function");

                        // Function name
                        if let Some(func) = function {
                            if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                                if !name.is_empty() {
                                    let input = func
                                        .get("arguments")
                                        .and_then(|a| a.as_str())
                                        .and_then(|s| {
                                            serde_json::from_str::<serde_json::Value>(s).ok()
                                        })
                                        .unwrap_or_else(|| serde_json::json!({}));
                                    return Ok(CanonicalStreamEvent::ToolUseStart {
                                        id: id.clone(),
                                        index: tc_index,
                                        name: name.to_string(),
                                        input,
                                    });
                                }
                            }
                        }

                        // Function arguments (incremental)
                        if let Some(func) = function {
                            if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                                if !args.is_empty() {
                                    return Ok(CanonicalStreamEvent::ToolInputDelta {
                                        tool_use_id: id.clone(),
                                        index: tc_index,
                                        input_chunk: args.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Usage at top level
    if let Some(usage) = json.get("usage") {
        let input_tokens = usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output_tokens = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return Ok(CanonicalStreamEvent::Usage {
            input_tokens,
            output_tokens,
        });
    }

    Ok(CanonicalStreamEvent::Raw {
        event: None,
        data: json.to_string(),
    })
}

fn convert_anthropic_event(sse: &SseEvent) -> Option<Result<CanonicalStreamEvent, String>> {
    match serde_json::from_str::<serde_json::Value>(&sse.data) {
        Ok(json) => match parse_anthropic_event(sse.event.as_deref(), &json) {
            Ok(Some(ce)) => Some(Ok(ce)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        },
        Err(_) => {
            if sse.event.is_some() || !sse.data.is_empty() {
                Some(Ok(CanonicalStreamEvent::Raw {
                    event: sse.event.clone(),
                    data: sse.data.clone(),
                }))
            } else {
                None
            }
        }
    }
}

/// 解析单个 Anthropic 流式事件
pub fn parse_anthropic_event(
    event_name: Option<&str>,
    json: &serde_json::Value,
) -> Result<Option<CanonicalStreamEvent>, String> {
    let event_name = event_name.unwrap_or("");

    match event_name {
        "content_block_delta" => {
            if let Some(delta) = json.get("delta") {
                let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match delta_type {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                return Ok(Some(CanonicalStreamEvent::TextDelta {
                                    text: text.to_string(),
                                }));
                            }
                        }
                    }
                    "thinking_delta" => {
                        // vllm thinking model: {"type": "thinking_delta", "thinking": "..."}
                        if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                return Ok(Some(CanonicalStreamEvent::ReasoningDelta {
                                    text: text.to_string(),
                                }));
                            }
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial_json) =
                            delta.get("partial_json").and_then(|p| p.as_str())
                        {
                            if !partial_json.is_empty() {
                                let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                                return Ok(Some(CanonicalStreamEvent::ToolInputDelta {
                                    tool_use_id: format!("tool_{}", index),
                                    index: Some(index),
                                    input_chunk: partial_json.to_string(),
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(None)
        }
        "content_block_start" => {
            if let Some(content_block) = json.get("content_block") {
                let cb_type = content_block
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                if cb_type == "tool_use" {
                    let id = content_block
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = content_block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = content_block
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    return Ok(Some(CanonicalStreamEvent::ToolUseStart {
                        id,
                        index: json.get("index").and_then(|v| v.as_u64()),
                        name,
                        input,
                    }));
                }
            }
            Ok(None)
        }
        "message_delta" => {
            // stop_reason 可能在 delta 子对象或顶层
            let stop_reason = json
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .or_else(|| json.get("stop_reason"))
                .and_then(|s| s.as_str());
            if let Some(stop_reason) = stop_reason {
                let reason = match stop_reason {
                    "end_turn" => CanonicalStopReason::EndTurn,
                    "max_tokens" => CanonicalStopReason::MaxTokens,
                    "stop_sequence" => CanonicalStopReason::StopSequence,
                    "tool_use" => CanonicalStopReason::ToolUse,
                    _ => CanonicalStopReason::EndTurn,
                };
                return Ok(Some(CanonicalStreamEvent::Stop { reason }));
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn convert_responses_event(sse: &SseEvent) -> Result<CanonicalStreamEvent, String> {
    match serde_json::from_str::<serde_json::Value>(&sse.data) {
        Ok(json) => match parse_responses_event(&json) {
            Ok(Some(ce)) => Ok(ce),
            Ok(None) => Ok(CanonicalStreamEvent::Raw {
                event: sse.event.clone(),
                data: sse.data.clone(),
            }),
            Err(e) => Err(e),
        },
        Err(_) => Ok(CanonicalStreamEvent::Raw {
            event: sse.event.clone(),
            data: sse.data.clone(),
        }),
    }
}

fn tool_input_as_arguments(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Null => "{}".to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone)]
struct AggregatedToolCall {
    id: String,
    name: String,
    args: String,
}

fn build_responses_output_from_aggregated(
    output_items: &[serde_json::Value],
    reasoning_out: &str,
    text_out: &str,
    tool_calls: &HashMap<String, AggregatedToolCall>,
) -> Vec<serde_json::Value> {
    if !output_items.is_empty() {
        return output_items.to_vec();
    }

    let mut output = Vec::new();

    if !reasoning_out.is_empty() {
        output.push(serde_json::json!({
            "type": "reasoning",
            "summary": [{"type": "reasoning_text", "text": reasoning_out}]
        }));
    }

    if !text_out.is_empty() {
        output.push(serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text_out}]
        }));
    }

    for tool in tool_calls.values() {
        output.push(serde_json::json!({
            "type": "function_call",
            "call_id": tool.id,
            "name": tool.name,
            "arguments": if tool.args.is_empty() { "{}" } else { &tool.args },
        }));
    }

    output
}

/// 将 Responses SSE 流聚合为单个 Responses JSON（用于上游强制 stream=true 的场景）
///
/// 主要用于 Codex 上游：客户端请求非流式时，上游仍返回 SSE，
/// 此函数负责 drain 全部事件并还原为可继续走协议转换链路的 Responses 响应体。
pub async fn drain_responses_sse_to_json(
    response: reqwest::Response,
    fallback_model: Option<&str>,
) -> std::result::Result<serde_json::Value, String> {
    let stream = response.bytes_stream().map(|item| {
        item.map(Vec::from)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    });
    drain_responses_sse_bytes(stream, fallback_model).await
}

async fn drain_responses_sse_bytes<S>(
    mut stream: S,
    fallback_model: Option<&str>,
) -> std::result::Result<serde_json::Value, String>
where
    S: Stream<Item = std::result::Result<Vec<u8>, std::io::Error>> + Unpin,
{
    let mut parser = SseParser::new();

    let mut text_out = String::new();
    let mut reasoning_out = String::new();
    let mut tool_calls: HashMap<String, AggregatedToolCall> = HashMap::new();
    let mut item_id_to_call_id: HashMap<String, String> = HashMap::new();
    let mut output_items: Vec<serde_json::Value> = Vec::new();
    let mut completed_response: Option<serde_json::Value> = None;
    let mut upstream_error: Option<String> = None;
    let mut status = "completed".to_string();
    let mut usage: Option<serde_json::Value> = None;

    let mut process_event_data = |data: &str| {
        let json: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return,
        };
        let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "response.output_text.delta" | "response.text.delta" => {
                if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                    text_out.push_str(delta);
                }
            }
            "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning.delta"
            | "response.output_reasoning.delta" => {
                if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                    reasoning_out.push_str(delta);
                }
            }
            "response.output_item.added" => {
                if let Some(item) = json.get("item") {
                    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !call_id.is_empty() {
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            tool_calls.entry(call_id.clone()).or_insert_with(|| {
                                AggregatedToolCall {
                                    id: call_id.clone(),
                                    name,
                                    args: String::new(),
                                }
                            });
                            if let Some(item_id) = item.get("id").and_then(|v| v.as_str()) {
                                if item_id != call_id {
                                    item_id_to_call_id.insert(item_id.to_string(), call_id);
                                }
                            }
                        }
                    }
                }
            }
            "response.output_item.done" => {
                if let Some(item) = json.get("item") {
                    output_items.push(item.clone());
                    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !call_id.is_empty() {
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let args = item
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            tool_calls
                                .entry(call_id.clone())
                                .and_modify(|t| {
                                    if t.name.is_empty() {
                                        t.name = name.clone();
                                    }
                                    if t.args.is_empty() {
                                        t.args = args.clone();
                                    }
                                })
                                .or_insert(AggregatedToolCall {
                                    id: call_id,
                                    name,
                                    args,
                                });
                        }
                    }
                }
            }
            "response.function_call_arguments.delta" | "response.function_call.parameter_delta" => {
                let reference = json
                    .get("item_id")
                    .or_else(|| json.get("call_id"))
                    .or_else(|| json.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if reference.is_empty() {
                    return;
                }
                let call_id = if tool_calls.contains_key(&reference) {
                    Some(reference.clone())
                } else {
                    item_id_to_call_id.get(&reference).cloned()
                };
                if let Some(delta) = json.get("delta").and_then(|v| v.as_str()) {
                    if let Some(cid) = call_id {
                        tool_calls
                            .entry(cid.clone())
                            .or_insert(AggregatedToolCall {
                                id: cid,
                                name: String::new(),
                                args: String::new(),
                            })
                            .args
                            .push_str(delta);
                    }
                }
            }
            "response.completed" | "response.done" => {
                let resp = json.get("response").cloned();
                if let Some(r) = resp {
                    usage = r.get("usage").cloned().or(usage.clone());
                    status = r
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or(status.as_str())
                        .to_string();
                    completed_response = Some(r);
                } else {
                    usage = json.get("usage").cloned().or(usage.clone());
                }
            }
            "response.failed" => {
                upstream_error = Some(
                    json.get("response")
                        .and_then(|r| r.get("error"))
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("Upstream error")
                        .to_string(),
                );
            }
            _ => {}
        }
    };

    while let Some(item) = stream.next().await {
        let bytes = item.map_err(|e| format!("读取上游流失败：{}", e))?;
        let events = parser.feed(&bytes);
        for event in events {
            if !event.data.is_empty() {
                process_event_data(&event.data);
            }
        }
    }

    // 刷新解析器缓冲，处理未以换行结尾的最后一行
    for event in parser.feed(b"\n") {
        if !event.data.is_empty() {
            process_event_data(&event.data);
        }
    }

    let has_aggregated_payload =
        !text_out.is_empty() || !reasoning_out.is_empty() || !tool_calls.is_empty();

    if let Some(mut completed) = completed_response {
        if let Some(obj) = completed.as_object_mut() {
            let has_output = obj
                .get("output")
                .and_then(|v| v.as_array())
                .map(|arr| !arr.is_empty())
                .unwrap_or(false);

            if !has_output {
                obj.insert(
                    "output".to_string(),
                    serde_json::json!(build_responses_output_from_aggregated(
                        &output_items,
                        &reasoning_out,
                        &text_out,
                        &tool_calls
                    )),
                );
            }
            if !obj.contains_key("usage") {
                if let Some(u) = usage.clone() {
                    obj.insert("usage".to_string(), u);
                }
            }
        }
        return Ok(completed);
    }

    if upstream_error.is_some() && !has_aggregated_payload && output_items.is_empty() {
        return Err(upstream_error.unwrap_or_else(|| "上游响应失败".to_string()));
    }

    if !has_aggregated_payload && output_items.is_empty() {
        return Err("上游流未返回可聚合的响应内容".to_string());
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    Ok(serde_json::json!({
        "id": format!("resp_{}", now),
        "object": "response",
        "created_at": now,
        "status": status,
        "model": fallback_model.unwrap_or(""),
        "output": build_responses_output_from_aggregated(
            &output_items,
            &reasoning_out,
            &text_out,
            &tool_calls
        ),
        "usage": usage.unwrap_or_else(|| serde_json::json!({
            "input_tokens": 0_u64,
            "output_tokens": 0_u64,
            "total_tokens": 0_u64
        })),
    }))
}

/// 解析单个 Responses 流式事件
pub fn parse_responses_event(
    json: &serde_json::Value,
) -> Result<Option<CanonicalStreamEvent>, String> {
    let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "response.text.delta" | "response.output_text.delta" => {
            if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                if !delta.is_empty() {
                    return Ok(Some(CanonicalStreamEvent::TextDelta {
                        text: delta.to_string(),
                    }));
                }
            }
            Ok(None)
        }
        "response.reasoning.delta"
        | "response.reasoning_text.delta"
        | "response.output_reasoning.delta" => {
            if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                if !delta.is_empty() {
                    return Ok(Some(CanonicalStreamEvent::ReasoningDelta {
                        text: delta.to_string(),
                    }));
                }
            }
            Ok(None)
        }
        "response.function_call.parameter_delta" => {
            let call_id = json
                .get("call_id")
                .or_else(|| json.get("id"))
                .or_else(|| json.get("item_id"))
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                if !delta.is_empty() {
                    return Ok(Some(CanonicalStreamEvent::ToolInputDelta {
                        tool_use_id: call_id,
                        index: json.get("index").and_then(|v| v.as_u64()),
                        input_chunk: delta.to_string(),
                    }));
                }
            }
            Ok(None)
        }
        "response.function_call_arguments.delta" => {
            let call_id = json
                .get("call_id")
                .or_else(|| json.get("id"))
                .or_else(|| json.get("item_id"))
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                if !delta.is_empty() {
                    return Ok(Some(CanonicalStreamEvent::ToolInputDelta {
                        tool_use_id: call_id,
                        index: json
                            .get("index")
                            .or_else(|| json.get("output_index"))
                            .and_then(|v| v.as_u64()),
                        input_chunk: delta.to_string(),
                    }));
                }
            }
            Ok(None)
        }
        "response.function_call.completed" => {
            let id = json
                .get("call_id")
                .or_else(|| json.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            let name = json
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let input = json
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            Ok(Some(CanonicalStreamEvent::ToolUseStart {
                id,
                index: json.get("index").and_then(|v| v.as_u64()),
                name,
                input,
            }))
        }
        "response.output_item.added" => {
            let item = json.get("item").unwrap_or(&serde_json::Value::Null);
            if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = item
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                return Ok(Some(CanonicalStreamEvent::ToolUseStart {
                    id,
                    index: json
                        .get("output_index")
                        .or_else(|| json.get("index"))
                        .and_then(|v| v.as_u64()),
                    name,
                    input,
                }));
            }
            Ok(None)
        }
        "response.output_item.done" => Ok(None),
        "response.output_item.delta" => {
            let item = json.get("item").unwrap_or(&serde_json::Value::Null);
            let item_type = item.get("type").and_then(|t| t.as_str());

            // 处理 message 类型的文本增量
            if item_type == Some("message") {
                if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                    if !delta.is_empty() {
                        return Ok(Some(CanonicalStreamEvent::TextDelta {
                            text: delta.to_string(),
                        }));
                    }
                }
                // 也检查 item 中的 content_delta
                if let Some(delta) = item.get("content_delta").and_then(|d| d.as_str()) {
                    if !delta.is_empty() {
                        return Ok(Some(CanonicalStreamEvent::TextDelta {
                            text: delta.to_string(),
                        }));
                    }
                }
                return Ok(None);
            }

            // 处理 function_call 类型的参数增量
            if item_type == Some("function_call") {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(delta) = item
                    .get("arguments_delta")
                    .or_else(|| item.get("arguments"))
                    .and_then(|d| d.as_str())
                {
                    if !delta.is_empty() {
                        return Ok(Some(CanonicalStreamEvent::ToolInputDelta {
                            tool_use_id: call_id,
                            index: json
                                .get("output_index")
                                .or_else(|| json.get("index"))
                                .and_then(|v| v.as_u64()),
                            input_chunk: delta.to_string(),
                        }));
                    }
                }
            }
            Ok(None)
        }
        "response.done" | "response.completed" => {
            let mut stop_reason = Some(CanonicalStopReason::EndTurn);

            // Extract stop reason from output
            let response = json.get("response").unwrap_or(json);
            if let Some(sr) = response.get("stop_reason").and_then(|v| v.as_str()) {
                stop_reason = Some(match sr {
                    "tool_use" | "tool_calls" | "function_call" => CanonicalStopReason::ToolUse,
                    "max_output_tokens" | "max_tokens" => CanonicalStopReason::MaxTokens,
                    "stop_sequence" => CanonicalStopReason::StopSequence,
                    _ => CanonicalStopReason::EndTurn,
                });
            }
            let mut has_function_call = false;
            if let Some(output) = response.get("output").and_then(|o| o.as_array()) {
                for item in output {
                    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                        has_function_call = true;
                    }
                    if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                        if let Some(status) = item.get("status").and_then(|s| s.as_str()) {
                            if status == "incomplete" {
                                stop_reason = Some(CanonicalStopReason::MaxTokens);
                            }
                        }
                    }
                }
            }
            if has_function_call && stop_reason == Some(CanonicalStopReason::EndTurn) {
                stop_reason = Some(CanonicalStopReason::ToolUse);
            }

            if let Some(reason) = stop_reason {
                return Ok(Some(CanonicalStreamEvent::Stop { reason }));
            }

            Ok(Some(CanonicalStreamEvent::Raw {
                event: None,
                data: json.to_string(),
            }))
        }
        _ => Ok(Some(CanonicalStreamEvent::Raw {
            event: None,
            data: json.to_string(),
        })),
    }
}

// ---------------------------------------------------------------------------
// Output conversion helpers
// ---------------------------------------------------------------------------

/// 将规范流事件转换为 OpenAI SSE 输出
pub fn canonical_to_openai_sse(event: &CanonicalStreamEvent) -> Result<String> {
    match event {
        CanonicalStreamEvent::TextDelta { text } => Ok(format!(
            "data: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "id": "chatcmpl-temp",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "",
                "choices": [{
                    "index": 0,
                    "delta": { "content": text },
                    "finish_reason": null
                }]
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::ReasoningDelta { text } => Ok(format!(
            "data: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "id": "chatcmpl-temp",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "",
                "choices": [{
                    "index": 0,
                    "delta": { "reasoning_content": text },
                    "finish_reason": null
                }]
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::ToolUseStart {
            id,
            index,
            name,
            input,
        } => Ok(format!(
            "data: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "id": "chatcmpl-temp",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": index.unwrap_or(0),
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": tool_input_as_arguments(input)
                            }
                        }]
                    },
                    "finish_reason": null
                }]
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::ToolInputDelta {
            tool_use_id,
            index,
            input_chunk,
        } => Ok(format!(
            "data: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "id": "chatcmpl-temp",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": index.unwrap_or(0),
                            "id": if tool_use_id.is_empty() { serde_json::Value::Null } else { serde_json::json!(tool_use_id) },
                            "type": "function",
                            "function": { "arguments": input_chunk }
                        }]
                    },
                    "finish_reason": null
                }]
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::Stop { reason } => {
            let finish_reason = match reason {
                CanonicalStopReason::EndTurn => "stop",
                CanonicalStopReason::StopSequence => "stop",
                CanonicalStopReason::MaxTokens => "length",
                CanonicalStopReason::ToolUse => "tool_calls",
            };
            Ok(format!(
                "data: {}\n\ndata: [DONE]\n\n",
                serde_json::to_string(&serde_json::json!({
                    "id": "chatcmpl-temp",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "",
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": finish_reason
                    }]
                }))
                .map_err(|e| anyhow::anyhow!(e))?
            ))
        }
        CanonicalStreamEvent::Usage { .. } => Ok(String::new()),
        CanonicalStreamEvent::Raw { data, .. } if data == "[DONE]" => {
            Ok("data: [DONE]\n\n".to_string())
        }
        _ => Ok(String::new()),
    }
}

/// 将规范流事件转换为 Anthropic SSE 输出（无状态版本）
pub fn canonical_to_anthropic_sse(event: &CanonicalStreamEvent) -> Result<String> {
    match event {
        CanonicalStreamEvent::TextDelta { text } => Ok(format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": text }
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::ReasoningDelta { text } => Ok(format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "thinking_delta", "thinking": text }
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::ToolUseStart {
            id,
            index,
            name,
            input,
        } => Ok(format!(
            "event: content_block_start\ndata: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "type": "content_block_start",
                "index": index.unwrap_or(0),
                "content_block": {
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input
                }
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::ToolInputDelta {
            index, input_chunk, ..
        } => Ok(format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "type": "content_block_delta",
                "index": index.unwrap_or(0),
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": input_chunk
                }
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::Stop { reason } => {
            let stop_reason = match reason {
                CanonicalStopReason::EndTurn => "end_turn",
                CanonicalStopReason::StopSequence => "stop_sequence",
                CanonicalStopReason::MaxTokens => "max_tokens",
                CanonicalStopReason::ToolUse => "tool_use",
            };
            Ok(format!(
                "event: message_delta\ndata: {}\n\nevent: message_stop\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "message_delta",
                    "stop_reason": stop_reason,
                    "stop_sequence": null
                }))
                .map_err(|e| anyhow::anyhow!(e))?,
                serde_json::to_string(&serde_json::json!({
                    "type": "message_stop"
                }))
                .map_err(|e| anyhow::anyhow!(e))?
            ))
        }
        CanonicalStreamEvent::Usage { .. } => Ok(String::new()),
        _ => Ok(String::new()),
    }
}

/// Anthropic SSE 输出包装器：注入 message_start 和 content_block_start
///
/// Anthropic 客户端期望完整的事件序列：
/// message_start → content_block_start → content_block_delta → message_delta → message_stop
/// 但 canonical 事件流中只有 TextDelta/ReasoningDelta/Stop，没有显式的 start 事件。
/// 此包装器在第一个内容事件之前注入 message_start 和 content_block_start。
pub struct AnthropicSseWrapper {
    sent_message_start: bool,
    sent_content_start: bool,
    last_index: u64,
    stopped: bool,
    saw_tool_use: bool,
}

impl AnthropicSseWrapper {
    pub fn new() -> Self {
        Self {
            sent_message_start: false,
            sent_content_start: false,
            last_index: 0,
            stopped: false,
            saw_tool_use: false,
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// 转换单个事件，返回 SSE 字符串列表（可能包含多个事件）
    pub fn convert(&mut self, event: &CanonicalStreamEvent) -> Result<Vec<String>> {
        match event {
            CanonicalStreamEvent::TextDelta { text } => {
                let mut outputs = Vec::new();
                self.emit_headers("text", 0, &mut outputs)?;
                outputs.push(format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::to_string(&serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": { "type": "text_delta", "text": text }
                    }))
                    .map_err(|e| anyhow::anyhow!(e))?
                ));
                Ok(outputs)
            }
            CanonicalStreamEvent::ReasoningDelta { text } => {
                let mut outputs = Vec::new();
                self.emit_headers("thinking", 0, &mut outputs)?;
                outputs.push(format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::to_string(&serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": { "type": "thinking_delta", "thinking": text }
                    }))
                    .map_err(|e| anyhow::anyhow!(e))?
                ));
                Ok(outputs)
            }
            CanonicalStreamEvent::ToolUseStart {
                id,
                index,
                name,
                input,
            } => {
                self.saw_tool_use = true;
                let mut outputs = Vec::new();
                self.emit_message_start_if_needed(&mut outputs)?;
                let idx = index.unwrap_or(0);
                outputs.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    serde_json::to_string(&serde_json::json!({
                        "type": "content_block_start",
                        "index": idx,
                        "content_block": {
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input
                        }
                    }))
                    .map_err(|e| anyhow::anyhow!(e))?
                ));
                self.sent_content_start = true;
                self.last_index = idx;
                Ok(outputs)
            }
            CanonicalStreamEvent::ToolInputDelta {
                index, input_chunk, ..
            } => {
                self.saw_tool_use = true;
                Ok(vec![format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::to_string(&serde_json::json!({
                        "type": "content_block_delta",
                        "index": index.unwrap_or(0),
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": input_chunk
                        }
                    }))
                    .map_err(|e| anyhow::anyhow!(e))?
                )])
            }
            CanonicalStreamEvent::Stop { reason } => {
                self.stopped = true;
                let effective_reason =
                    if *reason == CanonicalStopReason::EndTurn && self.saw_tool_use {
                        CanonicalStopReason::ToolUse
                    } else {
                        reason.clone()
                    };
                let stop_reason = match effective_reason {
                    CanonicalStopReason::EndTurn => "end_turn",
                    CanonicalStopReason::StopSequence => "stop_sequence",
                    CanonicalStopReason::MaxTokens => "max_tokens",
                    CanonicalStopReason::ToolUse => "tool_use",
                };
                Ok(vec![
                    // content_block_stop 必须在 message_delta 之前
                    format!(
                        "event: content_block_stop\ndata: {}\n\n",
                        serde_json::to_string(&serde_json::json!({
                            "type": "content_block_stop",
                            "index": self.last_index
                        }))
                        .map_err(|e| anyhow::anyhow!(e))?
                    ),
                    // message_delta
                    format!(
                        "event: message_delta\ndata: {}\n\n",
                        serde_json::to_string(&serde_json::json!({
                            "type": "message_delta",
                            "delta": {
                                "stop_reason": stop_reason,
                                "stop_sequence": null
                            },
                            "usage": {
                                "output_tokens": 0
                            }
                        }))
                        .map_err(|e| anyhow::anyhow!(e))?
                    ),
                    // message_stop
                    format!(
                        "event: message_stop\ndata: {}\n\n",
                        serde_json::to_string(&serde_json::json!({
                            "type": "message_stop"
                        }))
                        .map_err(|e| anyhow::anyhow!(e))?
                    ),
                ])
            }
            CanonicalStreamEvent::Usage { .. } => Ok(Vec::new()),
            _ => Ok(Vec::new()),
        }
    }

    fn emit_headers(
        &mut self,
        block_type: &'static str,
        index: u64,
        outputs: &mut Vec<String>,
    ) -> Result<()> {
        self.emit_message_start_if_needed(outputs)?;
        if !self.sent_content_start {
            outputs.push(format!(
                "event: content_block_start\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": block_type
                    }
                }))
                .map_err(|e| anyhow::anyhow!(e))?
            ));
            self.sent_content_start = true;
            self.last_index = index;
        }
        Ok(())
    }

    fn emit_message_start_if_needed(&mut self, outputs: &mut Vec<String>) -> Result<()> {
        if !self.sent_message_start {
            outputs.push(format!(
                "event: message_start\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_001",
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": "",
                        "stop_sequence": null,
                        "usage": {
                            "input_tokens": 0,
                            "output_tokens": 0
                        }
                    }
                }))
                .map_err(|e| anyhow::anyhow!(e))?
            ));
            self.sent_message_start = true;
        }
        Ok(())
    }
}

/// 将规范流事件转换为 Responses API SSE 输出
pub fn canonical_to_responses_sse(event: &CanonicalStreamEvent) -> Result<String> {
    match event {
        CanonicalStreamEvent::TextDelta { text } => Ok(format!(
            "data: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "event": "response.output_text.delta",
                "type": "response.output_text.delta",
                "delta": text,
                "index": 0
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::ReasoningDelta { text } => Ok(format!(
            "data: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "event": "response.reasoning.delta",
                "type": "response.reasoning.delta",
                "delta": text,
                "index": 0
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::ToolUseStart {
            id, name, input, ..
        } => Ok(format!(
            "data: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "event": "response.function_call.completed",
                "type": "response.function_call.completed",
                "id": id,
                "name": name,
                "arguments": tool_input_as_arguments(input)
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::ToolInputDelta {
            tool_use_id,
            index,
            input_chunk,
        } => Ok(format!(
            "data: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "event": "response.function_call.parameter_delta",
                "type": "response.function_call.parameter_delta",
                "id": tool_use_id,
                "index": index,
                "delta": input_chunk
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        CanonicalStreamEvent::Stop { reason } => {
            let stop_reason = match reason {
                CanonicalStopReason::EndTurn => "end_turn",
                CanonicalStopReason::StopSequence => "stop_sequence",
                CanonicalStopReason::MaxTokens => "max_output_tokens",
                CanonicalStopReason::ToolUse => "tool_calls",
            };
            Ok(format!(
                "data: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "event": "response.done",
                    "type": "response.done",
                    "response": {},
                    "done": true,
                    "stop_reason": stop_reason
                }))
                .map_err(|e| anyhow::anyhow!(e))?
            ))
        }
        CanonicalStreamEvent::Usage {
            input_tokens,
            output_tokens,
        } => Ok(format!(
            "data: {}\n\n",
            serde_json::to_string(&serde_json::json!({
                "event": "response.done",
                "type": "response.done",
                "response": {},
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens
                }
            }))
            .map_err(|e| anyhow::anyhow!(e))?
        )),
        _ => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_parser_basic() {
        let mut parser = SseParser::new();
        let data = b"data: {\"test\": true}\n\n";
        let events = parser.feed(data);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, r#"{"test": true}"#);
        assert!(events[0].event.is_none());
    }

    #[test]
    fn test_sse_parser_with_event() {
        let mut parser = SseParser::new();
        let data = b"event: content_block_delta\ndata: {\"text\": \"hello\"}\n\n";
        let events = parser.feed(data);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, Some("content_block_delta".to_string()));
    }

    #[test]
    fn test_sse_parser_incomplete_line_across_chunks() {
        let mut parser = SseParser::new();

        // First chunk: data line split across TCP boundary (no trailing newline)
        let chunk1 = b"data: {\"choices\":[{\"delta\":{\"content\":\"";
        let events1 = parser.feed(chunk1);
        // Incomplete line should NOT be emitted as an event
        assert_eq!(events1.len(), 0);

        // Second chunk: completes the data line with newline
        let chunk2 = b"Hello\"}}]}\n\n";
        let events2 = parser.feed(chunk2);
        assert_eq!(events2.len(), 1);
        assert_eq!(
            events2[0].data,
            r#"{"choices":[{"delta":{"content":"Hello"}}]}"#
        );
    }

    #[test]
    fn test_sse_parser_event_header_split_across_chunks() {
        let mut parser = SseParser::new();

        // First chunk: event header split
        let chunk1 = b"event: content_block";
        let events1 = parser.feed(chunk1);
        assert_eq!(events1.len(), 0);

        // Second chunk: completes event header + data line
        let chunk2 = b"_delta\ndata: {\"text\": \"hi\"}\n\n";
        let events2 = parser.feed(chunk2);
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].event, Some("content_block_delta".to_string()));
        assert_eq!(events2[0].data, r#"{"text": "hi"}"#);
    }

    #[test]
    fn test_canonical_to_openai_sse_text() {
        let event = CanonicalStreamEvent::TextDelta {
            text: "Hello".to_string(),
        };
        let output = canonical_to_openai_sse(&event).unwrap();
        assert!(output.starts_with("data:"));
        assert!(output.contains("Hello"));
    }

    #[test]
    fn test_canonical_to_openai_sse_reasoning_content() {
        let event = CanonicalStreamEvent::ReasoningDelta {
            text: "thinking".to_string(),
        };
        let output = canonical_to_openai_sse(&event).unwrap();
        assert!(output.starts_with("data:"));
        assert!(output.contains("reasoning_content"));
        assert!(output.contains("thinking"));
    }

    #[test]
    fn test_canonical_to_openai_sse_tool_call() {
        let start = CanonicalStreamEvent::ToolUseStart {
            id: "call_1".to_string(),
            index: Some(1),
            name: "search".to_string(),
            input: serde_json::json!({"q":"rust"}),
        };
        let start_out = canonical_to_openai_sse(&start).unwrap();
        assert!(start_out.contains("\"tool_calls\""));
        assert!(start_out.contains("\"name\":\"search\""));
        assert!(start_out.contains("\"id\":\"call_1\""));

        let delta = CanonicalStreamEvent::ToolInputDelta {
            tool_use_id: "call_1".to_string(),
            index: Some(1),
            input_chunk: "\"q\":\"r\"".to_string(),
        };
        let delta_out = canonical_to_openai_sse(&delta).unwrap();
        assert!(delta_out.contains("\"tool_calls\""));
        assert!(delta_out.contains("\"arguments\":\"\\\"q\\\":\\\"r\\\"\""));
    }

    #[test]
    fn test_canonical_to_anthropic_sse_text() {
        let event = CanonicalStreamEvent::TextDelta {
            text: "Hello".to_string(),
        };
        let output = canonical_to_anthropic_sse(&event).unwrap();
        assert!(output.contains("content_block_delta"));
        assert!(output.contains("Hello"));
    }

    #[test]
    fn test_canonical_to_anthropic_sse_tool_call() {
        let start = CanonicalStreamEvent::ToolUseStart {
            id: "toolu_1".to_string(),
            index: Some(2),
            name: "search".to_string(),
            input: serde_json::json!({"q":"rust"}),
        };
        let start_out = canonical_to_anthropic_sse(&start).unwrap();
        assert!(start_out.contains("content_block_start"));
        assert!(start_out.contains("\"type\":\"tool_use\""));
        assert!(start_out.contains("\"index\":2"));

        let delta = CanonicalStreamEvent::ToolInputDelta {
            tool_use_id: "toolu_1".to_string(),
            index: Some(2),
            input_chunk: "\"q\":\"r\"".to_string(),
        };
        let delta_out = canonical_to_anthropic_sse(&delta).unwrap();
        assert!(delta_out.contains("content_block_delta"));
        assert!(delta_out.contains("\"type\":\"input_json_delta\""));
    }

    #[test]
    fn test_parse_openai_event_text() {
        let json = serde_json::json!({
            "choices": [{
                "delta": { "content": "Hello" }
            }]
        });
        let event = parse_openai_event(&json).unwrap();
        match event {
            CanonicalStreamEvent::TextDelta { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn test_parse_openai_event_reasoning_content() {
        let json = serde_json::json!({
            "choices": [{
                "delta": { "reasoning_content": "thinking" }
            }]
        });
        let event = parse_openai_event(&json).unwrap();
        match event {
            CanonicalStreamEvent::ReasoningDelta { text } => assert_eq!(text, "thinking"),
            _ => panic!("expected ReasoningDelta"),
        }
    }

    #[test]
    fn test_parse_openai_event_stop() {
        let json = serde_json::json!({
            "choices": [{
                "finish_reason": "stop"
            }]
        });
        let event = parse_openai_event(&json).unwrap();
        match event {
            CanonicalStreamEvent::Stop { reason } => {
                assert_eq!(reason, CanonicalStopReason::EndTurn);
            }
            _ => panic!("expected Stop"),
        }
    }

    #[test]
    fn test_parse_openai_event_tool_call() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "id": "call_123",
                        "function": { "name": "search", "arguments": "" }
                    }]
                }
            }]
        });
        let event = parse_openai_event(&json).unwrap();
        match event {
            CanonicalStreamEvent::ToolUseStart { id, name, .. } => {
                assert_eq!(id, "call_123");
                assert_eq!(name, "search");
            }
            _ => panic!("expected ToolUseStart"),
        }
    }

    #[test]
    fn test_parse_openai_event_tool_call_with_arguments_in_same_chunk() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "id": "call_123",
                        "index": 0,
                        "function": { "name": "search", "arguments": "{\"q\":\"test\"}" }
                    }]
                }
            }]
        });
        let event = parse_openai_event(&json).unwrap();
        match event {
            CanonicalStreamEvent::ToolUseStart {
                id, name, input, ..
            } => {
                assert_eq!(id, "call_123");
                assert_eq!(name, "search");
                assert_eq!(input, serde_json::json!({"q":"test"}));
            }
            _ => panic!("expected ToolUseStart with parsed input"),
        }
    }

    #[test]
    fn test_parse_openai_event_tool_input_delta() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "id": "call_123",
                        "function": { "arguments": "{\"q\": \"test\"}" }
                    }]
                }
            }]
        });
        let event = parse_openai_event(&json).unwrap();
        match event {
            CanonicalStreamEvent::ToolInputDelta {
                tool_use_id,
                input_chunk,
                ..
            } => {
                assert_eq!(tool_use_id, "call_123");
                assert_eq!(input_chunk, "{\"q\": \"test\"}");
            }
            _ => panic!("expected ToolInputDelta"),
        }
    }

    #[test]
    fn test_parse_openai_event_usage() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20
            }
        });
        let event = parse_openai_event(&json).unwrap();
        match event {
            CanonicalStreamEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                assert_eq!(input_tokens, 10);
                assert_eq!(output_tokens, 20);
            }
            _ => panic!("expected Usage"),
        }
    }

    #[test]
    fn test_parse_anthropic_event_text_delta() {
        let json = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": "Hello" }
        });
        let event = parse_anthropic_event(Some("content_block_delta"), &json).unwrap();
        match event {
            Some(CanonicalStreamEvent::TextDelta { text }) => assert_eq!(text, "Hello"),
            _ => panic!("expected Some(TextDelta)"),
        }
    }

    #[test]
    fn test_parse_anthropic_event_thinking_delta() {
        let json = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "thinking" }
        });
        let event = parse_anthropic_event(Some("content_block_delta"), &json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ReasoningDelta { text }) => assert_eq!(text, "thinking"),
            _ => panic!("expected Some(ReasoningDelta)"),
        }
    }

    #[test]
    fn test_parse_anthropic_event_stop() {
        let json = serde_json::json!({
            "type": "message_delta",
            "stop_reason": "end_turn"
        });
        let event = parse_anthropic_event(Some("message_delta"), &json).unwrap();
        match event {
            Some(CanonicalStreamEvent::Stop { reason }) => {
                assert_eq!(reason, CanonicalStopReason::EndTurn);
            }
            _ => panic!("expected Some(Stop)"),
        }
    }

    #[test]
    fn test_parse_anthropic_event_tool_use() {
        let json = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_123",
                "name": "search",
                "input": { "query": "test" }
            }
        });
        let event = parse_anthropic_event(Some("content_block_start"), &json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ToolUseStart { id, name, .. }) => {
                assert_eq!(id, "toolu_123");
                assert_eq!(name, "search");
            }
            _ => panic!("expected Some(ToolUseStart)"),
        }
    }

    #[test]
    fn test_parse_anthropic_event_skip_ping() {
        let json = serde_json::json!({});
        let event = parse_anthropic_event(Some("ping"), &json).unwrap();
        assert!(event.is_none());
    }

    #[test]
    fn test_parse_anthropic_event_input_json_delta() {
        let json = serde_json::json!({
            "type": "content_block_delta",
            "index": 2,
            "delta": { "type": "input_json_delta", "partial_json": "\"query\"" }
        });
        let event = parse_anthropic_event(Some("content_block_delta"), &json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ToolInputDelta {
                tool_use_id,
                input_chunk,
                ..
            }) => {
                assert_eq!(tool_use_id, "tool_2");
                assert_eq!(input_chunk, "\"query\"");
            }
            _ => panic!("expected Some(ToolInputDelta)"),
        }
    }

    #[test]
    fn test_parse_responses_event_text_delta() {
        let json = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "Hello"
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::TextDelta { text }) => assert_eq!(text, "Hello"),
            _ => panic!("expected Some(TextDelta)"),
        }
    }

    #[test]
    fn test_parse_responses_event_reasoning_delta() {
        let json = serde_json::json!({
            "type": "response.reasoning.delta",
            "delta": "thinking"
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ReasoningDelta { text }) => assert_eq!(text, "thinking"),
            _ => panic!("expected Some(ReasoningDelta)"),
        }
    }

    #[test]
    fn test_canonical_to_responses_sse_tool_call() {
        let start = CanonicalStreamEvent::ToolUseStart {
            id: "call_1".to_string(),
            index: Some(0),
            name: "search".to_string(),
            input: serde_json::json!({"q":"rust"}),
        };
        let start_out = canonical_to_responses_sse(&start).unwrap();
        assert!(start_out.contains("response.function_call.completed"));
        assert!(start_out.contains("\"name\":\"search\""));

        let delta = CanonicalStreamEvent::ToolInputDelta {
            tool_use_id: "call_1".to_string(),
            index: Some(0),
            input_chunk: "{\"q\":\"".to_string(),
        };
        let delta_out = canonical_to_responses_sse(&delta).unwrap();
        assert!(delta_out.contains("response.function_call.parameter_delta"));
        assert!(delta_out.contains("\"id\":\"call_1\""));
    }

    #[test]
    fn test_parse_responses_event_function_call_completed() {
        let json = serde_json::json!({
            "type": "response.function_call.completed",
            "id": "call_123",
            "name": "search",
            "arguments": "{\"query\": \"test\"}"
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ToolUseStart { id, name, .. }) => {
                assert_eq!(id, "call_123");
                assert_eq!(name, "search");
            }
            _ => panic!("expected Some(ToolUseStart)"),
        }
    }

    #[test]
    fn test_parse_responses_event_parameter_delta() {
        let json = serde_json::json!({
            "type": "response.function_call.parameter_delta",
            "id": "call_123",
            "delta": "{\"query\": \""
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ToolInputDelta {
                tool_use_id,
                input_chunk,
                ..
            }) => {
                assert_eq!(tool_use_id, "call_123");
                assert_eq!(input_chunk, "{\"query\": \"");
            }
            _ => panic!("expected Some(ToolInputDelta)"),
        }
    }

    #[test]
    fn test_parse_responses_event_function_call_arguments_delta() {
        let json = serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "call_id": "call_456",
            "output_index": 1,
            "delta": "{\"city\":\"Sh"
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ToolInputDelta {
                tool_use_id,
                index,
                input_chunk,
            }) => {
                assert_eq!(tool_use_id, "call_456");
                assert_eq!(index, Some(1));
                assert_eq!(input_chunk, "{\"city\":\"Sh");
            }
            _ => panic!("expected Some(ToolInputDelta)"),
        }
    }

    #[test]
    fn test_parse_responses_event_output_item_added_function_call() {
        let json = serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "call_id": "call_789",
                "name": "get_weather",
                "arguments": "{\"city\":\"Shanghai\"}"
            }
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ToolUseStart {
                id,
                index,
                name,
                input,
            }) => {
                assert_eq!(id, "call_789");
                assert_eq!(index, Some(0));
                assert_eq!(name, "get_weather");
                assert_eq!(input, serde_json::json!("{\"city\":\"Shanghai\"}"));
            }
            _ => panic!("expected Some(ToolUseStart)"),
        }
    }

    #[test]
    fn test_parse_responses_event_prefers_call_id_over_internal_id() {
        let json = serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "fc_abc",
                "call_id": "call_xyz",
                "name": "get_weather",
                "arguments": "{\"city\":\"Tokyo\"}"
            }
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ToolUseStart { id, .. }) => {
                assert_eq!(id, "call_xyz");
            }
            _ => panic!("expected Some(ToolUseStart)"),
        }
    }

    #[test]
    fn test_parse_responses_event_output_item_delta_function_call() {
        let json = serde_json::json!({
            "type": "response.output_item.delta",
            "output_index": 2,
            "item": {
                "type": "function_call",
                "id": "call_999",
                "arguments_delta": "\"city\":\"Shang"
            }
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::ToolInputDelta {
                tool_use_id,
                index,
                input_chunk,
            }) => {
                assert_eq!(tool_use_id, "call_999");
                assert_eq!(index, Some(2));
                assert_eq!(input_chunk, "\"city\":\"Shang");
            }
            _ => panic!("expected Some(ToolInputDelta)"),
        }
    }

    #[test]
    fn test_parse_responses_event_done() {
        let json = serde_json::json!({
            "type": "response.done",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20
            }
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::Stop { reason }) => {
                assert_eq!(reason, CanonicalStopReason::EndTurn);
            }
            _ => panic!("expected Some(Stop)"),
        }
    }

    #[test]
    fn test_parse_responses_event_completed_with_function_call_is_tool_use() {
        let json = serde_json::json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "output": [
                    {
                        "type": "function_call",
                        "call_id": "call_1",
                        "name": "hello_tool",
                        "arguments": "{\"name\":\"world\"}"
                    }
                ]
            }
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::Stop { reason }) => {
                assert_eq!(reason, CanonicalStopReason::ToolUse);
            }
            _ => panic!("expected Some(Stop)"),
        }
    }

    #[test]
    fn test_parse_responses_event_output_item_done_ignored() {
        let json = serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "hello_tool",
                "arguments": "{\"name\":\"world\"}"
            }
        });
        let event = parse_responses_event(&json).unwrap();
        assert!(event.is_none());
    }

    #[test]
    fn test_parse_responses_event_unknown_type() {
        let json = serde_json::json!({
            "type": "response.created",
            "id": "resp_123"
        });
        let event = parse_responses_event(&json).unwrap();
        match event {
            Some(CanonicalStreamEvent::Raw { data, .. }) => {
                assert!(data.contains("resp_123"));
            }
            _ => panic!("expected Some(Raw)"),
        }
    }

    #[test]
    fn test_openai_done_marker() {
        let sse = SseEvent {
            event: None,
            data: "[DONE]".to_string(),
        };
        let event = convert_openai_event(&sse).unwrap();
        match event {
            CanonicalStreamEvent::Raw { data, .. } => assert_eq!(data, "[DONE]"),
            _ => panic!("expected Raw [DONE]"),
        }
    }

    #[tokio::test]
    async fn test_drain_responses_sse_captures_completed_response() {
        let completed = serde_json::json!({
            "id": "resp_x",
            "object": "response",
            "status": "completed",
            "output": [{"type":"message","content":[{"type":"output_text","text":"hi"}]}],
            "usage": {"input_tokens": 3, "output_tokens": 1}
        });
        let chunks = vec![
            br#"event: response.created
data: {"response":{"id":"resp_x"}}

"#
            .to_vec(),
            br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"h"}

"#
            .to_vec(),
            br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"i"}

"#
            .to_vec(),
            format!(
                "event: response.completed\ndata: {}\n\n",
                serde_json::json!({"type":"response.completed","response": completed})
            )
            .into_bytes(),
        ];

        let out = drain_responses_sse_bytes(
            futures::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>)),
            Some("gpt-5.5"),
        )
        .await
        .unwrap();
        assert_eq!(out["id"], "resp_x");
        assert_eq!(out["status"], "completed");
        assert_eq!(out["usage"]["input_tokens"], 3);
        assert_eq!(out["output"][0]["content"][0]["text"], "hi");
    }

    #[tokio::test]
    async fn test_drain_responses_sse_flushes_final_unterminated_line() {
        let completed = serde_json::json!({
            "response": {
                "id": "resp_short",
                "status": "completed",
                "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }
        });
        let chunks = vec![
            br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"ok"}

"#
            .to_vec(),
            format!(
                "event: response.completed\ndata: {}",
                serde_json::json!({
                    "type":"response.completed",
                    "response": completed["response"]
                })
            )
            .into_bytes(),
        ];

        let out = drain_responses_sse_bytes(
            futures::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>)),
            Some("gpt-5.5"),
        )
        .await
        .unwrap();
        assert_eq!(out["id"], "resp_short");
        assert_eq!(out["status"], "completed");
        assert_eq!(out["output"][0]["content"][0]["text"], "ok");
    }

    #[tokio::test]
    async fn test_drain_responses_sse_item_id_differs_from_call_id() {
        let chunks = vec![
            br#"event: response.output_item.added
data: {"type":"response.output_item.added","item":{"id":"fc_abc","call_id":"call_xyz","type":"function_call","name":"get_weather"}}

"#
            .to_vec(),
            br#"event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","item_id":"fc_abc","delta":"{\"city\":\""}

"#
            .to_vec(),
            br#"event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","item_id":"fc_abc","delta":"Tokyo\"}"}

"#
            .to_vec(),
            br#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"id":"fc_abc","call_id":"call_xyz","type":"function_call","name":"get_weather","arguments":"{\"city\":\"Tokyo\"}"}}

"#
            .to_vec(),
            br#"event: response.completed
data: {"type":"response.completed","response":{"status":"completed"}}

"#
            .to_vec(),
        ];

        let out = drain_responses_sse_bytes(
            futures::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>)),
            Some("gpt-5.5"),
        )
        .await
        .unwrap();
        let output = out["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["call_id"], "call_xyz");
        assert_eq!(output[0]["arguments"], "{\"city\":\"Tokyo\"}");
    }

    #[tokio::test]
    async fn test_drain_responses_sse_aggregates_reasoning_and_tool_args_without_completed() {
        let chunks = vec![
            br#"event: response.reasoning_summary_text.delta
data: {"type":"response.reasoning_summary_text.delta","delta":"think "}

"#
            .to_vec(),
            br#"event: response.reasoning_summary_text.delta
data: {"type":"response.reasoning_summary_text.delta","delta":"more"}

"#
            .to_vec(),
            br#"event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"c1","name":"do_thing"}}

"#
            .to_vec(),
            br#"event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","item_id":"c1","delta":"{\"a\":"}

"#
            .to_vec(),
            br#"event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","item_id":"c1","delta":"1}"}

"#
            .to_vec(),
        ];

        let out = drain_responses_sse_bytes(
            futures::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>)),
            Some("gpt-5.5"),
        )
        .await
        .unwrap();
        let output = out["output"].as_array().unwrap();
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["text"], "think more");
        assert_eq!(output[1]["type"], "function_call");
        assert_eq!(output[1]["name"], "do_thing");
        assert_eq!(output[1]["arguments"], "{\"a\":1}");
    }

    #[tokio::test]
    async fn test_drain_responses_sse_surfaces_failed_error() {
        let chunks = vec![br#"event: response.failed
data: {"type":"response.failed","response":{"error":{"message":"boom"}}}

"#
        .to_vec()];

        let err = drain_responses_sse_bytes(
            futures::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>)),
            Some("gpt-5.5"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("boom"));
    }
}
