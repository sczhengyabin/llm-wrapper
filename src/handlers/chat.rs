use actix_web::{web, HttpResponse};
use serde_json::json;

use crate::handlers::require_client_api_key;
use crate::state::AppState;

pub(crate) async fn chat_completions(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_client_api_key(&state, &req).await {
        return resp;
    }
    handle_protocol_request(state, body.into_inner(), req, "/v1/chat/completions").await
}

pub(crate) async fn responses(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_client_api_key(&state, &req).await {
        return resp;
    }
    handle_protocol_request(state, body.into_inner(), req, "/v1/responses").await
}

pub(crate) async fn messages(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_client_api_key(&state, &req).await {
        return resp;
    }
    let endpoint_path = req.uri().path().to_string();
    handle_protocol_request(state, body.into_inner(), req, &endpoint_path).await
}

/// 通用的协议请求处理器
/// CLIProxyAPI 上游转发到 CLIProxyAPI，其他上游直接代理
pub(crate) async fn handle_protocol_request(
    state: web::Data<AppState>,
    body: serde_json::Value,
    req: actix_web::HttpRequest,
    endpoint_path: &str,
) -> HttpResponse {
    use llm_wrapper::proxy::Proxy;
    use llm_wrapper::router::ModelRouter;

    let debug_mode = req
        .headers()
        .get("X-Debug-Mode")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    let model = match body.get("model").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return HttpResponse::BadRequest()
                .json(json!({"error": {"message": "缺少 model 参数"}}))
        }
    };

    let router = ModelRouter::new(state.config.clone());
    let proxy = Proxy::new(state.auth_manager.clone());

    let route = match router.route(&model).await {
        Some(r) => r,
        None => {
            return HttpResponse::BadRequest()
                .json(json!({"error": {"message": format!("找不到模型 {} 的路由", model)}}))
        }
    };

    // 检查上游是否支持当前协议
    let protocol_supported = if llm_wrapper::proxy::is_anthropic_messages_endpoint(endpoint_path) {
        route.support_anthropic_messages
    } else {
        match endpoint_path {
            "/v1/chat/completions" => route.support_chat_completions,
            "/v1/responses" => route.support_responses,
            _ => true,
        }
    };
    if !protocol_supported {
        return HttpResponse::BadRequest().json(json!({
            "error": {
                "message": format!("上游 '{}' 不支持当前协议端点 ({})", route.upstream_name, endpoint_path)
            }
        }));
    }

    // CLIProxyAPI 代理：如果上游使用 CLIProxyAPI 管理的认证，直接转发请求
    if route.use_cli_proxy_api {
        let manager = match &state.cli_proxy_api_manager {
            Some(m) => m,
            None => {
                return HttpResponse::BadGateway().json(json!({
                    "error": {"message": "CLIProxyAPI is not configured"}
                }));
            }
        };

        // Lazy start: if CLIProxyAPI is not running, try to start it
        if !manager.is_running().await {
            tracing::info!("CLIProxyAPI not running, attempting lazy start...");
            if let Err(e) = manager.start().await {
                return HttpResponse::BadGateway().json(json!({
                    "error": {"message": format!("CLIProxyAPI is not running: {}. Please login first via the WebUI.", e)}
                }));
            }
        }

        let manager_api_key = manager.api_key().await;
        let cli_proxy_api_key = if manager_api_key.is_empty() {
            route.cli_proxy_api_api_key.as_deref()
        } else {
            Some(manager_api_key.as_str())
        };
        return crate::cli_proxy_api_proxy::proxy_to_cli_proxy_api(
            &route.cli_proxy_api_endpoint,
            cli_proxy_api_key,
            &route.target_model,
            endpoint_path,
            req.query_string(),
            req.headers(),
            &body,
            Some(&state.debug_data),
            Some(&state.stream_hub),
        )
        .await;
    }

    // 直接代理到上游
    let client_ip = req
        .peer_addr()
        .map(|p| p.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let client_url = format!(
        "{}://{}{}",
        req.connection_info().scheme(),
        req.connection_info().host(),
        req.uri()
            .path_and_query()
            .map(|path_and_query| path_and_query.as_str())
            .unwrap_or(req.uri().path())
    );

    match proxy
        .proxy_request_with_debug(
            &route,
            endpoint_path,
            req.query_string(),
            req.headers(),
            body,
            client_ip,
            client_url,
            Some(state.debug_data.data.clone()),
            Some(state.stream_hub.sender.clone()),
        )
        .await
    {
        Ok(resp) => {
            if debug_mode {
                let is_stream = resp
                    .headers()
                    .get(actix_web::http::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.contains("text/event-stream"))
                    .unwrap_or(false);
                if !is_stream {
                    let debug_info = state.debug_data.get().await.unwrap_or_default();
                    return HttpResponse::Ok().json(json!({
                        "debug": debug_info
                    }));
                }
            }
            resp
        }
        Err(e) => {
            if e.contains("404") || e.contains("405") {
                HttpResponse::BadGateway().json(json!({
                    "error": {
                        "message": format!("上游服务不支持该端点。错误：{}", e)
                    }
                }))
            } else {
                HttpResponse::BadGateway().json(json!({"error": {"message": e}}))
            }
        }
    }
}
