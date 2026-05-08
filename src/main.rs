use llm_wrapper::config;
use llm_wrapper::models;
use llm_wrapper::oauth::AuthManager;
use llm_wrapper::proxy;
use llm_wrapper::router;

use llm_wrapper::models::{ModelAliasSource, UpstreamAuth};
use llm_wrapper::proxy::DebugInfo;
use serde_json::json;

mod cli_proxy_api_manager;
mod cli_proxy_api_proxy;

use actix_cors::Cors;
use actix_files as fs;
use actix_web::{middleware, web, App, Error, HttpResponse, HttpServer};
use config::ConfigManager;
use futures::{Stream, StreamExt};
use models::AppConfig;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[allow(dead_code)]
struct AppState {
    config: ConfigManager,
    auth_manager: AuthManager,
    debug_data: web::Data<DebugDataStore>,
    stream_hub: web::Data<DebugStreamHub>,
    cli_proxy_api_manager: Option<Arc<cli_proxy_api_manager::CliProxyApiManager>>,
}

/// 调试数据存储
#[derive(Clone, Default)]
struct DebugDataStore {
    data: Arc<RwLock<Option<DebugInfo>>>,
}

impl DebugDataStore {
    async fn get(&self) -> Option<DebugInfo> {
        let guard = self.data.read().await;
        guard.clone()
    }
}

/// 调试流式广播中心
#[derive(Clone)]
struct DebugStreamHub {
    sender: Arc<tokio::sync::broadcast::Sender<String>>,
}

impl DebugStreamHub {
    fn new() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(100);
        Self {
            sender: Arc::new(sender),
        }
    }

    /// 创建 SSE 流
    fn create_stream(
        &self,
    ) -> Pin<Box<dyn Stream<Item = Result<actix_web::web::Bytes, Error>> + Send>> {
        let receiver = self.sender.subscribe();
        let stream = tokio_stream::wrappers::BroadcastStream::new(receiver)
            .map(|result| match result {
                Ok(chunk) => Ok(actix_web::web::Bytes::from(chunk)),
                Err(_) => Ok(actix_web::web::Bytes::from(
                    "data: {\"error\":\"connection reset\"}\n\n",
                )),
            })
            .boxed();
        stream
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // 初始化日志
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "llm_wrapper=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 初始化配置
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.yaml".to_string());
    let config_manager = ConfigManager::new(&config_path)
        .await
        .expect("无法加载配置");

    // 初始化认证管理器（token 缓存放在 config 同目录下，便于 Docker 持久化）
    let cache_dir = std::path::Path::new(&config_path)
        .parent()
        .map(|p| p.join(".llm-wrapper"))
        .unwrap_or_else(|| std::path::PathBuf::from(".llm-wrapper"));
    let auth_manager = AuthManager::new(Some(&cache_dir));
    auth_manager.load_cache().await;

    // 检查是否需要启动 CLIProxyAPI
    let cli_proxy_api_manager = {
        let config_snapshot = config_manager.get_config().await;
        let needs_cli_proxy_api = config_snapshot
            .upstreams
            .iter()
            .any(|u| u.enabled && u.auth.is_cli_proxy_api());

        if needs_cli_proxy_api {
            let cli_proxy_api_dir = std::path::Path::new(&config_path)
                .parent()
                .map(|p| p.join("cli-proxy-api"))
                .unwrap_or_else(|| std::path::PathBuf::from("cli-proxy-api"));

            let manager = cli_proxy_api_manager::CliProxyApiManager::new(
                cli_proxy_api_dir.clone(),
                config_snapshot.cli_proxy_api_endpoint.clone(),
            );

            let mgr = Arc::new(manager);
            // Spawn monitor task for crash recovery (always, even if not started yet)
            mgr.clone().spawn_monitor();

            // Only start CLIProxyAPI process if accounts already exist
            if mgr.has_accounts().await {
                if let Err(e) = mgr.start().await {
                    warn!("Failed to start CLIProxyAPI: {}. CLIProxyAPI upstreams will be unavailable.", e);
                }
            } else {
                info!("No CLIProxyAPI accounts found. Login via WebUI to add an account.");
            }

            Some(mgr)
        } else {
            None
        }
    };

    let debug_store = web::Data::new(DebugDataStore::default());
    let stream_hub = web::Data::new(DebugStreamHub::new());
    let state = web::Data::new(AppState {
        config: config_manager,
        auth_manager,
        debug_data: debug_store.clone(),
        stream_hub: stream_hub.clone(),
        cli_proxy_api_manager,
    });

    // 启动服务器
    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    info!("LLM Wrapper 启动在 http://{}", addr);
    info!("WebUI 访问 http://{}/", addr);
    info!("API 端点 http://{}/v1/chat/completions", addr);

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .app_data(web::JsonConfig::default().limit(32 * 1024 * 1024)) // 32MB，支持 256K token 上下文
            .wrap(middleware::Logger::default())
            .wrap(Cors::permissive().max_age(3600))
            // 配置 API
            .route("/api/config", web::get().to(get_config))
            .route("/api/config", web::put().to(update_config))
            .route("/api/upstream-models", web::get().to(get_upstream_models))
            .route(
                "/api/upstream-models/alias",
                web::post().to(create_upstream_model_alias),
            )
            .route(
                "/api/upstream-models/{upstream}",
                web::get().to(get_upstream_models_by_name),
            )
            // 认证 API
            .route(
                "/api/auth/login/{upstream_name}",
                web::post().to(auth_login),
            )
            .route(
                "/api/auth/token/{upstream_name}",
                web::delete().to(auth_clear_token),
            )
            // CLIProxyAPI 认证 API
            .route(
                "/api/cli-proxy-api/login/{upstream_name}",
                web::post().to(cli_proxy_api_login),
            )
            .route(
                "/api/cli-proxy-api/complete-login/{upstream_name}",
                web::post().to(cli_proxy_api_complete_login),
            )
            .route(
                "/api/cli-proxy-api/login-stream/{upstream_name}",
                web::get().to(cli_proxy_api_login_stream),
            )
            .route(
                "/api/cli-proxy-api/status",
                web::get().to(cli_proxy_api_status),
            )
            .route("/api/debug", web::get().to(get_debug_data))
            .route("/api/debug", web::delete().to(clear_debug_data))
            .route("/api/debug/stream", web::get().to(debug_stream))
            // API v1 路由
            .route("/v1/chat/completions", web::post().to(chat_completions))
            .route("/v1/responses", web::post().to(responses))
            .route("/v1/messages", web::post().to(messages))
            .route("/v1/models/", web::get().to(list_models))
            .route("/v1/models", web::get().to(list_models))
            // WebUI
            .route("/", web::get().to(webui_index))
            .service(fs::Files::new("/webui", "src/webui").index_file("index.html"))
    })
    .bind(&addr)?
    .run()
    .await
}

