//! 协议转换层 — 将不同协议归一化为 canonical 表示，再做双向转换

pub mod canonical;
pub mod image;
pub mod stream;

pub use canonical::*;
pub use image::ImageDownloader;
pub use stream::*;

use anyhow::Result;
use futures::{Stream, StreamExt};
use std::pin::Pin;

/// 协议类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

impl Protocol {
    /// 从端点路径解析协议类型
    pub fn from_endpoint(path: &str) -> Self {
        match path {
            "/v1/chat/completions" => Protocol::ChatCompletions,
            "/v1/responses" => Protocol::Responses,
            "/v1/messages" => Protocol::AnthropicMessages,
            _ => Protocol::ChatCompletions,
        }
    }

    /// 转换为上游路径
    pub fn to_upstream_path(&self) -> &'static str {
        match self {
            Protocol::ChatCompletions => "/v1/chat/completions",
            Protocol::Responses => "/v1/responses",
            Protocol::AnthropicMessages => "/v1/messages",
        }
    }

    /// 协议选择优先级：入口协议优先，其次其他
    pub fn selection_priority(entry: Protocol) -> [Protocol; 3] {
        match entry {
            Protocol::ChatCompletions => [
                Protocol::ChatCompletions,
                Protocol::Responses,
                Protocol::AnthropicMessages,
            ],
            Protocol::Responses => [
                Protocol::Responses,
                Protocol::ChatCompletions,
                Protocol::AnthropicMessages,
            ],
            Protocol::AnthropicMessages => [
                Protocol::AnthropicMessages,
                Protocol::ChatCompletions,
                Protocol::Responses,
            ],
        }
    }
}

/// 将协议特定请求转为 canonical 格式
pub fn request_to_canonical(
    protocol: Protocol,
    body: &serde_json::Value,
) -> Result<CanonicalRequest> {
    match protocol {
        Protocol::ChatCompletions => chat_completions::to_canonical_request(body),
        Protocol::Responses => responses::to_canonical_request(body),
        Protocol::AnthropicMessages => anthropic::to_canonical_request(body),
    }
}

/// 将 canonical 格式转为协议特定请求
pub fn canonical_to_request(
    protocol: Protocol,
    canonical: &CanonicalRequest,
) -> Result<serde_json::Value> {
    match protocol {
        Protocol::ChatCompletions => chat_completions::from_canonical_request(canonical),
        Protocol::Responses => responses::from_canonical_request(canonical),
        Protocol::AnthropicMessages => anthropic::from_canonical_request(canonical),
    }
}

/// 将协议特定响应转为 canonical 格式
pub fn response_to_canonical(
    protocol: Protocol,
    body: &serde_json::Value,
) -> Result<CanonicalResponse> {
    match protocol {
        Protocol::ChatCompletions => chat_completions::to_canonical_response(body),
        Protocol::Responses => responses::to_canonical_response(body),
        Protocol::AnthropicMessages => anthropic::to_canonical_response(body),
    }
}

/// 将 canonical 格式转为协议特定响应
pub fn canonical_to_response(
    protocol: Protocol,
    canonical: &CanonicalResponse,
) -> Result<serde_json::Value> {
    match protocol {
        Protocol::ChatCompletions => chat_completions::from_canonical_response(canonical),
        Protocol::Responses => responses::from_canonical_response(canonical),
        Protocol::AnthropicMessages => anthropic::from_canonical_response(canonical),
    }
}

/// 完整的请求转换：from → canonical → to
pub fn convert_request(
    from: Protocol,
    to: Protocol,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut canonical = request_to_canonical(from, body)?;
    detect_unmapped_fields(from, body, &mut canonical);
    if !canonical.unmapped.is_empty() {
        tracing::warn!("请求包含无法映射到目标协议的字段: {:?}", canonical.unmapped);
    }
    canonical_to_request(to, &canonical)
}

