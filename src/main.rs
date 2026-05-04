use llm_wrapper::config;
use llm_wrapper::models;
use llm_wrapper::oauth::AuthManager;
use llm_wrapper::proxy;
use llm_wrapper::router;
use llm_wrapper::transform::Protocol;

use llm_wrapper::models::UpstreamAuth;
use llm_wrapper::proxy::apply_param_overrides_inner;
use llm_wrapper::proxy::DebugInfo;

use actix_cors::Cors;
use actix_files as fs;
use actix_web::{middleware, web, App, Error, HttpResponse, HttpServer};
use config::ConfigManager;
use futures::{Stream, StreamExt};
use models::AppConfig;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

struct AppState {
    config: ConfigManager,
    auth_manager: AuthManager,
    debug_data: web::Data<DebugDataStore>,
    stream_hub: web::Data<DebugStreamHub>,
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

    let debug_store = web::Data::new(DebugDataStore::default());
    let stream_hub = web::Data::new(DebugStreamHub::new());
    let state = web::Data::new(AppState {
        config: config_manager,
        auth_manager,
        debug_data: debug_store.clone(),
        stream_hub: stream_hub.clone(),
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
                "/api/auth/status/{upstream_name}",
                web::get().to(auth_status),
            )
            .route(
                "/api/auth/token/{upstream_name}",
                web::delete().to(auth_clear_token),
            )
            .route("/api/auth/login-stream", web::get().to(auth_login_stream))
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
        Protocol::ChatCompletions,
        "Chat Completions",
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
        Protocol::Responses,
        "Responses",
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
        Protocol::AnthropicMessages,
        "Anthropic Messages",
        "/v1/messages",
    )
    .await
}