async fn get_config(state: web::Data<AppState>) -> HttpResponse {
    let config = state.config.get_config().await;
    HttpResponse::Ok().json(&config)
}

async fn update_config(state: web::Data<AppState>, body: web::Json<AppConfig>) -> HttpResponse {
    match state.config.update_config(body.into_inner()).await {
        Ok(_) => HttpResponse::Ok().json(json!({"success": true})),
        Err(e) => HttpResponse::InternalServerError()
            .json(json!({"success": false, "error": e.to_string()})),
    }
}

async fn chat_completions(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    handle_protocol_request(
        state,
        body.into_inner(),
        req,
        "/v1/chat/completions",
    )
    .await
}

async fn responses(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    handle_protocol_request(
        state,
        body.into_inner(),
        req,
        "/v1/responses",
    )
    .await
}

async fn messages(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    handle_protocol_request(
        state,
        body.into_inner(),
        req,
        "/v1/messages",
    )
    .await
}

/// 通用的协议请求处理器
/// CLIProxyAPI 上游转发到 CLIProxyAPI，其他上游直接代理
async fn handle_protocol_request(
    state: web::Data<AppState>,
    body: serde_json::Value,
    req: actix_web::HttpRequest,
    endpoint_path: &str,
) -> HttpResponse {
    use proxy::Proxy;
    use router::ModelRouter;

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
            info!("CLIProxyAPI not running, attempting lazy start...");
            if let Err(e) = manager.start().await {
                return HttpResponse::BadGateway().json(json!({
                    "error": {"message": format!("CLIProxyAPI is not running: {}. Please login first via the WebUI.", e)}
                }));
            }
        }

        let cli_proxy_api_key = Some(manager.api_key().await).or(route.cli_proxy_api_api_key.clone());
        return cli_proxy_api_proxy::proxy_to_cli_proxy_api(
            &route.cli_proxy_api_endpoint,
            cli_proxy_api_key.as_deref(),
            endpoint_path,
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
        req.uri().path()
    );

    match proxy
        .proxy_request_with_debug(
            &route,
            endpoint_path,
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

async fn get_debug_data(state: web::Data<AppState>) -> HttpResponse {
    if let Some(data) = state.debug_data.get().await {
        HttpResponse::Ok().json(data)
    } else {
        HttpResponse::Ok().json(json!({
            "client_request": null,
            "upstream_request": null,
            "upstream_response": null
        }))
    }
}

async fn clear_debug_data(state: web::Data<AppState>) -> HttpResponse {
    state.debug_data.data.write().await.take();
    HttpResponse::Ok().json(json!({"success": true}))
}

/// SSE 流式调试端点
async fn debug_stream(state: web::Data<AppState>) -> impl actix_web::Responder {
    let stream = state.stream_hub.create_stream();

    actix_web::HttpResponse::Ok()
        .content_type("text/event-stream")
        .body(actix_web::body::BodyStream::new(stream))
}

async fn list_models(state: web::Data<AppState>) -> HttpResponse {
    let config = state.config.get_config().await;
    let aliases = config.aliases;

    // 收集需要查询的上游（去重）
    let unique_upstreams: std::collections::HashSet<_> =
        aliases.iter().map(|a| a.upstream.as_str()).collect();

    // 并发获取所有上游的模型列表
    let auth_manager = state.auth_manager.clone();
    let cli_proxy_api_manager = state.cli_proxy_api_manager.clone();
    let futures: Vec<_> = unique_upstreams
        .iter()
        .filter_map(|upstream_name| {
            let up = config
                .upstreams
                .iter()
                .find(|up| up.name == *upstream_name && up.enabled)?;
            let _auth_manager = auth_manager.clone();
            let cli_proxy_api_manager = cli_proxy_api_manager.clone();
            Some(async move {
                let url = up.get_models_url();
                let upstream_name = up.name.clone();
                let auth = up.auth.clone();
                let mut request = reqwest::Client::new()
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(2));

                match &auth {
                    UpstreamAuth::ApiKey { key } => {
                        if let Some(k) = key {
                            if k != "none" && !k.is_empty() {
                                request = request.bearer_auth(k);
                            }
                        }
                    }
                    // CLIProxyAPI 上游的模型列表从 CLIProxyAPI 服务获取
                    UpstreamAuth::AnthropicOAuth | UpstreamAuth::CodexOAuth => {
                        let _provider = match &auth {
                            UpstreamAuth::AnthropicOAuth => "claude",
                            UpstreamAuth::CodexOAuth => "codex",
                            _ => unreachable!(),
                        };
                        if let Some(mgr) = &cli_proxy_api_manager {
                            if mgr.is_running().await {
                                let api_key = mgr.api_key().await;
                                let endpoint = mgr.endpoint().await;
                                request = reqwest::Client::new()
                                    .get(format!("{}/v1/models", endpoint))
                                    .timeout(std::time::Duration::from_secs(2))
                                    .bearer_auth(&api_key);
                            }
                        }
                    }
                }

                match request.send().await {
                    Ok(resp) => match resp.json::<serde_json::Value>().await {
                        Ok(body) => {
                            let mut models = Vec::new();
                            for (id, max_len) in extract_models_from_upstream_response(&body) {
                                models.push((up.name.clone(), id, max_len));
                            }
                            models
                        }
                        Err(e) => {
                            tracing::warn!("解析上游 {} 模型列表失败：{}", upstream_name, e);
                            Vec::new()
                        }
                    },
                    Err(e) => {
                        tracing::warn!("获取上游 {} 模型列表失败：{}", upstream_name, e);
                        Vec::new()
                    }
                }
            })
        })
        .collect();

    let results = futures::future::join_all(futures).await;

    // 构建 (upstream_name, target_model) -> max_model_len 的查找表
    let mut model_len_map: std::collections::HashMap<(String, String), u32> =
        std::collections::HashMap::new();
    for models in results {
        for (upstream, id, max_len) in models {
            if let Some(len) = max_len {
                model_len_map.insert((upstream, id), len);
            }
        }
    }

    // 构建响应，同时收集需要更新的 aliases
    let mut changed = false;
    let model_objects: Vec<_> = aliases
        .iter()
        .map(|a| {
            if let Some(&len) = model_len_map.get(&(a.upstream.clone(), a.target_model.clone())) {
                if a.max_model_len != Some(len) {
                    changed = true;
                }
                json!({
                    "id": a.alias,
                    "object": "model",
                    "created": 0,
                    "owned_by": a.upstream,
                    "max_model_len": len
                })
            } else {
                json!({
                    "id": a.alias,
                    "object": "model",
                    "created": 0,
                    "owned_by": a.upstream
                })
            }
        })
        .collect();

    // 如果 max_model_len 有变化，更新配置持久化
    if changed {
        let mut cfg = state.config.get_config().await;
        for a in &mut cfg.aliases {
            if let Some(&len) = model_len_map.get(&(a.upstream.clone(), a.target_model.clone())) {
                a.max_model_len = Some(len);
            }
        }
        let _ = state.config.update_config(cfg).await;
    }

    HttpResponse::Ok().json(json!({
        "object": "list",
        "data": model_objects
    }))
}

