use llm_wrapper::config;
use llm_wrapper::models;
use llm_wrapper::proxy;
use llm_wrapper::router;

use llm_wrapper::proxy::DebugInfo;

use actix_files as fs;
use actix_web::{web, App, HttpServer, HttpResponse, middleware, Error};
use config::ConfigManager;
use models::AppConfig;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tracing::info;
use std::sync::Arc;
use tokio::sync::RwLock;
use futures::{Stream, StreamExt};
use std::pin::Pin;

struct AppState {
    config: ConfigManager,
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
    fn create_stream(&self) -> Pin<Box<dyn Stream<Item = Result<actix_web::web::Bytes, Error>> + Send>> {
        let receiver = self.sender.subscribe();
        let stream = tokio_stream::wrappers::BroadcastStream::new(receiver)
            .map(|result| {
                match result {
                    Ok(chunk) => Ok(actix_web::web::Bytes::from(chunk)),
                    Err(_) => Ok(actix_web::web::Bytes::from("data: {\"error\":\"connection reset\"}\n\n")),
                }
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
    let config_manager = ConfigManager::new(&config_path).await.expect("无法加载配置");

    let debug_store = web::Data::new(DebugDataStore::default());
    let stream_hub = web::Data::new(DebugStreamHub::new());
    let state = web::Data::new(AppState {
        config: config_manager,
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
            // 配置 API
            .route("/api/config", web::get().to(get_config))
            .route("/api/config", web::put().to(update_config))
            .route("/api/upstream-models", web::get().to(get_upstream_models))
            .route("/api/upstream-models/alias", web::post().to(create_upstream_model_alias))
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
        Err(e) => HttpResponse::InternalServerError().json(json!({"success": false, "error": e.to_string()})),
    }
}

async fn chat_completions(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    use crate::proxy::Proxy;
    use crate::router::ModelRouter;

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
        None => return HttpResponse::BadRequest().json(json!({"error": {"message": "缺少 model 参数"}})),
    };

    let router = ModelRouter::new(state.config.clone());
    let proxy = Proxy::new();

    // 查找路由
    let route = match router.route(&model).await {
        Some(r) => r,
        None => return HttpResponse::BadRequest().json(json!({"error": {"message": format!("找不到模型 {} 的路由", model)}})),
    };

    // 检查协议支持
    if !route.support_openai {
        return HttpResponse::UnprocessableEntity().json(json!({
            "error": {"message": "该上游不支持 OpenAI 协议"}
        }));
    }

    // 提取客户端信息
    let client_ip = req.peer_addr()
        .map(|p| p.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let client_url = format!(
        "{}://{}{}",
        req.connection_info().scheme(),
        req.connection_info().host(),
        req.uri().path()
    );

    // 代理请求（调试数据在 proxy 内部保存）
    match proxy.proxy_request_with_debug(
        &route,
        "POST".to_string(),
        "/v1/chat/completions".to_string(),
        body.into_inner(),
        client_ip,
        client_url,
        Some(state.debug_data.data.clone()),
        Some(state.stream_hub.sender.clone()),
    ).await {
        Ok(resp) => {
            if debug_mode {
                // 返回调试信息（只返回调试数据）
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
            resp
        }
        Err(e) => HttpResponse::BadGateway().json(json!({"error": {"message": e}})),
    }
}

async fn responses(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    use crate::proxy::Proxy;
    use crate::router::ModelRouter;

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
        None => return HttpResponse::BadRequest().json(json!({"error": {"message": "缺少 model 参数"}})),
    };

    let router = ModelRouter::new(state.config.clone());
    let proxy = Proxy::new();

    // 查找路由
    let route = match router.route(&model).await {
        Some(r) => r,
        None => return HttpResponse::BadRequest().json(json!({"error": {"message": format!("找不到模型 {} 的路由", model)}})),
    };

    // 检查协议支持
    if !route.support_openai {
        return HttpResponse::UnprocessableEntity().json(json!({
            "error": {"message": "该上游不支持 OpenAI 协议"}
        }));
    }

    // 提取客户端信息
    let client_ip = req.peer_addr()
        .map(|p| p.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let client_url = format!(
        "{}://{}{}",
        req.connection_info().scheme(),
        req.connection_info().host(),
        req.uri().path()
    );

    // 直接转发到上游的 /v1/responses 端点
    let original_body = body.into_inner();

    match proxy.proxy_request_with_debug(
        &route,
        "POST".to_string(),
        "/v1/responses".to_string(),
        original_body,
        client_ip,
        client_url,
        Some(state.debug_data.data.clone()),
        Some(state.stream_hub.sender.clone()),
    ).await {
        Ok(resp) => {
            if debug_mode {
                // 返回调试信息（只返回调试数据）
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
            resp
        }
        Err(e) => {
            // 检查是否是上游不支持 Responses API 的错误
            if e.contains("404") || e.contains("405") {
                HttpResponse::BadGateway().json(json!({
                    "error": {
                        "message": format!("上游服务不支持 Responses API (/v1/responses)，请使用 Chat Completions API (/v1/chat/completions)。错误：{}", e)
                    }
                }))
            } else {
                HttpResponse::BadGateway().json(json!({"error": {"message": e}}))
            }
        }
    }
}

async fn messages(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    use crate::proxy::Proxy;
    use crate::router::ModelRouter;

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
        None => return HttpResponse::BadRequest().json(json!({"error": {"message": "缺少 model 参数"}})),
    };

    let router = ModelRouter::new(state.config.clone());
    let proxy = Proxy::new();

    // 查找路由
    let route = match router.route(&model).await {
        Some(r) => r,
        None => return HttpResponse::BadRequest().json(json!({"error": {"message": format!("找不到模型 {} 的路由", model)}})),
    };

    // 检查协议支持
    if !route.support_anthropic {
        return HttpResponse::UnprocessableEntity().json(json!({
            "error": {"message": "该上游不支持 Anthropic 协议"}
        }));
    }

    // 提取客户端信息
    let client_ip = req.peer_addr()
        .map(|p| p.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let client_url = format!(
        "{}://{}{}",
        req.connection_info().scheme(),
        req.connection_info().host(),
        req.uri().path()
    );

    // 直接转发到上游的 /v1/messages 端点
    let original_body = body.into_inner();

    match proxy.proxy_request_with_debug(
        &route,
        "POST".to_string(),
        "/v1/messages".to_string(),
        original_body,
        client_ip,
        client_url,
        Some(state.debug_data.data.clone()),
        Some(state.stream_hub.sender.clone()),
    ).await {
        Ok(resp) => {
            if debug_mode {
                // 返回调试信息（只返回调试数据）
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
            resp
        }
        Err(e) => {
            // 检查是否是上游不支持 Messages API 的错误
            if e.contains("404") || e.contains("405") {
                HttpResponse::BadGateway().json(json!({
                    "error": {
                        "message": format!("上游服务不支持 Messages API (/v1/messages)。错误：{}", e)
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
async fn debug_stream(
    state: web::Data<AppState>,
) -> impl actix_web::Responder {
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

    // 构建 (upstream_name, target_model) -> max_model_len 的查找表
    let mut model_len_map: std::collections::HashMap<(String, String), u32> =
        std::collections::HashMap::new();

    for upstream_name in unique_upstreams {
        let u = if let Some(up) = config
            .upstreams
            .iter()
            .find(|up| up.name == upstream_name && up.enabled)
        {
            up
        } else {
            continue;
        };

        let url = format!("{}/v1/models", u.base_url);
        let mut request = reqwest::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_secs(2));
        if let Some(api_key) = &u.api_key {
            if api_key != "none" && !api_key.is_empty() {
                request = request.bearer_auth(api_key);
            }
        }

        match request.send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(body) => {
                    if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
                        for model in data {
                            if let Some(id) = model.get("id").and_then(|i| i.as_str()) {
                                if let Some(max_len) = model
                                    .get("max_model_len")
                                    .and_then(|v| v.as_u64())
                                    .map(|v| v as u32)
                                {
                                    model_len_map
                                        .insert((u.name.clone(), id.to_string()), max_len);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("解析上游 {} 模型列表失败：{}", upstream_name, e);
                }
            },
            Err(e) => {
                tracing::warn!("获取上游 {} 模型列表失败：{}", upstream_name, e);
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
    let mut all_models = Vec::new();

    // 遍历所有启用的上游，获取它们的模型列表
    for upstream in &config.upstreams {
        if !upstream.enabled {
            continue;
        }

        // 直接使用 reqwest 发送 GET 请求到上游的 /v1/models
        let url = format!("{}/v1/models", upstream.base_url);
        let mut request = reqwest::Client::new().get(&url);

        // 添加 API 密钥（如果有）
        if let Some(api_key) = &upstream.api_key {
            if api_key != "none" && !api_key.is_empty() {
                request = request.bearer_auth(api_key);
            }
        }

        match request.send().await {
            Ok(resp) => {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
                        for model in data {
                            if let Some(id) = model.get("id").and_then(|i| i.as_str()) {
                                all_models.push(id.to_string());
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("获取上游 {} 模型列表失败：{}", upstream.name, e);
            }
        }
    }

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
    let upstream = config.upstreams.iter()
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
