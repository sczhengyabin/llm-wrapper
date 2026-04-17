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

    // 代理请求（调试数据在 proxy 内部保存）
    match proxy.proxy_request_with_debug(
        &route,
        "POST".to_string(),
        "/v1/chat/completions".to_string(),
        body.into_inner(),
        Some(state.debug_data.data.clone()),
        Some(state.stream_hub.sender.clone()),
    ).await {
        Ok(resp) => {
            if debug_mode {
                // 返回调试信息（只返回调试数据）
                let debug_info = state.debug_data.get().await.unwrap_or_else(|| DebugInfo {
                    client_request: serde_json::Value::Null,
                    endpoint: String::new(),
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

    // 直接转发到上游的 /v1/responses 端点
    let original_body = body.into_inner();

    match proxy.proxy_request_with_debug(
        &route,
        "POST".to_string(),
        "/v1/responses".to_string(),
        original_body,
        Some(state.debug_data.data.clone()),
        Some(state.stream_hub.sender.clone()),
    ).await {
        Ok(resp) => {
            if debug_mode {
                // 返回调试信息（只返回调试数据）
                let debug_info = state.debug_data.get().await.unwrap_or_else(|| DebugInfo {
                    client_request: serde_json::Value::Null,
                    endpoint: String::new(),
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

    // 直接转发到上游的 /v1/messages 端点
    let original_body = body.into_inner();

    match proxy.proxy_request_with_debug(
        &route,
        "POST".to_string(),
        "/v1/messages".to_string(),
        original_body,
        Some(state.debug_data.data.clone()),
        Some(state.stream_hub.sender.clone()),
    ).await {
        Ok(resp) => {
            if debug_mode {
                // 返回调试信息（只返回调试数据）
                let debug_info = state.debug_data.get().await.unwrap_or_else(|| DebugInfo {
                    client_request: serde_json::Value::Null,
                    endpoint: String::new(),
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
    use crate::router::ModelRouter;

    let router = ModelRouter::new(state.config.clone());
    let models = router.get_models().await;

    let model_objects: Vec<_> = models
        .iter()
        .map(|m| {
            json!({
                "id": m,
                "object": "model",
                "created": 0,
                "owned_by": "llm-wrapper"
            })
        })
        .collect();

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
