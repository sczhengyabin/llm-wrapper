use crate::models::ApiType;
use crate::oauth::AuthManager;
use crate::router::RouteResult;
use crate::transform::Protocol;
use futures::StreamExt;
use reqwest::Client;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

/// 调试信息
#[derive(Debug, Clone, serde::Serialize)]
pub struct DebugInfo {
    pub client_request: serde_json::Value,
    pub client_ip: String,
    pub client_url: String,
    pub endpoint: String,
    pub upstream_url: String,
    pub upstream_request: serde_json::Value,
    pub upstream_response: serde_json::Value,
}

/// 请求代理
pub struct Proxy {
    client: Client,
    auth_manager: AuthManager,
}

impl Proxy {
    pub fn new(auth_manager: AuthManager) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(1200))
                .build()
                .expect("无法创建 HTTP 客户端"),
            auth_manager,
        }
    }

    /// 构建上游请求（共享逻辑：路径变换、参数覆盖、认证）
    fn build_upstream_request(
        &self,
        route: &RouteResult,
        method: String,
        target_protocol: Protocol,
        body: &serde_json::Value,
    ) -> Result<(String, Vec<u8>, reqwest::RequestBuilder), String> {
        // 根据 api_type 和目标协议重写路径
        let upstream_path = transform_upstream_path(&route.api_type, target_protocol);

        // 构建上游 URL（根据目标协议选择有效 base URL）
        let effective_base_url = route.effective_base_url(target_protocol);
        let upstream_url = format!("{}{}", effective_base_url, upstream_path);

        debug!("代理请求到上游：{}", upstream_url);

        // 构建请求体
        let mut request_body = body.clone();
        // 应用参数覆盖
        apply_param_overrides_inner(&mut request_body, route);

        // ChatCompletions 直连 Codex 时，先转换为 Responses 形状
        if route.api_type == ApiType::ChatGptCodex && target_protocol == Protocol::ChatCompletions {
            transform_chat_to_responses(&mut request_body);
        }
        // Codex Responses 的协议要求：必须携带 instructions，且 store=false
        if route.api_type == ApiType::ChatGptCodex && target_protocol == Protocol::Responses {
            ensure_codex_responses_requirements(&mut request_body);
        }

        let request_body_bytes = serde_json::to_vec(&request_body).map_err(|e| e.to_string())?;

        // 构建请求
        let req_builder = self.client.request(
            method
                .parse::<reqwest::Method>()
                .map_err(|e| e.to_string())?,
            &upstream_url,
        );

        Ok((upstream_url, request_body_bytes, req_builder))
    }

    /// 构建上游请求（完整版：包含调试信息和流式标志）
    fn build_upstream_request_full(
        &self,
        route: &RouteResult,
        method: String,
        target_protocol: Protocol,
        body: &serde_json::Value,
    ) -> Result<
        (
            String,
            Vec<u8>,
            reqwest::RequestBuilder,
            serde_json::Value,
            bool,
        ),
        String,
    > {
        // 根据 api_type 和目标协议重写路径
        let upstream_path = transform_upstream_path(&route.api_type, target_protocol);

        // 构建上游 URL
        let effective_base_url = route.effective_base_url(target_protocol);
        let upstream_url = format!("{}{}", effective_base_url, upstream_path);

        debug!("代理请求到上游：{}", upstream_url);

        // 构建并修改请求体
        let mut request_body = body.clone();
        apply_param_overrides_inner(&mut request_body, route);
        if route.api_type == ApiType::ChatGptCodex && target_protocol == Protocol::ChatCompletions {
            transform_chat_to_responses(&mut request_body);
        }
        if route.api_type == ApiType::ChatGptCodex && target_protocol == Protocol::Responses {
            ensure_codex_responses_requirements(&mut request_body);
        }

        // 流式标志
        let is_stream = request_body
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // 序列化为字节
        let request_body_bytes = serde_json::to_vec(&request_body).map_err(|e| e.to_string())?;

        // 构建请求构建器
        let req_builder = self.client.request(
            method
                .parse::<reqwest::Method>()
                .map_err(|e| e.to_string())?,
            &upstream_url,
        );

        Ok((
            upstream_url,
            request_body_bytes,
            req_builder,
            request_body,
            is_stream,
        ))
    }

    /// 应用认证到请求构建器
    async fn apply_auth(
        &self,
        route: &RouteResult,
        mut req_builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        if let Some(access_token) = self
            .auth_manager
            .get_access_token(&route.upstream_name, &route.upstream_auth)
            .await
        {
            if !access_token.is_empty() && access_token != "none" {
                req_builder = req_builder.bearer_auth(&access_token);
            }
        }
        req_builder
    }

    fn apply_protocol_headers(
        target_protocol: Protocol,
        req_builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        if target_protocol == Protocol::AnthropicMessages {
            req_builder.header("anthropic-version", "2023-06-01")
        } else {
            req_builder
        }
    }

    /// 代理请求到上游（带调试）- 用于直接转发，不做协议转换
    pub async fn proxy_request_with_debug(
        &self,
        route: &RouteResult,
        method: String,
        target_protocol: Protocol,
        body: serde_json::Value,
        client_ip: String,
        client_url: String,
        debug_data: Option<Arc<RwLock<Option<DebugInfo>>>>,
        stream_hub: Option<Arc<tokio::sync::broadcast::Sender<String>>>,
    ) -> Result<actix_web::HttpResponse, String> {
        // 保存客户端原始请求
        let client_request = body.clone();

        let endpoint = target_protocol.to_upstream_path().to_string();

        // 构建上游请求
        let (upstream_url, request_body_bytes, req_builder, upstream_request, is_stream) =
            self.build_upstream_request_full(route, method, target_protocol, &body)?;
        let req_builder = self.apply_auth(route, req_builder).await;
        let req_builder = Self::apply_protocol_headers(target_protocol, req_builder);

        // 发送请求
        let response = req_builder
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request_body_bytes)
            .send()
            .await
            .map_err(|e| format!("上游请求失败：{}", e))?;

        // 读取响应
        let status = response.status();
        let headers = response.headers().clone();

        // 检查是否是不支持端点的错误（404/405）
        if status.as_u16() == 404 || status.as_u16() == 405 {
            let body_bytes = response
                .bytes()
                .await
                .map_err(|e| format!("读取响应失败：{}", e))?;
            let error_body = String::from_utf8_lossy(&body_bytes);
            return Err(format!("上游返回 {} - {}", status.as_u16(), error_body));
        }

        // 检查是否是流式响应
        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if is_stream || content_type.contains("text/event-stream") {
            // 流式响应 - 直接流式代理，同时通过 SSE 广播到前端
            use actix_web::body::BodyStream;

            // 保存初始调试数据（不包含响应内容）
            let initial_debug_info = DebugInfo {
                client_request: client_request.clone(),
                client_ip: client_ip.clone(),
                client_url: client_url.clone(),
                endpoint: endpoint.clone(),
                upstream_url: upstream_url.clone(),
                upstream_request: upstream_request.clone(),
                upstream_response: serde_json::Value::Null,
            };

            if let Some(ref debug_store) = debug_data {
                debug_store.write().await.replace(initial_debug_info);
            }

            // 获取 stream_hub 用于广播
            let stream_hub_clone = stream_hub.clone();

            // 流式代理，同时广播 chunk
            let stream = response.bytes_stream().map(move |item| {
                // 先广播到 SSE 前端（不持有 item 的引用）
                if let Ok(chunk) = &item {
                    if let Ok(text) = std::str::from_utf8(chunk) {
                        if let Some(ref hub) = stream_hub_clone {
                            let hub = hub.clone();
                            let text = text.to_string();
                            tokio::spawn(async move {
                                let _ = hub.send(text);
                            });
                        }
                    }
                }
                // 返回原始 item
                item.map(|chunk| chunk)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            });

            let mut resp_builder = actix_web::HttpResponse::build(
                actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap(),
            );
            resp_builder.content_type("text/event-stream");

            Ok(resp_builder.body(BodyStream::new(stream)))
        } else {
            // 普通响应
            let body_bytes = response
                .bytes()
                .await
                .map_err(|e| format!("读取响应失败：{}", e))?;

            // 保存上游响应（调试用）- 总是保存
            let upstream_response = serde_json::from_slice::<serde_json::Value>(&body_bytes)
                .unwrap_or(serde_json::Value::Null);

            let mut resp_builder = actix_web::HttpResponse::build(
                actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap(),
            );
            if let Some(ct) = headers.get(reqwest::header::CONTENT_TYPE) {
                resp_builder.content_type(ct.to_str().unwrap_or("application/json"));
            }

            let debug_info = DebugInfo {
                client_request,
                client_ip,
                client_url,
                endpoint,
                upstream_url,
                upstream_request,
                upstream_response,
            };

            // 保存调试数据
            if let Some(ref debug_store) = debug_data {
                debug_store.write().await.replace(debug_info.clone());
            }

            Ok(resp_builder.body(body_bytes.to_vec()))
        }
    }

    /// 代理请求并返回原始响应数据（用于协议转换场景，非流式）
    /// 返回 (upstream_url, status, headers, body)
    pub async fn proxy_request_raw(
        &self,
        route: &RouteResult,
        method: String,
        target_protocol: Protocol,
        body: serde_json::Value,
    ) -> Result<(String, u16, reqwest::header::HeaderMap, Vec<u8>), String> {
        // 构建上游请求
        let (upstream_url, request_body_bytes, req_builder) =
            self.build_upstream_request(route, method, target_protocol, &body)?;
        let req_builder = self.apply_auth(route, req_builder).await;
        let req_builder = Self::apply_protocol_headers(target_protocol, req_builder);

        // 发送请求
        let response = req_builder
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request_body_bytes)
            .send()
            .await
            .map_err(|e| format!("上游请求失败：{}", e))?;

        let status = response.status();
        let headers = response.headers().clone();

        // 检查是否是不支持端点的错误（404/405）
        if status.as_u16() == 404 || status.as_u16() == 405 {
            let body_bytes = response
                .bytes()
                .await
                .map_err(|e| format!("读取响应失败：{}", e))?;
            let error_body = String::from_utf8_lossy(&body_bytes);
            return Err(format!("上游返回 {} - {}", status.as_u16(), error_body));
        }

        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| format!("读取响应失败：{}", e))?;

        Ok((upstream_url, status.as_u16(), headers, body_bytes.to_vec()))
    }

    /// 代理请求并返回原始流式响应（用于协议转换场景，流式）
    /// 返回 (upstream_url, status, headers, stream)
    pub async fn proxy_request_stream_raw(
        &self,
        route: &RouteResult,
        method: String,
        target_protocol: Protocol,
        body: serde_json::Value,
    ) -> Result<(String, u16, reqwest::header::HeaderMap, reqwest::Response), String> {
        // 构建上游请求
        let (upstream_url, request_body_bytes, req_builder) =
            self.build_upstream_request(route, method, target_protocol, &body)?;
        let req_builder = self.apply_auth(route, req_builder).await;
        let req_builder = Self::apply_protocol_headers(target_protocol, req_builder);

        // 发送请求
        let response = req_builder
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request_body_bytes)
            .send()
            .await
            .map_err(|e| format!("上游请求失败：{}", e))?;

        let status = response.status();
        let headers = response.headers().clone();

        Ok((upstream_url, status.as_u16(), headers, response))
    }
}

