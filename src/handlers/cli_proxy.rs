use actix_web::{web, Error, HttpResponse};
use futures::stream;
use serde_json::json;
use std::sync::Arc;

use crate::handlers::admin::require_admin;
use crate::state::{AppState, LOGIN_POLL_INTERVAL, LOGIN_POLL_MAX_ATTEMPTS};

pub(crate) async fn auth_login(
    state: web::Data<AppState>,
    path: web::Path<String>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let upstream_name = path.into_inner();
    let config = state.config.get_config().await;

    let Some(upstream) = config
        .upstreams
        .iter()
        .find(|u| u.name == upstream_name && u.enabled)
    else {
        return HttpResponse::NotFound().json(json!({
            "error": "上游不存在或未启用"
        }));
    };

    match &upstream.auth {
        llm_wrapper::models::UpstreamAuth::ApiKey { .. } => {
            HttpResponse::BadRequest().json(json!({
                "error": "该上游不使用 OAuth 认证"
            }))
        }
        llm_wrapper::models::UpstreamAuth::AnthropicOAuth | llm_wrapper::models::UpstreamAuth::CodexOAuth => {
            HttpResponse::BadRequest().json(json!({
                "error": format!("上游 '{}' 的登录由 CLIProxyAPI 管理，请使用 /api/cli-proxy-api/login/{}", upstream.name, upstream_name)
            }))
        }
    }
}

pub(crate) async fn auth_clear_token(
    state: web::Data<AppState>,
    path: web::Path<String>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let upstream_name = path.into_inner();
    state.auth_manager.clear_token(&upstream_name).await;
    HttpResponse::Ok().json(json!({
        "success": true
    }))
}

fn cli_proxy_api_provider(auth: &llm_wrapper::models::UpstreamAuth) -> Option<&'static str> {
    match auth {
        llm_wrapper::models::UpstreamAuth::AnthropicOAuth => Some("claude"),
        llm_wrapper::models::UpstreamAuth::CodexOAuth => Some("codex"),
        llm_wrapper::models::UpstreamAuth::ApiKey { .. } => None,
    }
}

/// CLIProxyAPI 登录：发起 OAuth 登录流程
pub(crate) async fn cli_proxy_api_login(
    state: web::Data<AppState>,
    path: web::Path<String>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let upstream_name = path.into_inner();

    // 验证上游存在且是 CLIProxyAPI 类型
    let config = state.config.get_config().await;
    let Some(upstream) = config
        .upstreams
        .iter()
        .find(|u| u.name == upstream_name && u.auth.is_cli_proxy_api())
    else {
        return HttpResponse::BadRequest().json(json!({
            "error": format!("上游 '{}' 不存在或不是 CLIProxyAPI 类型", upstream_name)
        }));
    };

    let provider = match cli_proxy_api_provider(&upstream.auth) {
        Some(provider) => provider,
        None => {
            return HttpResponse::BadRequest().json(json!({
                "error": format!("上游 '{}' 不是 CLIProxyAPI 类型", upstream_name)
            }));
        }
    };

    let manager = match &state.cli_proxy_api_manager {
        Some(m) => m,
        None => {
            return HttpResponse::InternalServerError().json(json!({
                "error": "CLIProxyAPI manager not initialized"
            }));
        }
    };

    match manager.start_login(provider).await {
        Ok(result) => HttpResponse::Ok().json(json!({
            "auth_url": result.auth_url,
            "provider": provider,
            "device_code": result.device_code,
            "manual": true,
        })),
        Err(e) => HttpResponse::InternalServerError().json(json!({
            "error": format!("Failed to start login: {}", e)
        })),
    }
}

