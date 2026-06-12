// CLIProxyAPI 代理模块
// 将请求转发到 CLIProxyAPI sidecar 进程

use actix_web::body::BodyStream;
use actix_web::http::header::{
    HeaderMap as ActixHeaderMap, HeaderName as ActixHeaderName, HeaderValue as ActixHeaderValue,
};
use actix_web::web;
use futures::StreamExt;
use reqwest::header::{
    HeaderMap as ReqwestHeaderMap, HeaderName as ReqwestHeaderName,
    HeaderValue as ReqwestHeaderValue, AUTHORIZATION, CONTENT_TYPE,
};

use crate::state::{DebugDataStore, DebugStreamHub};
use llm_wrapper::proxy::{build_endpoint_path_with_query, replace_model_only, DebugInfo};

fn is_hop_by_hop_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("keep-alive")
        || name.eq_ignore_ascii_case("proxy-authenticate")
        || name.eq_ignore_ascii_case("proxy-authorization")
        || name.eq_ignore_ascii_case("te")
        || name.eq_ignore_ascii_case("trailer")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("upgrade")
}

fn should_forward_request_header(name: &str) -> bool {
    !is_hop_by_hop_header(name)
        && !name.eq_ignore_ascii_case("host")
        && !name.eq_ignore_ascii_case("content-length")
        && !name.eq_ignore_ascii_case("authorization")
}

fn should_forward_response_header(name: &str) -> bool {
    !is_hop_by_hop_header(name) && !name.eq_ignore_ascii_case("content-length")
}

fn build_cli_proxy_request_headers(
    client_headers: &ActixHeaderMap,
    api_key: Option<&str>,
) -> ReqwestHeaderMap {
    let mut headers = ReqwestHeaderMap::new();

    for (name, value) in client_headers {
        if !should_forward_request_header(name.as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            ReqwestHeaderName::from_bytes(name.as_str().as_bytes()),
            ReqwestHeaderValue::from_bytes(value.as_bytes()),
        ) {
            headers.append(name, value);
        }
    }

    if !headers.contains_key(CONTENT_TYPE) {
        headers.insert(
            CONTENT_TYPE,
            ReqwestHeaderValue::from_static("application/json"),
        );
    }

    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        if let Ok(value) = ReqwestHeaderValue::from_str(&format!("Bearer {}", key)) {
            headers.insert(AUTHORIZATION, value);
        }
    }

    headers
}

fn append_cli_proxy_response_headers(
    resp_builder: &mut actix_web::HttpResponseBuilder,
    headers: &ReqwestHeaderMap,
) {
    for (name, value) in headers {
        if !should_forward_response_header(name.as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            ActixHeaderName::from_bytes(name.as_str().as_bytes()),
            ActixHeaderValue::from_bytes(value.as_bytes()),
        ) {
            resp_builder.append_header((name, value));
        }
    }
}