/// 应用参数覆盖到请求体（提取为独立函数供测试使用）
pub fn apply_param_overrides_inner(body: &mut serde_json::Value, route: &RouteResult) {
    if let serde_json::Value::Object(ref mut map) = body {
        // 先应用 default 参数（只有当用户没有设置时才应用）
        for (key, value) in &route.default_params {
            if !map.contains_key(key) {
                debug!("应用默认参数：{} = {}", key, value);
                // 如果参数名是 extra_body，将其内容展开到请求体顶层
                if key == "extra_body" {
                    if let serde_json::Value::Object(extra_body_map) = value {
                        for (extra_key, extra_value) in extra_body_map {
                            debug!("展开 extra_body 参数：{} = {}", extra_key, extra_value);
                            map.insert(extra_key.clone(), extra_value.clone());
                        }
                    }
                } else {
                    map.insert(key.clone(), value.clone());
                }
            }
        }

        // 再应用 override 参数（强制覆盖）
        for (key, value) in &route.override_params {
            debug!("强制覆盖参数：{} = {}", key, value);
            // 如果参数名是 extra_body，将其内容展开到请求体顶层
            if key == "extra_body" {
                if let serde_json::Value::Object(extra_body_map) = value {
                    for (extra_key, extra_value) in extra_body_map {
                        debug!("展开 extra_body 参数：{} = {}", extra_key, extra_value);
                        map.insert(extra_key.clone(), extra_value.clone());
                    }
                }
            } else {
                map.insert(key.clone(), value.clone());
            }
        }

        // 确保 model 字段使用目标模型
        map.insert(
            "model".to_string(),
            serde_json::Value::String(route.target_model.clone()),
        );
    }
}