/// 通用的协议请求处理器
/// 支持直接转发和协议转换两种模式
async fn handle_protocol_request(
    state: web::Data<AppState>,
    body: serde_json::Value,
    req: actix_web::HttpRequest,
    entry_protocol: Protocol,
    protocol_name: &str,
    endpoint_path: &str,
) -> HttpResponse {
    use proxy::Proxy;
    use router::ModelRouter;

    // 检查是否启用调试模式
    let debug_mode = req
        .headers()
        .get("X-Debug-Mode")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    // 获取模型名
    let model = match body.get("model").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return HttpResponse::BadRequest()
                .json(json!({"error": {"message": "缺少 model 参数"}}))
        }
    };

    let router = ModelRouter::new(state.config.clone());
    let proxy = Proxy::new(state.auth_manager.clone());

    // 查找路由
    let route = match router.route(&model).await {
        Some(r) => r,
        None => {
            return HttpResponse::BadRequest()
                .json(json!({"error": {"message": format!("找不到模型 {} 的路由", model)}}))
        }
    };

    // 确定是否需要协议转换
    let needs_conversion = !route.supports(entry_protocol);
    let target_protocol = if needs_conversion {
        let config_snapshot = state.config.get_config().await;
        if !config_snapshot.allow_protocol_conversion {
            return HttpResponse::UnprocessableEntity().json(json!({
                "error": {"message": format!("该上游不支持 {0} 协议", protocol_name)}
            }));
        }
        match route.best_available_protocol(entry_protocol) {
            Some(p) => p,
            None => {
                return HttpResponse::UnprocessableEntity().json(json!({
                    "error": {"message": "该上游不支持任何可用协议"}
                }))
            }
        }
    } else {
        entry_protocol
    };

    // 如果需要转换，先应用 alias 参数覆盖（在客户端协议字段名下），再转换
    let body_for_conversion = if needs_conversion {
        let mut b = body.clone();
        apply_param_overrides_inner(&mut b, &route);
        b
    } else {
        body.clone()
    };

    // 协议转换（不含图片下载）
    let mut upstream_body = if needs_conversion {
        match llm_wrapper::transform::convert_request_with_images(
            entry_protocol,
            target_protocol,
            &body_for_conversion,
        )
        .await
        {
            Ok(b) => b,
            Err(e) => {
                return HttpResponse::BadRequest().json(json!({
                    "error": {"message": format!("请求转换失败：{}", e)}
                }))
            }
        }
    } else {
        body.clone()
    };

    // 如果目标协议是 Anthropic，解析图片 URL 为 base64
    if needs_conversion && target_protocol == Protocol::AnthropicMessages {
        upstream_body =
            match llm_wrapper::transform::resolve_images_for_anthropic(&upstream_body).await {
                Ok(b) => b,
                Err(e) => {
                    return HttpResponse::BadRequest().json(json!({
                        "error": {"message": format!("图片下载失败：{}", e)}
                    }))
                }
            };
    }

    // 检查是否是流式请求
    let is_stream = upstream_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 提取客户端信息
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

    if needs_conversion && is_stream {
        // 流式协议转换（带调试）
        return handle_streaming_conversion(
            &proxy,
            &route,
            &upstream_body,
            target_protocol,
            entry_protocol,
            &body,
            client_ip,
            client_url,
            endpoint_path,
            &state.debug_data,
            &state.stream_hub,
        )
        .await;
    } else if needs_conversion {
        // 非流式协议转换（带调试）
        match proxy
            .proxy_request_raw(&route, "POST".to_string(), target_protocol, upstream_body.clone())
            .await
        {
            Ok((upstream_url, status, _headers, body_bytes)) => {
                // 保存调试信息
                let debug_info = DebugInfo {
                    client_request: body.clone(),
                    client_ip,
                    client_url,
                    endpoint: endpoint_path.to_string(),
                    upstream_url,
                    upstream_request: upstream_body.clone(),
                    upstream_response: serde_json::from_slice::<serde_json::Value>(&body_bytes)
                        .unwrap_or(serde_json::Value::Null),
                };
                state.debug_data.data.write().await.replace(debug_info);

                // 非 2xx 错误响应直接转发，不做协议转换
                if status >= 400 {
                    let status_code = actix_web::http::StatusCode::from_u16(status)
                        .unwrap_or(actix_web::http::StatusCode::BAD_GATEWAY);

                    // 尝试解析为 JSON 返回，否则返回原始文本
                    if let Ok(err_json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                        return HttpResponse::build(status_code).json(err_json);
                    }

                    return HttpResponse::build(status_code)
                        .content_type("text/plain")
                        .body(body_bytes);
                }

                let response_json = match serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                    Ok(j) => j,
                    Err(e) => {
                        return HttpResponse::BadGateway().json(json!({
                            "error": {"message": format!("解析上游响应失败：{}", e)}
                        }))
                    }
                };

                let converted = match llm_wrapper::transform::convert_response(
                    target_protocol,
                    entry_protocol,
                    &response_json,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        return HttpResponse::BadGateway().json(json!({
                            "error": {"message": format!("响应转换失败：{}", e)}
                        }))
                    }
                };

                // 调试模式下返回调试信息而非业务响应
                if debug_mode {
                    let debug_info = state.debug_data.get().await.unwrap_or_else(|| DebugInfo {
                        client_request: serde_json::Value::Null,
                        client_ip: String::new(),
                        client_url: String::new(),
                        endpoint: String::new(),
                        upstream_url: String::new(),
                        upstream_request: serde_json::Value::Null,
                        upstream_response: serde_json::Value::Null,
                    });
                    return HttpResponse::Ok().json(json!({
                        "debug": debug_info
                    }));
                }

                let status_code = actix_web::http::StatusCode::from_u16(status)
                    .unwrap_or(actix_web::http::StatusCode::OK);
                HttpResponse::build(status_code).json(converted)
            }
            Err(e) => {
                if e.contains("404") || e.contains("405") {
                    HttpResponse::BadGateway().json(json!({
                        "error": {
                            "message": format!("上游服务不支持目标端点，无法进行协议转换。错误：{}", e)
                        }
                    }))
                } else {
                    HttpResponse::BadGateway().json(json!({"error": {"message": e}}))
                }
            }
        }
    } else {
        // 直接转发 - 原有行为
        match proxy
            .proxy_request_with_debug(
                &route,
                "POST".to_string(),
                entry_protocol,
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
                    // 流式响应不返回 debug JSON，否则流不会被消费，广播也不会触发
                    // 调试信息通过 /api/debug 端点获取
                    let is_stream = resp
                        .headers()
                        .get(actix_web::http::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(|v| v.contains("text/event-stream"))
                        .unwrap_or(false);
                    if !is_stream {
                        let debug_info = state.debug_data.get().await.unwrap_or_else(|| DebugInfo {
                            client_request: serde_json::Value::Null,
                            client_ip: String::new(),
                            client_url: String::new(),
                            endpoint: String::new(),
                            upstream_url: String::new(),
                            upstream_request: serde_json::Value::Null,
                            upstream_response: serde_json::Value::Null,
                        });
                        return HttpResponse::Ok().json(json!({
                            "debug": debug_info
                        }));
                    }
                }
                resp
            }
            Err(e) => {
                // 对 responses/messages 端点提供更友好的错误信息
                if (entry_protocol == Protocol::Responses
                    || entry_protocol == Protocol::AnthropicMessages)
                    && (e.contains("404") || e.contains("405"))
                {
                    HttpResponse::BadGateway().json(json!({
                        "error": {
                            "message": format!("上游服务不支持 {0} API ({1})。错误：{2}", protocol_name, endpoint_path, e)
                        }
                    }))
                } else {
                    HttpResponse::BadGateway().json(json!({"error": {"message": e}}))
                }
            }
        }
    }
}