/// 检测无法映射的字段
/// 各协议有各自的已知字段集，canonical 无法表示的字段会被记录
fn detect_unmapped_fields(
    from: Protocol,
    body: &serde_json::Value,
    canonical: &mut CanonicalRequest,
) {
    if let serde_json::Value::Object(ref map) = body {
        // 各协议已知字段（canonical 可以处理的）
        let known_fields: std::collections::HashSet<&str> = match from {
            Protocol::ChatCompletions => [
                "model",
                "messages",
                "temperature",
                "top_p",
                "max_tokens",
                "stop",
                "stream",
                "stream_options",
                "tools",
                "tool_choice",
                "n",
                "user",
                "response_format",
                // OpenAI 特有但 canonical 可以忽略的
                "frequency_penalty",
                "presence_penalty",
                "logit_bias",
                "logprobs",
                "top_logprobs",
                "modalities",
                "prediction",
                "reasoning_effort",
                "reasoning",
                "service_tier",
                "parallel_tool_calls",
                "store",
                "metadata",
                "annotations",
                "prompt_tags",
            ]
            .into_iter()
            .collect(),
            Protocol::Responses => [
                "model",
                "input",
                "instructions",
                "temperature",
                "top_p",
                "max_output_tokens",
                "stop",
                "stream",
                "tools",
                "tool_choice",
                "truncation",
                "text",
                "output",
                "user",
                "metadata",
                "prompt_tags",
                "store",
                "replay",
                "reasoning",
                "reasoning_effort",
            ]
            .into_iter()
            .collect(),
            Protocol::AnthropicMessages => [
                "model",
                "messages",
                "system",
                "temperature",
                "top_p",
                "max_tokens",
                "stop_sequences",
                "stream",
                "tools",
                "tool_choice",
                "metadata",
                "thinking",
                "output_config",
                "effort",
                "budget",
            ]
            .into_iter()
            .collect(),
        };

        for key in map.keys() {
            if !known_fields.contains(key.as_str()) {
                canonical.unmapped.push(key.clone());
            }
        }
    }
}

/// 异步完整请求转换：from → canonical → to（不含图片下载）
/// 图片 URL 解析由调用方根据目标协议决定是否执行
pub async fn convert_request_with_images(
    from: Protocol,
    to: Protocol,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut canonical = request_to_canonical(from, body)?;
    detect_unmapped_fields(from, body, &mut canonical);
    if !canonical.unmapped.is_empty() {
        tracing::warn!("请求包含无法映射到目标协议的字段: {:?}", canonical.unmapped);
    }
    // 注意：不在这里无条件解析图片 URL。
    // 只有 Anthropic 目标需要 base64 图片；Chat Completions 和 Responses
    // 支持 image_url，保留 URL 即可。图片解析由调用方按需执行。
    canonical_to_request(to, &canonical)
}

/// 对已转换的请求体解析图片 URL（仅用于 Anthropic 目标）
/// 将 canonical 中的 Image::Url 下载为 Image::Base64
pub async fn resolve_images_for_anthropic(body: &serde_json::Value) -> Result<serde_json::Value> {
    let mut canonical = request_to_canonical(Protocol::AnthropicMessages, body)?;
    resolve_image_urls(&mut canonical).await?;
    canonical_to_request(Protocol::AnthropicMessages, &canonical)
}

/// 完整的响应转换：from → canonical → to
pub fn convert_response(
    from: Protocol,
    to: Protocol,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let canonical = response_to_canonical(from, body)?;
    canonical_to_response(to, &canonical)
}

/// 流式 SSE 转换：上游协议 SSE → canonical → 目标协议 SSE
///
/// 用于协议转换场景下的流式响应处理。接收上游原始字节流，
/// 解析为规范事件后转换为目标协议的 SSE 输出。
pub fn convert_stream_sse(
    from: Protocol,
    to: Protocol,
    stream: impl Stream<Item = Result<Vec<u8>, std::io::Error>> + Unpin + Send + 'static,
) -> Pin<Box<dyn Stream<Item = Result<actix_web::web::Bytes, actix_web::Error>> + Send + 'static>> {
    use stream::{canonical_to_openai_sse, canonical_to_responses_sse, parse_stream_events};

    let parsed = parse_stream_events(from, stream);

    // Anthropic 需要状态包装器（注入 content_block_start）
    if to == Protocol::AnthropicMessages {
        let wrapped = AnthropicStream::new(parsed);
        let out = wrapped.map(|item| match item {
            Ok(s) => Ok(actix_web::web::Bytes::from(s)),
            Err(e) => Err(actix_web::error::ErrorInternalServerError(e)),
        });
        Box::pin(out)
    } else {
        // 非 Anthropic：简单转换 + 过滤空字符串
        let out = parsed
            .map(move |item| match item {
                Ok(event) => match to {
                    Protocol::ChatCompletions => canonical_to_openai_sse(&event),
                    Protocol::Responses => canonical_to_responses_sse(&event),
                    Protocol::AnthropicMessages => unreachable!(),
                },
                Err(e) => Err(anyhow::anyhow!(e)),
            })
            .filter(|item| {
                futures::future::ready(match item {
                    Ok(s) => !s.is_empty(),
                    Err(_) => true,
                })
            })
            .map(|item| match item {
                Ok(s) => Ok(actix_web::web::Bytes::from(s)),
                Err(e) => Err(actix_web::error::ErrorInternalServerError(e)),
            });
        Box::pin(out)
    }
}

/// Anthropic 流式包装：使用 AnthropicSseWrapper 注入 content_block_start
struct AnthropicStream<S> {
    inner: S,
    wrapper: AnthropicSseWrapper,
    pending: Vec<Result<String, String>>,
}