async fn get_upstream_models(state: web::Data<AppState>) -> HttpResponse {
    let config = state.config.get_config().await;

    // 并发获取所有启用上游的模型列表
    let auth_manager = state.auth_manager.clone();
    let futures: Vec<_> = config
        .upstreams
        .iter()
        .filter(|u| u.enabled)
        .map(|upstream| {
            let url = upstream.get_models_url();
            let name = upstream.name.clone();
            let auth = upstream.auth.clone();
            let _auth_manager = auth_manager.clone();

            async move {
                let mut request = reqwest::Client::new().get(&url);

                // 同时支持 ApiKey 与 OAuth 上游（如 codex）模型列表拉取
                let token = match &auth {
                    UpstreamAuth::ApiKey { key } => key
                        .as_ref()
                        .filter(|k| *k != "none" && !k.is_empty())
                        .cloned(),
                    // CLIProxyAPI 上游的模型列表从 CLIProxyAPI 获取
                    UpstreamAuth::AnthropicOAuth | UpstreamAuth::CodexOAuth => None,
                };

                if let Some(token) = token {
                    request = request.bearer_auth(token);
                }

                match request.send().await {
                    Ok(resp) => {
                        let mut models = Vec::new();
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            for (id, _) in extract_models_from_upstream_response(&body) {
                                models.push(id);
                            }
                        }
                        models
                    }
                    Err(e) => {
                        tracing::warn!("获取上游 {} 模型列表失败：{}", name, e);
                        Vec::new()
                    }
                }
            }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    let all_models: Vec<String> = results.into_iter().flatten().collect();

    HttpResponse::Ok().json(json!({
        "object": "list",
        "data": all_models
    }))
}

async fn webui_index() -> HttpResponse {
    HttpResponse::Found()
        .append_header(("Location", "/webui/index.html"))
        .finish()
}

/// 获取指定上游的模型列表（支持 OAuth token 注入）
async fn get_upstream_models_by_name(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    let upstream_name = path.into_inner();
    let config = state.config.get_config().await;

    let upstream = config
        .upstreams
        .iter()
        .find(|u| u.name == upstream_name && u.enabled);

    if upstream.is_none() {
        return HttpResponse::NotFound().json(json!({
            "error": "上游不存在或未启用"
        }));
    }
    let upstream = upstream.unwrap();

    let url = upstream.get_models_url();
    let mut request = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(5));

    match &upstream.auth {
        UpstreamAuth::ApiKey { key } => {
            if let Some(k) = key {
                if k != "none" && !k.is_empty() {
                    request = request.bearer_auth(k);
                }
            }
        }
        // CLIProxyAPI 上游的模型列表从 CLIProxyAPI 服务获取
        UpstreamAuth::AnthropicOAuth | UpstreamAuth::CodexOAuth => {
            let _provider = match &upstream.auth {
                UpstreamAuth::AnthropicOAuth => "claude",
                UpstreamAuth::CodexOAuth => "codex",
                _ => unreachable!(),
            };
            if let Some(mgr) = &state.cli_proxy_api_manager {
                if mgr.is_running().await {
                    let api_key = mgr.api_key().await;
                    let endpoint = mgr.endpoint().await;
                    request = reqwest::Client::new()
                        .get(format!("{}/v1/models", endpoint))
                        .timeout(std::time::Duration::from_secs(5))
                        .bearer_auth(&api_key);
                } else {
                    return HttpResponse::BadRequest().json(json!({
                        "error": format!("上游 '{}' 的 CLIProxyAPI 服务未运行", upstream.name)
                    }));
                }
            } else {
                return HttpResponse::BadRequest().json(json!({
                    "error": format!("上游 '{}' 的 CLIProxyAPI 未配置", upstream.name)
                }));
            }
        }
    }

    match request.send().await {
        Ok(resp) => {
            let mut models = Vec::new();
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                for (id, _) in extract_models_from_upstream_response(&body) {
                    models.push(id);
                }
            }
            HttpResponse::Ok().json(json!({
                "object": "list",
                "data": models
            }))
        }
        Err(e) => {
            if e.is_timeout() {
                HttpResponse::GatewayTimeout().json(json!({
                    "error": "请求上游超时"
                }))
            } else {
                HttpResponse::BadGateway().json(json!({
                    "error": format!("请求上游失败: {}", e)
                }))
            }
        }
    }
}