/// CLIProxyAPI 登录完成：用户粘贴回调 URL
pub(crate) async fn cli_proxy_api_complete_login(
    state: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let upstream_name = path.into_inner();

    let callback_url = match body.get("callback_url").and_then(|v| v.as_str()) {
        Some(u) => u.to_string(),
        None => {
            return HttpResponse::BadRequest().json(json!({
                "error": "Missing 'callback_url' field"
            }));
        }
    };

    // 验证上游存在且是 CLIProxyAPI 类型
    let config = state.config.get_config().await;
    let Some(upstream) = config
        .upstreams
        .iter()
        .find(|u| u.name == upstream_name && u.auth.is_cli_proxy_api())
    else {
        return HttpResponse::BadRequest().json(json!({
            "error": format!("上游 '{}' 不存在或不是 CLIProxyAPI 类型", upstream_name)
        }));
    };

    let provider = match cli_proxy_api_provider(&upstream.auth) {
        Some(provider) => provider,
        None => {
            return HttpResponse::BadRequest().json(json!({
                "error": format!("上游 '{}' 不是 CLIProxyAPI 类型", upstream_name)
            }));
        }
    };

    let manager = match &state.cli_proxy_api_manager {
        Some(m) => m,
        None => {
            return HttpResponse::InternalServerError().json(json!({
                "error": "CLIProxyAPI manager not initialized"
            }));
        }
    };

    match manager.complete_login(provider, &callback_url).await {
        Ok(()) => HttpResponse::Ok().json(json!({
            "success": true,
            "message": "Login callback submitted, completing authentication..."
        })),
        // complete_login 同步路径只有回调 URL 校验会失败，按客户端错误处理
        Err(e) => HttpResponse::BadRequest().json(json!({
            "error": format!("Failed to complete login: {}", e)
        })),
    }
}

/// CLIProxyAPI 登录状态：SSE 推送登录完成事件
pub(crate) async fn cli_proxy_api_login_stream(
    state: web::Data<AppState>,
    path: web::Path<String>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let upstream_name = path.into_inner();

    let config = state.config.get_config().await;
    let upstream = config
        .upstreams
        .iter()
        .find(|u| u.name == upstream_name && u.auth.is_cli_proxy_api());

    let Some(upstream) = upstream else {
        return HttpResponse::BadRequest().json(json!({
            "error": format!("上游 '{}' 不存在或不是 CLIProxyAPI 类型", upstream_name)
        }));
    };

    let Some(provider) = cli_proxy_api_provider(&upstream.auth) else {
        return HttpResponse::BadRequest().json(json!({
            "error": format!("上游 '{}' 不是 CLIProxyAPI 类型", upstream_name)
        }));
    };

    let manager = match &state.cli_proxy_api_manager {
        Some(m) => Arc::clone(m),
        None => {
            return HttpResponse::InternalServerError().json(json!({
                "error": "CLIProxyAPI manager not initialized"
            }));
        }
    };

    let provider = provider.to_string();
    let stream = stream::unfold(
        (false, 0u16, manager, upstream_name, provider),
        |(finished, attempt, manager, upstream_name, provider)| async move {
            if finished {
                return None;
            }

            if attempt > 0 {
                tokio::time::sleep(LOGIN_POLL_INTERVAL).await;
            }

            let status = manager.get_account_status().await;
            let provider_status = status
                .get("providers")
                .and_then(|providers| providers.get(&provider));
            let account_count = provider_status
                .and_then(|s| s.get("account_count"))
                .and_then(|count| count.as_u64())
                .unwrap_or(0);

            if account_count > 0 {
                let account = provider_status
                    .and_then(|s| s.get("accounts"))
                    .and_then(|accounts| accounts.as_array())
                    .and_then(|accounts| accounts.first())
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let payload = json!({
                    "upstream": upstream_name,
                    "status": {
                        "status": "success",
                        "message": "登录成功",
                        "email": account.get("email").and_then(|v| v.as_str()).unwrap_or(""),
                        "expires_at": account.get("expiresAt").and_then(|v| v.as_str()).unwrap_or(""),
                        "lastRefresh": account.get("lastRefresh").and_then(|v| v.as_str()).unwrap_or("")
                    }
                });
                let chunk = format!("data: {}\n\n", payload);
                return Some((
                    Ok::<_, Error>(web::Bytes::from(chunk)),
                    (true, attempt + 1, manager, upstream_name, provider),
                ));
            }

            if attempt >= LOGIN_POLL_MAX_ATTEMPTS {
                let payload = json!({
                    "upstream": upstream_name,
                    "status": {
                        "status": "timeout",
                        "message": "登录超时，请重新发起登录"
                    }
                });
                let chunk = format!("data: {}\n\n", payload);
                return Some((
                    Ok::<_, Error>(web::Bytes::from(chunk)),
                    (true, attempt + 1, manager, upstream_name, provider),
                ));
            }

            Some((
                Ok::<_, Error>(web::Bytes::from(": keep-alive\n\n")),
                (false, attempt + 1, manager, upstream_name, provider),
            ))
        },
    );

    HttpResponse::Ok()
        .insert_header(("Content-Type", "text/event-stream"))
        .insert_header(("Cache-Control", "no-cache"))
        .streaming(stream)
}

