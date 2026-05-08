// CLIProxyAPI 代理模块
// 将请求转发到 CLIProxyAPI sidecar 进程

use actix_web::body::BodyStream;
use actix_web::web;
use futures::StreamExt;

use crate::proxy::DebugInfo;

/// 将请求代理到 CLIProxyAPI
///
/// 当上游使用 CLIProxyAPI 管理的认证方式时，
/// 请求原样转发到 CLIProxyAPI，由 CLIProxyAPI 负责
/// OAuth 认证、协议转换和请求伪装。
pub async fn proxy_to_cli_proxy_api(
    cli_proxy_api_endpoint: &str,
    api_key: Option<&str>,
    endpoint_path: &str,
    body: &serde_json::Value,
    debug_data: Option<&web::Data<crate::DebugDataStore>>,
    stream_hub: Option<&web::Data<crate::DebugStreamHub>>,
) -> actix_web::HttpResponse {
    let url = format!("{}{}", cli_proxy_api_endpoint, endpoint_path);
    let client_request = body.clone();

    let mut builder = reqwest::Client::new()
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string());

    if let Some(key) = api_key {
        builder = builder.bearer_auth(key);
    }

    match builder.send().await {
        Ok(response) => {
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/event-stream")
                .to_string();

            let is_stream = content_type.contains("text/event-stream");

            if is_stream {
                let initial_debug = DebugInfo {
                    client_request,
                    client_ip: String::new(),
                    client_url: String::new(),
                    endpoint: endpoint_path.to_string(),
                    upstream_url: url.clone(),
                    upstream_request: body.clone(),
                    upstream_response: serde_json::Value::Null,
                };

                if let Some(debug_store) = debug_data {
                    debug_store.data.write().await.replace(initial_debug);
                }

                let stream_hub_clone = stream_hub
                    .map(|h| h.sender.clone());

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
                resp_builder.content_type(content_type);

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

                let upstream_response =
                    serde_json::from_slice::<serde_json::Value>(&body_bytes)
                        .unwrap_or(serde_json::Value::Null);

                let debug_info = DebugInfo {
                    client_request,
                    client_ip: String::new(),
                    client_url: String::new(),
                    endpoint: endpoint_path.to_string(),
                    upstream_url: url,
                    upstream_request: body.clone(),
                    upstream_response,
                };

                if let Some(debug_store) = debug_data {
                    debug_store.data.write().await.replace(debug_info);
                }

                let mut resp_builder = actix_web::HttpResponse::build(
                    actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap(),
                );
                resp_builder.content_type(content_type);

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