fn extract_models_from_upstream_response(body: &serde_json::Value) -> Vec<(String, Option<u32>)> {
    let mut models = Vec::new();

    // OpenAI-compatible format: {"data":[{"id":"...", "max_model_len": ...}]}
    if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        for model in data {
            if let Some(id) = model.get("id").and_then(|i| i.as_str()) {
                let max_len = model
                    .get("max_model_len")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                models.push((id.to_string(), max_len));
            }
        }
        return models;
    }

    // Codex format: {"models":[{"slug":"...", "context_window": ...}]}
    if let Some(data) = body.get("models").and_then(|d| d.as_array()) {
        for model in data {
            if let Some(slug) = model.get("slug").and_then(|i| i.as_str()) {
                let max_len = model
                    .get("max_context_window")
                    .or_else(|| model.get("context_window"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                models.push((slug.to_string(), max_len));
            }
        }
    }

    models
}

// === 认证 API 处理函数 ===

async fn auth_login(state: web::Data<AppState>, path: web::Path<String>) -> HttpResponse {
    let upstream_name = path.into_inner();
    let config = state.config.get_config().await;

    let upstream = config
        .upstreams
        .iter()
        .find(|u| u.name == upstream_name && u.enabled);

    if upstream.is_none() {
        return HttpResponse::NotFound().json(json!({
            "error": "上游不存在或未启用"
        }));
    }
    let upstream = upstream.unwrap();

    match &upstream.auth {
        UpstreamAuth::ApiKey { .. } => {
            return HttpResponse::BadRequest().json(json!({
                "error": "该上游不使用 OAuth 认证"
            }));
        }
        UpstreamAuth::AnthropicOAuth | UpstreamAuth::CodexOAuth => {
            return HttpResponse::BadRequest().json(json!({
                "error": format!("上游 '{}' 的登录由 CLIProxyAPI 管理，请使用 /api/cli-proxy-api/login/{}", upstream.name, upstream_name)
            }));
        }
    }
}

async fn auth_clear_token(state: web::Data<AppState>, path: web::Path<String>) -> HttpResponse {
    let upstream_name = path.into_inner();
    state.auth_manager.clear_token(&upstream_name).await;
    HttpResponse::Ok().json(json!({
        "success": true
    }))
}

/// CLIProxyAPI 登录：发起 OAuth 登录流程
async fn cli_proxy_api_login(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    let upstream_name = path.into_inner();

    // 验证上游存在且是 CLIProxyAPI 类型
    let config = state.config.get_config().await;
    let upstream = config
        .upstreams
        .iter()
        .find(|u| u.name == upstream_name && u.auth.is_cli_proxy_api());

    if upstream.is_none() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!("上游 '{}' 不存在或不是 CLIProxyAPI 类型", upstream_name)
        }));
    }

    let upstream = upstream.unwrap();

    // 将 auth 类型映射为 provider id
    let provider = match &upstream.auth {
        UpstreamAuth::AnthropicOAuth => "claude",
        UpstreamAuth::CodexOAuth => "codex",
        _ => unreachable!(),
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
async fn cli_proxy_api_complete_login(
    state: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
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
    let upstream = config
        .upstreams
        .iter()
        .find(|u| u.name == upstream_name && u.auth.is_cli_proxy_api());

    if upstream.is_none() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!("上游 '{}' 不存在或不是 CLIProxyAPI 类型", upstream_name)
        }));
    }

    let upstream = upstream.unwrap();

    let provider = match &upstream.auth {
        UpstreamAuth::AnthropicOAuth => "claude",
        UpstreamAuth::CodexOAuth => "codex",
        _ => unreachable!(),
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
        Err(e) => HttpResponse::InternalServerError().json(json!({
            "error": format!("Failed to complete login: {}", e)
        })),
    }
}