/// 处理流式协议转换：发送请求到上游，获取流式响应，转换 SSE 格式后返回
async fn handle_streaming_conversion(
    proxy: &llm_wrapper::proxy::Proxy,
    route: &llm_wrapper::router::RouteResult,
    upstream_body: &serde_json::Value,
    target_protocol: Protocol,
    entry_protocol: Protocol,
    client_request: &serde_json::Value,
    client_ip: String,
    client_url: String,
    endpoint_path: &str,
    debug_store: &DebugDataStore,
    stream_hub: &DebugStreamHub,
) -> HttpResponse {
    use llm_wrapper::transform::convert_stream_sse;

    // 使用 proxy 获取原始流式响应
    match proxy
        .proxy_request_stream_raw(
            route,
            "POST".to_string(),
            target_protocol,
            upstream_body.clone(),
        )
        .await
    {
        Ok((upstream_url, status, _headers, response)) => {
            if status == 404 || status == 405 {
                return HttpResponse::BadGateway().json(json!({
                    "error": {"message": format!("上游返回 {}", status)}
                }));
            }

            // 保存初始调试数据（流式响应，upstream_response 为 null）
            let debug_info = DebugInfo {
                client_request: client_request.clone(),
                client_ip,
                client_url,
                endpoint: endpoint_path.to_string(),
                upstream_url,
                upstream_request: upstream_body.clone(),
                upstream_response: serde_json::Value::Null,
            };
            debug_store.data.write().await.replace(debug_info);

            // 获取 stream_hub 用于广播转换后的 SSE chunk
            let hub = stream_hub.sender.clone();

            // 获取原始字节流并转换
            let raw_stream = response
                .bytes_stream()
                .map(|result: Result<_, reqwest::Error>| {
                    result
                        .map(Vec::from)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
                });

            let converted_stream = convert_stream_sse(target_protocol, entry_protocol, raw_stream);

            // 广播转换后的 SSE chunk 到调试前端
            let broadcast_stream = converted_stream.map(move |item| {
                if let Ok(ref bytes) = item {
                    if let Ok(text) = std::str::from_utf8(bytes) {
                        let _ = hub.send(text.to_string());
                    }
                }
                item
            });

            HttpResponse::Ok()
                .content_type("text/event-stream")
                .body(actix_web::body::BodyStream::new(broadcast_stream))
        }
        Err(e) => HttpResponse::BadGateway().json(json!({"error": {"message": e}})),
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
    let futures: Vec<_> = unique_upstreams
        .iter()
        .filter_map(|upstream_name| {
            let up = config
                .upstreams
                .iter()
                .find(|up| up.name == *upstream_name && up.enabled)?;
            let auth_manager = auth_manager.clone();
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
                    UpstreamAuth::OAuthDevice { .. } => {
                        if let Some(token) =
                            auth_manager.get_access_token(&upstream_name, &auth).await
                        {
                            if !token.is_empty() && token != "none" {
                                request = request.bearer_auth(token);
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
            let auth_manager = auth_manager.clone();

            async move {
                let mut request = reqwest::Client::new().get(&url);

                // 同时支持 ApiKey 与 OAuth 上游（如 codex）模型列表拉取
                let token = match &auth {
                    UpstreamAuth::ApiKey { key } => key
                        .as_ref()
                        .filter(|k| *k != "none" && !k.is_empty())
                        .cloned(),
                    UpstreamAuth::OAuthDevice { .. } => auth_manager
                        .get_access_token(&name, &auth)
                        .await
                        .filter(|t| !t.is_empty() && t != "none"),
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

use serde_json::json;

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
        UpstreamAuth::OAuthDevice { .. } => {
            // 尝试获取 OAuth token
            if let Some(token) = state
                .auth_manager
                .get_access_token(&upstream.name, &upstream.auth)
                .await
            {
                if !token.is_empty() && token != "none" {
                    request = request.bearer_auth(&token);
                } else {
                    return HttpResponse::Unauthorized().json(json!({
                        "error": format!("上游 '{}' 未登录，请先完成 OAuth 登录", upstream.name)
                    }));
                }
            } else {
                return HttpResponse::Unauthorized().json(json!({
                    "error": format!("上游 '{}' 没有有效的访问令牌", upstream.name)
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

    let (client_id, device_auth_url, token_url, scope) = match &upstream.auth {
        UpstreamAuth::OAuthDevice {
            client_id,
            device_auth_url,
            token_url,
            scope,
        } => (
            client_id.as_str(),
            device_auth_url.as_str(),
            token_url.as_str(),
            scope.as_deref(),
        ),
        UpstreamAuth::ApiKey { .. } => {
            return HttpResponse::BadRequest().json(json!({
                "error": "该上游不使用 OAuth 认证"
            }));
        }
    };

    match state
        .auth_manager
        .initiate_device_auth(&upstream.name, client_id, device_auth_url, token_url, scope)
        .await
    {
        Ok(session) => HttpResponse::Ok().json(json!({
            "status": "pending",
            "user_code": session.user_code,
            "verification_uri": session.verification_uri,
            "verification_uri_complete": session.verification_uri_complete,
            "expires_at": session.expires_at.to_rfc3339(),
        })),
        Err(e) => HttpResponse::InternalServerError().json(json!({
            "error": e
        })),
    }
}

async fn auth_status(state: web::Data<AppState>, path: web::Path<String>) -> HttpResponse {
    let upstream_name = path.into_inner();
    let status = state.auth_manager.get_token_status(&upstream_name).await;
    HttpResponse::Ok().json(status)
}

async fn auth_clear_token(state: web::Data<AppState>, path: web::Path<String>) -> HttpResponse {
    let upstream_name = path.into_inner();
    state.auth_manager.clear_token(&upstream_name).await;
    HttpResponse::Ok().json(json!({
        "success": true
    }))
}

/// SSE 流：推送 OAuth 登录完成事件
async fn auth_login_stream(state: web::Data<AppState>) -> impl actix_web::Responder {
    let receiver = state.auth_manager.subscribe_completion();
    let stream: Pin<Box<dyn Stream<Item = Result<actix_web::web::Bytes, Error>> + Send>> =
        tokio_stream::wrappers::BroadcastStream::new(receiver)
            .map(|result| match result {
                Ok((upstream_name, status)) => {
                    let json = serde_json::to_string(&json!({
                        "type": "status",
                        "upstream": upstream_name,
                        "status": status,
                    }))
                    .unwrap_or_default();
                    Ok(actix_web::web::Bytes::from(format!("data: {}\n\n", json)))
                }
                Err(_) => Ok(actix_web::web::Bytes::from(
                    "data: {\"error\":\"connection reset\"}\n\n",
                )),
            })
            .boxed();

    actix_web::HttpResponse::Ok()
        .content_type("text/event-stream")
        .body(actix_web::body::BodyStream::new(stream))
}
use models::ModelAliasSource;

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
