use crate::oauth::AuthManager;
use crate::router::RouteResult;
use actix_web::http::header::HeaderMap;
use futures::StreamExt;
use reqwest::Client;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

const ANTHROPIC_PASSTHROUGH_HEADERS: &[&str] = &[
    "anthropic-version",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
];

/// 调试信息
#[derive(Debug, Clone, Default, serde::Serialize)]
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

    /// 代理请求到上游（带调试）- 直接转发
    #[allow(clippy::too_many_arguments)]
    pub async fn proxy_request_with_debug(
        &self,
        route: &RouteResult,
        endpoint_path: &str,
        query_string: &str,
        client_headers: &HeaderMap,
        body: serde_json::Value,
        client_ip: String,
        client_url: String,
        debug_data: Option<Arc<RwLock<Option<DebugInfo>>>>,
        stream_hub: Option<Arc<tokio::sync::broadcast::Sender<String>>>,
    ) -> Result<actix_web::HttpResponse, String> {
        let client_request = body.clone();

        // 构建请求体（应用参数覆盖）
        let mut request_body = body.clone();
        apply_param_overrides_inner(&mut request_body, route);

        let upstream_url = build_upstream_url(route, endpoint_path, query_string);
        debug!("代理请求到上游：{}", upstream_url);

        let request_body_bytes =
            serde_json::to_vec(&request_body).map_err(|e| format!("序列化失败：{}", e))?;

        let req_builder = self
            .client
            .post(&upstream_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request_body_bytes);
        let req_builder =
            apply_anthropic_passthrough_headers(req_builder, endpoint_path, client_headers);
        let req_builder = self.apply_auth(route, req_builder).await;

        let response = req_builder
            .send()
            .await
            .map_err(|e| format!("上游请求失败：{}", e))?;

        let status = response.status();
        let headers = response.headers().clone();

        if status.as_u16() == 404 || status.as_u16() == 405 {
            let body_bytes = response
                .bytes()
                .await
                .map_err(|e| format!("读取响应失败：{}", e))?;
            let error_body = String::from_utf8_lossy(&body_bytes);
            return Err(format!("上游返回 {} - {}", status.as_u16(), error_body));
        }

        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let is_stream = request_body
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || content_type.contains("text/event-stream");

        if is_stream {
            use actix_web::body::BodyStream;

            let initial_debug_info = DebugInfo {
                client_request: client_request.clone(),
                client_ip: client_ip.clone(),
                client_url: client_url.clone(),
                endpoint: endpoint_path.to_string(),
                upstream_url: upstream_url.clone(),
                upstream_request: request_body.clone(),
                upstream_response: serde_json::Value::Null,
            };

            if let Some(ref debug_store) = debug_data {
                debug_store.write().await.replace(initial_debug_info);
            }

            let stream_hub_clone = stream_hub.clone();

            let stream = response.bytes_stream().map(move |item| {
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
                item.map_err(std::io::Error::other)
            });

            let mut resp_builder = actix_web::HttpResponse::build(
                actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap(),
            );
            resp_builder.content_type("text/event-stream");

            Ok(resp_builder.body(BodyStream::new(stream)))
        } else {
            let body_bytes = response
                .bytes()
                .await
                .map_err(|e| format!("读取响应失败：{}", e))?;

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
                endpoint: endpoint_path.to_string(),
                upstream_url,
                upstream_request: request_body,
                upstream_response,
            };

            if let Some(ref debug_store) = debug_data {
                debug_store.write().await.replace(debug_info);
            }

            Ok(resp_builder.body(body_bytes.to_vec()))
        }
    }
}

pub fn build_endpoint_path_with_query(endpoint_path: &str, query_string: &str) -> String {
    if query_string.is_empty() {
        endpoint_path.to_string()
    } else {
        format!("{}?{}", endpoint_path, query_string)
    }
}

pub fn is_anthropic_messages_endpoint(endpoint_path: &str) -> bool {
    endpoint_path == "/v1/messages" || endpoint_path.starts_with("/v1/messages/")
}