/// 根据 API 类型和目标协议重写上游路径
fn transform_upstream_path(api_type: &ApiType, protocol: Protocol) -> String {
    match api_type {
        ApiType::ChatGptCodex => {
            // Codex 只支持 /codex/responses
            "/codex/responses".to_string()
        }
        ApiType::OpenAI => protocol.to_upstream_path().to_string(),
    }
}

/// 将 chat completions 请求格式转换为 Responses API 格式
fn transform_chat_to_responses(body: &mut serde_json::Value) {
    if let serde_json::Value::Object(ref mut map) = body {
        // 从 messages 中分离 system 消息和普通消息
        if let Some(messages) = map.remove("messages") {
            if let serde_json::Value::Array(ref msgs) = messages {
                let mut instructions_parts = Vec::new();
                let mut non_system_msgs = serde_json::Value::Array(Vec::new());

                for msg in msgs {
                    if let serde_json::Value::Object(ref m) = msg {
                        if let Some(role) = m.get("role") {
                            if role == "system" {
                                // system 消息的内容提取到 instructions
                                if let Some(content) = m.get("content") {
                                    instructions_parts.push(content.clone());
                                }
                                continue;
                            }
                        }
                    }
                    non_system_msgs.as_array_mut().unwrap().push(msg.clone());
                }

                // 构建 instructions（合并所有 system 消息）
                if !instructions_parts.is_empty() {
                    let instructions = if instructions_parts.len() == 1 {
                        instructions_parts[0].clone()
                    } else {
                        serde_json::Value::String(
                            instructions_parts
                                .iter()
                                .map(|c| c.as_str().unwrap_or(""))
                                .collect::<Vec<_>>()
                                .join("\n\n"),
                        )
                    };
                    map.insert("instructions".to_string(), instructions);
                } else {
                    // 没有 system 消息时也设置空 instructions（Codex 后端必需）
                    map.insert(
                        "instructions".to_string(),
                        serde_json::Value::String("".to_string()),
                    );
                }

                // 非 system 消息放入 input
                map.insert("input".to_string(), non_system_msgs);
            } else {
                // 不是数组，直接放入 input
                map.insert("input".to_string(), messages);
            }
        } else {
            // 没有 messages 字段，设置空 instructions
            map.insert(
                "instructions".to_string(),
                serde_json::Value::String("".to_string()),
            );
        }

        // max_tokens → max_output_tokens
        if let Some(max_tokens) = map.remove("max_tokens") {
            map.insert("max_output_tokens".to_string(), max_tokens);
        }

        // 移除 stream_options（Responses API 不需要）
        map.remove("stream_options");

        // 注入 store: false（Codex OAuth token 要求）
        map.insert("store".to_string(), serde_json::Value::Bool(false));
    }
}