/// CLIProxyAPI 登录状态：SSE 推送登录完成事件
async fn cli_proxy_api_login_stream(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    let upstream_name = path.into_inner();

    let config = state.config.get_config().await;
    let upstream = config
        .upstreams
        .iter()
        .find(|u| u.name == upstream_name && u.auth.is_cli_proxy_api());

    if upstream.is_none() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!("上游 '{}' 不存在或不是 CLIProxyAPI 类型", upstream_name)
        }));
    }

    let _ = &upstream_name;
    HttpResponse::BadRequest().json(json!({
        "error": "Login stream not yet implemented. Use /api/cli-proxy-api/login/{name} first."
    }))
}

/// 将 CLIProxyAPI 管理 API 响应转换为前端期望格式
fn transform_cli_proxy_api_status(body: &serde_json::Value) -> serde_json::Value {
    let mut providers = serde_json::Map::new();

    if let Some(files) = body.get("files").and_then(|f| f.as_array()) {
        for file in files {
            let provider = file.get("provider").and_then(|p| p.as_str()).unwrap_or("");
            let account = serde_json::json!({
                "email": file.get("email").and_then(|e| e.as_str()).unwrap_or(""),
                "status": file.get("status").and_then(|s| s.as_str()).unwrap_or("unknown"),
                "expiresAt": file.get("expiresAt").and_then(|e| e.as_str()).unwrap_or("")
            });

            let entry: serde_json::Value =
                providers.get(provider).unwrap_or(&serde_json::json!({})).clone();
            let mut accounts = entry.get("accounts").and_then(|a| a.as_array()).cloned().unwrap_or(vec![]);
            accounts.push(account);
            let mut new_entry = serde_json::Map::new();
            new_entry.insert("account_count".to_string(), serde_json::json!(accounts.len()));
            new_entry.insert("accounts".to_string(), serde_json::json!(accounts));
            providers.insert(provider.to_string(), serde_json::Value::Object(new_entry));
        }
    }

    serde_json::json!({"providers": providers})
}