fn build_upstream_url(route: &RouteResult, endpoint_path: &str, query_string: &str) -> String {
    let base_url = if is_anthropic_messages_endpoint(endpoint_path) {
        route
            .anthropic_base_url
            .as_deref()
            .unwrap_or(&route.upstream_base_url)
    } else {
        &route.upstream_base_url
    };
    let path = build_endpoint_path_with_query(endpoint_path, query_string);
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

pub fn anthropic_passthrough_headers(
    endpoint_path: &str,
    headers: &HeaderMap,
) -> Vec<(&'static str, String)> {
    if !is_anthropic_messages_endpoint(endpoint_path) {
        return Vec::new();
    }

    ANTHROPIC_PASSTHROUGH_HEADERS
        .iter()
        .filter_map(|name| {
            headers
                .get(*name)
                .and_then(|value| value.to_str().ok())
                .map(|value| (*name, value.to_string()))
        })
        .collect()
}

pub(crate) fn apply_anthropic_passthrough_headers(
    mut req_builder: reqwest::RequestBuilder,
    endpoint_path: &str,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    for (name, value) in anthropic_passthrough_headers(endpoint_path, headers) {
        req_builder = req_builder.header(name, value);
    }
    req_builder
}

/// 仅替换请求体中的 model 字段，不应用别名参数覆盖。
pub fn replace_model_only(body: &mut serde_json::Value, target_model: &str) {
    if let serde_json::Value::Object(ref mut map) = body {
        map.insert(
            "model".to_string(),
            serde_json::Value::String(target_model.to_string()),
        );
    }
}

/// 应用参数覆盖到请求体（提取为独立函数供测试使用）
pub fn apply_param_overrides_inner(body: &mut serde_json::Value, route: &RouteResult) {
    if let serde_json::Value::Object(ref mut map) = body {
        for (key, value) in &route.default_params {
            if !map.contains_key(key) {
                debug!("应用默认参数：{} = {}", key, value);
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

        for (key, value) in &route.override_params {
            debug!("强制覆盖参数：{} = {}", key, value);
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

        map.insert(
            "model".to_string(),
            serde_json::Value::String(route.target_model.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn create_test_route(
        override_params: HashMap<String, serde_json::Value>,
        default_params: HashMap<String, serde_json::Value>,
    ) -> RouteResult {
        RouteResult {
            upstream_base_url: "http://localhost:8080".to_string(),
            anthropic_base_url: None,
            upstream_name: "test".to_string(),
            upstream_auth: crate::models::UpstreamAuth::ApiKey {
                key: Some("test-key".to_string()),
            },
            target_model: "gpt-4-turbo".to_string(),
            override_params,
            default_params,
            use_cli_proxy_api: false,
            cli_proxy_api_endpoint: "http://127.0.0.1:8317".to_string(),
            cli_proxy_api_api_key: None,
            support_chat_completions: true,
            support_responses: false,
            support_anthropic_messages: true,
        }
    }

    #[test]
    fn test_build_upstream_url_uses_anthropic_base_url_and_query() {
        let mut route = create_test_route(HashMap::new(), HashMap::new());
        route.upstream_base_url = "https://example.com/openai/".to_string();
        route.anthropic_base_url = Some("https://example.com/anthropic/".to_string());

        let url = build_upstream_url(&route, "/v1/messages", "beta=true");

        assert_eq!(url, "https://example.com/anthropic/v1/messages?beta=true");
    }

    #[test]
    fn test_build_upstream_url_uses_default_base_for_non_anthropic() {
        let mut route = create_test_route(HashMap::new(), HashMap::new());
        route.upstream_base_url = "https://example.com/openai/".to_string();
        route.anthropic_base_url = Some("https://example.com/anthropic/".to_string());

        let url = build_upstream_url(&route, "/v1/chat/completions", "");

        assert_eq!(url, "https://example.com/openai/v1/chat/completions");
    }

    #[test]
    fn test_build_upstream_url_uses_anthropic_base_url_for_messages_subpaths() {
        let mut route = create_test_route(HashMap::new(), HashMap::new());
        route.upstream_base_url = "https://example.com/openai/".to_string();
        route.anthropic_base_url = Some("https://example.com/anthropic/".to_string());

        let url = build_upstream_url(&route, "/v1/messages/count_tokens", "beta=true");

        assert_eq!(
            url,
            "https://example.com/anthropic/v1/messages/count_tokens?beta=true"
        );
    }

    #[test]
    fn test_anthropic_passthrough_headers_are_limited_to_messages() {
        use actix_web::http::header::HeaderName;

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            "2023-06-01".parse().expect("valid header value"),
        );
        headers.insert(
            HeaderName::from_static("anthropic-beta"),
            "fine-grained-tool-streaming-2025-05-14"
                .parse()
                .expect("valid header value"),
        );
        headers.insert(
            HeaderName::from_static("authorization"),
            "Bearer client-token".parse().expect("valid header value"),
        );

        let forwarded = anthropic_passthrough_headers("/v1/messages", &headers);
        assert_eq!(
            forwarded,
            vec![
                ("anthropic-version", "2023-06-01".to_string()),
                (
                    "anthropic-beta",
                    "fine-grained-tool-streaming-2025-05-14".to_string()
                ),
            ]
        );

        assert!(anthropic_passthrough_headers("/v1/chat/completions", &headers).is_empty());
    }

    #[test]
    fn test_anthropic_passthrough_headers_apply_to_messages_subpaths() {
        use actix_web::http::header::HeaderName;

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            "2023-06-01".parse().expect("valid header value"),
        );

        let forwarded = anthropic_passthrough_headers("/v1/messages/count_tokens", &headers);

        assert_eq!(
            forwarded,
            vec![("anthropic-version", "2023-06-01".to_string())]
        );
    }

    #[test]
    fn test_apply_override_mode_forces_coverage() {
        let mut override_params = HashMap::new();
        override_params.insert("temperature".to_string(), serde_json::json!(0.9));
        let route = create_test_route(override_params, HashMap::new());

        let mut body = serde_json::json!({"model": "gpt-4", "temperature": 0.5});
        apply_param_overrides_inner(&mut body, &route);
        assert_eq!(body["temperature"], 0.9);
    }

    #[test]
    fn test_apply_default_mode_when_not_set() {
        let mut default_params = HashMap::new();
        default_params.insert("top_p".to_string(), serde_json::json!(0.8));
        let route = create_test_route(HashMap::new(), default_params);

        let mut body = serde_json::json!({"model": "gpt-4"});
        apply_param_overrides_inner(&mut body, &route);
        assert_eq!(body["top_p"], 0.8);
    }

    #[test]
    fn test_apply_default_mode_when_already_set() {
        let mut default_params = HashMap::new();
        default_params.insert("temperature".to_string(), serde_json::json!(0.7));
        let route = create_test_route(HashMap::new(), default_params);

        let mut body = serde_json::json!({"model": "gpt-4", "temperature": 0.5});
        apply_param_overrides_inner(&mut body, &route);
        assert_eq!(body["temperature"], 0.5);
    }

    #[test]
    fn test_apply_both_override_and_default() {
        let mut override_params = HashMap::new();
        override_params.insert("temperature".to_string(), serde_json::json!(0.9));
        let mut default_params = HashMap::new();
        default_params.insert("top_p".to_string(), serde_json::json!(0.8));
        let route = create_test_route(override_params, default_params);

        let mut body = serde_json::json!({"model": "gpt-4", "temperature": 0.5});
        apply_param_overrides_inner(&mut body, &route);
        assert_eq!(body["temperature"], 0.9);
        assert_eq!(body["top_p"], 0.8);
    }

    #[test]
    fn test_apply_model_replacement() {
        let route = create_test_route(HashMap::new(), HashMap::new());
        let mut body = serde_json::json!({"model": "gpt-4"});
        apply_param_overrides_inner(&mut body, &route);
        assert_eq!(body["model"], "gpt-4-turbo");
    }

    #[test]
    fn test_apply_empty_body() {
        let route = create_test_route(HashMap::new(), HashMap::new());
        let mut body = serde_json::json!({});
        apply_param_overrides_inner(&mut body, &route);
        assert_eq!(body["model"], "gpt-4-turbo");
    }

    #[test]
    fn test_apply_extra_body_expand() {
        let mut default_params = HashMap::new();
        default_params.insert(
            "extra_body".to_string(),
            serde_json::json!({"custom_field": "value"}),
        );
        let route = create_test_route(HashMap::new(), default_params);

        let mut body = serde_json::json!({"model": "gpt-4"});
        apply_param_overrides_inner(&mut body, &route);
        assert_eq!(body["custom_field"], "value");
    }
}