/// 确保 Codex Responses 请求满足后端约束
fn ensure_codex_responses_requirements(body: &mut serde_json::Value) {
    if let serde_json::Value::Object(ref mut map) = body {
        // Codex 要求必须有 instructions 字段
        if !map.contains_key("instructions") || map.get("instructions").is_some_and(|v| v.is_null())
        {
            map.insert(
                "instructions".to_string(),
                serde_json::Value::String("".to_string()),
            );
        }
        // Codex OAuth token 要求 store=false
        map.insert("store".to_string(), serde_json::Value::Bool(false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_codex_responses_requirements_adds_missing_fields() {
        let mut body = serde_json::json!({
            "model": "gpt-5.5"
        });
        ensure_codex_responses_requirements(&mut body);
        assert_eq!(body["instructions"], serde_json::json!(""));
        assert_eq!(body["store"], serde_json::json!(false));
    }

    #[test]
    fn test_ensure_codex_responses_requirements_keeps_existing_instructions() {
        let mut body = serde_json::json!({
            "model": "gpt-5.5",
            "instructions": "You are helpful",
            "store": true
        });
        ensure_codex_responses_requirements(&mut body);
        assert_eq!(body["instructions"], serde_json::json!("You are helpful"));
        assert_eq!(body["store"], serde_json::json!(false));
    }
}