/// 将 CLIProxyAPI 管理 API 响应转换为前端期望格式
pub(crate) fn transform_cli_proxy_api_status(body: &serde_json::Value) -> serde_json::Value {
    let mut providers = serde_json::Map::new();

    if let Some(files) = body.get("files").and_then(|f| f.as_array()) {
        for file in files {
            let provider = file.get("provider").and_then(|p| p.as_str()).unwrap_or("");
            let account = serde_json::json!({
                "email": file.get("email").and_then(|e| e.as_str()).unwrap_or(""),
                "status": file.get("status").and_then(|s| s.as_str()).unwrap_or("unknown"),
                "expiresAt": file.get("expiresAt").and_then(|e| e.as_str()).or_else(|| file.get("expired").and_then(|e| e.as_str())).unwrap_or(""),
                "lastRefresh": file.get("last_refresh").and_then(|e| e.as_str()).unwrap_or("")
            });

            let entry: serde_json::Value = providers
                .get(provider)
                .unwrap_or(&serde_json::json!({}))
                .clone();
            let mut accounts = entry
                .get("accounts")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or(vec![]);
            accounts.push(account);
            let mut new_entry = serde_json::Map::new();
            new_entry.insert(
                "account_count".to_string(),
                serde_json::json!(accounts.len()),
            );
            new_entry.insert("accounts".to_string(), serde_json::json!(accounts));
            providers.insert(provider.to_string(), serde_json::Value::Object(new_entry));
        }
    }

    serde_json::json!({"providers": providers})
}

/// CLIProxyAPI 状态：获取账号信息
pub(crate) async fn cli_proxy_api_status(
    state: web::Data<AppState>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let manager = match &state.cli_proxy_api_manager {
        Some(m) => m,
        None => {
            return HttpResponse::Ok().json(json!({"files": []}));
        }
    };

    if manager.is_running().await {
        let endpoint = manager.endpoint().await;
        let secret = manager.management_secret().await;

        let mut builder =
            reqwest::Client::new().get(format!("{}/v0/management/auth-files", endpoint));
        builder = builder.bearer_auth(&secret);

        match builder.send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        // CLIProxyAPI 返回 {"files": [{"provider":"codex","status":"active","email":"..."}]}
                        // 转换为 JS 期望的格式: {"providers": {"codex": {"account_count":1,"accounts":[...]}}}
                        let transformed = transform_cli_proxy_api_status(&body);
                        return HttpResponse::Ok().json(transformed);
                    }
                    Err(e) => {
                        return HttpResponse::InternalServerError().json(json!({
                            "error": format!("Failed to parse CLIProxyAPI response: {}", e)
                        }))
                    }
                }
            }
            _ => {
                // Fallback to file check below
            }
        }
    }

    // Fallback: check auth directory for account files
    let status = manager.get_account_status().await;
    HttpResponse::Ok().json(status)
}