/// 将请求代理到 CLIProxyAPI
///
/// 当上游使用 CLIProxyAPI 管理的认证方式时，
/// 除 model 替换为路由目标模型外，请求透传到 CLIProxyAPI，
/// 由 CLIProxyAPI 负责 OAuth 认证、协议转换和请求伪装。
#[allow(clippy::too_many_arguments)]
pub async fn proxy_to_cli_proxy_api(
    cli_proxy_api_endpoint: &str,
    api_key: Option<&str>,
    target_model: &str,
    endpoint_path: &str,
    query_string: &str,
    client_headers: &ActixHeaderMap,
    body: &serde_json::Value,
    debug_data: Option<&web::Data<DebugDataStore>>,
    stream_hub: Option<&web::Data<DebugStreamHub>>,
) -> actix_web::HttpResponse {
    let url = format!(
        "{}{}",
        cli_proxy_api_endpoint.trim_end_matches('/'),
        build_endpoint_path_with_query(endpoint_path, query_string)
    );
    let client_request = body.clone();
    let mut upstream_request = body.clone();
    replace_model_only(&mut upstream_request, target_model);

    let builder = reqwest::Client::new()
        .post(&url)
        .headers(build_cli_proxy_request_headers(client_headers, api_key))
        .body(upstream_request.to_string());

    match builder.send().await {
        Ok(response) => {
            let status = response.status();
            let response_headers = response.headers().clone();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_string();

            let is_stream = content_type.contains("text/event-stream");

            if is_stream {
                let initial_debug = DebugInfo {
                    client_request,
                    client_ip: String::new(),
                    client_url: String::new(),
                    endpoint: endpoint_path.to_string(),
                    upstream_url: url.clone(),
                    upstream_request: upstream_request.clone(),
                    upstream_response: serde_json::Value::Null,
                };

                if let Some(debug_store) = debug_data {
                    debug_store.data.write().await.replace(initial_debug);
                }

                let stream_hub_clone = stream_hub.map(|h| h.sender.clone());

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
                append_cli_proxy_response_headers(&mut resp_builder, &response_headers);

                resp_builder.body(BodyStream::new(stream))
            } else {
                let body_bytes = match response.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        return actix_web::HttpResponse::BadGateway().json(serde_json::json!({
                            "error": format!("Failed to read CLIProxyAPI response: {}", e)
                        }));
                    }
                };

                let upstream_response = serde_json::from_slice::<serde_json::Value>(&body_bytes)
                    .unwrap_or(serde_json::Value::Null);

                let debug_info = DebugInfo {
                    client_request,
                    client_ip: String::new(),
                    client_url: String::new(),
                    endpoint: endpoint_path.to_string(),
                    upstream_url: url,
                    upstream_request,
                    upstream_response,
                };

                if let Some(debug_store) = debug_data {
                    debug_store.data.write().await.replace(debug_info);
                }

                let mut resp_builder = actix_web::HttpResponse::build(
                    actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap(),
                );
                append_cli_proxy_response_headers(&mut resp_builder, &response_headers);

                resp_builder.body(body_bytes)
            }
        }
        Err(e) => {
            if e.is_connect() {
                actix_web::HttpResponse::BadGateway().json(serde_json::json!({
                    "error": format!("Cannot connect to CLIProxyAPI at {}. Is it running?", cli_proxy_api_endpoint)
                }))
            } else {
                actix_web::HttpResponse::BadGateway().json(serde_json::json!({
                    "error": format!("CLIProxyAPI proxy error: {}", e)
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::header::{HeaderMap, HeaderName, HeaderValue};

    fn insert(headers: &mut HeaderMap, name: &'static str, value: &'static str) {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }

    #[test]
    fn test_cli_proxy_request_headers_passthrough_except_proxy_managed_headers() {
        let mut client_headers = HeaderMap::new();
        insert(&mut client_headers, "host", "wrapper.local");
        insert(
            &mut client_headers,
            "content-type",
            "application/json; charset=utf-8",
        );
        insert(&mut client_headers, "content-length", "123");
        insert(&mut client_headers, "connection", "keep-alive");
        insert(&mut client_headers, "authorization", "Bearer client-key");
        insert(
            &mut client_headers,
            "anthropic-beta",
            "fine-grained-tool-streaming",
        );
        insert(&mut client_headers, "x-amp-thread-id", "thread-123");
        insert(&mut client_headers, "user-agent", "claude-cli/1.0");

        let forwarded = build_cli_proxy_request_headers(&client_headers, Some("sidecar-key"));

        assert_eq!(
            forwarded.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(
            forwarded.get(AUTHORIZATION).and_then(|v| v.to_str().ok()),
            Some("Bearer sidecar-key")
        );
        assert_eq!(
            forwarded
                .get("anthropic-beta")
                .and_then(|v| v.to_str().ok()),
            Some("fine-grained-tool-streaming")
        );
        assert_eq!(
            forwarded
                .get("x-amp-thread-id")
                .and_then(|v| v.to_str().ok()),
            Some("thread-123")
        );
        assert_eq!(
            forwarded.get("user-agent").and_then(|v| v.to_str().ok()),
            Some("claude-cli/1.0")
        );
        assert!(forwarded.get("host").is_none());
        assert!(forwarded.get("content-length").is_none());
        assert!(forwarded.get("connection").is_none());
    }

    #[test]
    fn test_cli_proxy_request_headers_default_to_json_content_type() {
        let forwarded = build_cli_proxy_request_headers(&HeaderMap::new(), None);

        assert_eq!(
            forwarded.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert!(forwarded.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn test_cli_proxy_response_headers_filter_only_transport_headers() {
        assert!(should_forward_response_header("content-type"));
        assert!(should_forward_response_header("anthropic-request-id"));
        assert!(should_forward_response_header("x-request-id"));
        assert!(!should_forward_response_header("content-length"));
        assert!(!should_forward_response_header("transfer-encoding"));
        assert!(!should_forward_response_header("connection"));
    }
}