/// CLIProxyAPI 状态：获取账号信息
async fn cli_proxy_api_status(state: web::Data<AppState>) -> HttpResponse {
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
                    Err(e) => return HttpResponse::InternalServerError().json(json!({
                        "error": format!("Failed to parse CLIProxyAPI response: {}", e)
                    })),
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

/// 创建上游模型 alias 的请求
#[derive(Debug, serde::Deserialize)]
struct CreateAliasRequest {
    upstream: String,
    model: String,
}

/// 创建上游模型 auto alias
async fn create_upstream_model_alias(
    state: web::Data<AppState>,
    body: web::Json<CreateAliasRequest>,
) -> HttpResponse {
    let mut config = state.config.get_config().await;

    // 验证上游存在且启用
    let upstream = config
        .upstreams
        .iter()
        .find(|u| u.name == body.upstream && u.enabled);

    if upstream.is_none() {
        return HttpResponse::BadRequest().json(json!({
            "error": "上游不存在或未启用"
        }));
    }

    let upstream = upstream.unwrap();

    // 检查 alias 是否已存在（通过 alias 名）
    if config.aliases.iter().any(|a| a.alias == body.model) {
        return HttpResponse::Conflict().json(json!({
            "error": "别名已存在",
            "alias": body.model
        }));
    }

    // 注意：不检查 target_model 是否已存在
    // 原因：auto alias (alias=target_model) 和手动 alias (alias!=target_model) 可以共存
    // 用户可以通过选择不同的 alias 名来控制行为（是否带参数覆盖）

    // 创建 auto alias（透传：alias = target_model）
    config.aliases.push(models::ModelAlias {
        alias: body.model.clone(),
        target_model: body.model.clone(),
        upstream: upstream.name.clone(),
        param_overrides: vec![],
        source: ModelAliasSource::Auto,
        max_model_len: None,
    });

    // 保存配置
    if let Err(e) = state.config.update_config(config).await {
        return HttpResponse::InternalServerError().json(json!({
            "error": format!("保存配置失败：{}", e)
        }));
    }

    HttpResponse::Ok().json(json!({
        "success": true,
        "alias": body.model
    }))
}