impl<S> AnthropicStream<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            wrapper: AnthropicSseWrapper::new(),
            pending: Vec::new(),
        }
    }
}

impl<S> Stream for AnthropicStream<S>
where
    S: Stream<Item = Result<stream::CanonicalStreamEvent, String>> + Unpin + Send + 'static,
{
    type Item = Result<String, String>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();

        loop {
            // 先尝试从 pending 取数据
            if let Some(item) = this.pending.pop() {
                // 如果已停止且 pending 已空，返回 item 后下次返回 None
                if this.wrapper.is_stopped() && this.pending.is_empty() {
                    return std::task::Poll::Ready(Some(item));
                }
                return std::task::Poll::Ready(Some(item));
            }

            // pending 为空，检查是否已停止
            if this.wrapper.is_stopped() {
                return std::task::Poll::Ready(None);
            }

            // 从内层流获取下一个事件
            match futures::ready!(std::pin::Pin::new(&mut this.inner).poll_next(cx)) {
                Some(Ok(event)) => {
                    match this.wrapper.convert(&event) {
                        Ok(outputs) => {
                            // 反转后推入，保持顺序
                            for s in outputs.into_iter().rev() {
                                this.pending.push(Ok(s));
                            }
                            // 继续循环，从 pending 取数据
                        }
                        Err(e) => {
                            return std::task::Poll::Ready(Some(Err(e.to_string())));
                        }
                    }
                }
                Some(Err(e)) => return std::task::Poll::Ready(Some(Err(e))),
                None => return std::task::Poll::Ready(None),
            }
        }
    }
}

/// 异步：解析图片 URL 并转为 base64
pub async fn resolve_image_urls(canonical: &mut CanonicalRequest) -> Result<()> {
    let downloader = ImageDownloader::new();
    for message in &mut canonical.messages {
        for block in &mut message.content {
            if let CanonicalContentBlock::Image { source } = block {
                if let CanonicalImageSource::Url { ref url } = source {
                    let (base64_data, media_type) = downloader.download_to_base64(url).await?;
                    *source = CanonicalImageSource::Base64 {
                        media_type,
                        data: base64_data,
                    };
                }
            }
        }
    }
    Ok(())
}

// 子模块（Phase 3/4 实现）
mod anthropic;
mod chat_completions;
mod responses;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_from_endpoint() {
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
    fn test_protocol_to_upstream_path() {
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

    #[test]
    fn test_selection_priority() {
        let priority = Protocol::selection_priority(Protocol::ChatCompletions);
        assert_eq!(
            priority,
            [
                Protocol::ChatCompletions,
                Protocol::Responses,
                Protocol::AnthropicMessages
            ]
        );

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

    #[test]
    fn test_unmapped_fields_chat_completions() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "frequency_penalty": 0.5,
            "unknown_field": true
        });
        let mut canonical = request_to_canonical(Protocol::ChatCompletions, &body).unwrap();
        detect_unmapped_fields(Protocol::ChatCompletions, &body, &mut canonical);

        // frequency_penalty is in known fields list, should not be unmapped
        assert!(!canonical
            .unmapped
            .contains(&"frequency_penalty".to_string()));
        // unknown_field is not known, should be unmapped
        assert!(canonical.unmapped.contains(&"unknown_field".to_string()));
    }

    #[test]
    fn test_unmapped_fields_anthropic() {
        let body = serde_json::json!({
            "model": "claude-3",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100,
            "thinking": {"budget": 1024},
            "weird_custom_field": 42
        });
        let mut canonical = request_to_canonical(Protocol::AnthropicMessages, &body).unwrap();
        detect_unmapped_fields(Protocol::AnthropicMessages, &body, &mut canonical);

        // thinking is in known fields, silently dropped
        assert!(!canonical.unmapped.contains(&"thinking".to_string()));
        // weird_custom_field is unknown
        assert!(canonical
            .unmapped
            .contains(&"weird_custom_field".to_string()));
    }

    #[test]
    fn test_unmapped_fields_responses() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "input": "hello",
            "max_output_tokens": 100,
            "custom_beta_feature": true
        });
        let mut canonical = request_to_canonical(Protocol::Responses, &body).unwrap();
        detect_unmapped_fields(Protocol::Responses, &body, &mut canonical);

        assert!(!canonical
            .unmapped
            .contains(&"max_output_tokens".to_string()));
        assert!(canonical
            .unmapped
            .contains(&"custom_beta_feature".to_string()));
    }

    #[test]
    fn test_no_unmapped_when_all_known() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.7,
            "stream": true,
            "tools": []
        });
        let mut canonical = request_to_canonical(Protocol::ChatCompletions, &body).unwrap();
        detect_unmapped_fields(Protocol::ChatCompletions, &body, &mut canonical);
        assert!(canonical.unmapped.is_empty());
    }
}
