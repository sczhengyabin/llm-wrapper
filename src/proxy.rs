use crate::router::RouteResult;
use reqwest::Client;
use tracing::debug;
use std::sync::Arc;
use tokio::sync::RwLock;
use futures::StreamExt;

/// 调试信息
#[derive(Debug, Clone, serde::Serialize)]
pub struct DebugInfo {
    pub client_request: serde_json::Value,
    pub endpoint: String,
    pub upstream_request: serde_json::Value,
    pub upstream_response: serde_json::Value,
}

/// 请求代理
pub struct Proxy {
    client: Client,
}

impl Proxy {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(1200))
                .build()
                .expect("无法创建 HTTP 客户端"),
        }
    }

    /// 代理请求到上游（带调试）
    pub async fn proxy_request_with_debug(
        &self,
        route: &RouteResult,
        method: String,
        path: String,
        body: serde_json::Value,
        debug_data: Option<Arc<RwLock<Option<DebugInfo>>>>,
        stream_hub: Option<Arc<tokio::sync::broadcast::Sender<String>>>,
    ) -> Result<actix_web::HttpResponse, String> {
        // 保存客户端原始请求
        let client_request = body.clone();

        // 保存端点
        let endpoint = path.clone();

        // 构建上游 URL
        let upstream_url = format!("{}{}", route.upstream_base_url, path);

        debug!("代理请求到上游：{}", upstream_url);

        // 构建请求体
        let mut request_body = body;
        // 应用参数覆盖
        apply_param_overrides_inner(&mut request_body, route);

        // 保存上游请求体（调试用）- 总是保存，不依赖 debug_mode
        let upstream_request = request_body.clone();

        // 检查是否是流式请求
        let is_stream = request_body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

        let request_body = serde_json::to_vec(&request_body).map_err(|e| e.to_string())?;

        // 构建请求
        let mut req_builder = self.client
            .request(
                method.parse::<reqwest::Method>().map_err(|e| e.to_string())?,
                &upstream_url,
            );

        // 添加上游 API 密钥
        if let Some(api_key) = &route.upstream_api_key {
            if api_key != "none" && !api_key.is_empty() {
                req_builder = req_builder.bearer_auth(api_key);
            }
        }

        // 发送请求
        let response = req_builder
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request_body)
            .send()
            .await
            .map_err(|e| format!("上游请求失败：{}", e))?;

        // 读取响应
        let status = response.status();
        let headers = response.headers().clone();

        // 检查是否是不支持端点的错误（404/405）
        if status.as_u16() == 404 || status.as_u16() == 405 {
            let body_bytes = response.bytes().await.map_err(|e| format!("读取响应失败：{}", e))?;
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
                endpoint: endpoint.clone(),
                upstream_request: upstream_request.clone(),
                upstream_response: serde_json::Value::Null,
            };

            if let Some(ref debug_store) = debug_data {
                debug_store.write().await.replace(initial_debug_info);
            }

            // 获取 stream_hub 用于广播
            let stream_hub_clone = stream_hub.clone();

            // 流式代理，同时广播 chunk
            let stream = response.bytes_stream()
                .map(move |item| {
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
                actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap()
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
            let upstream_response = serde_json::from_slice::<serde_json::Value>(&body_bytes).unwrap_or(serde_json::Value::Null);

            let mut resp_builder = actix_web::HttpResponse::build(
                actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap()
            );
            if let Some(ct) = headers.get(reqwest::header::CONTENT_TYPE) {
                resp_builder.content_type(ct.to_str().unwrap_or("application/json"));
            }

            let debug_info = DebugInfo {
                client_request,
                endpoint,
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
}

/// 应用参数覆盖到请求体（提取为独立函数供测试使用）
pub fn apply_param_overrides_inner(
    body: &mut serde_json::Value,
    route: &RouteResult,
) {
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
        map.insert("model".to_string(), serde_json::Value::String(route.target_model.clone()));
    }
}

impl Default for Proxy {
    fn default() -> Self {
        Self::new()
    }
}
