mod cli_proxy_api_manager;
mod cli_proxy_api_proxy;
mod handlers;
mod state;

use actix_cors::Cors;
use actix_files as fs;
use actix_web::{middleware, web, App, HttpServer};
use handlers::admin::{admin_login, admin_logout, admin_setup, admin_status};
use handlers::chat::{chat_completions, messages, responses};
use handlers::cli_proxy::{
    auth_clear_token, auth_login, cli_proxy_api_complete_login, cli_proxy_api_login,
    cli_proxy_api_login_stream, cli_proxy_api_status,
};
use handlers::config_api::{get_config, get_version, reveal_client_api_key, update_config};
use handlers::debug::{clear_debug_data, debug_stream, get_debug_data, webui_index};
use handlers::models_api::{
    create_upstream_model_alias, get_upstream_models, get_upstream_models_by_name, list_models,
    test_upstream_models,
};
use handlers::quota::cli_proxy_api_quota;
use llm_wrapper::config::ConfigManager;
use llm_wrapper::oauth::AuthManager;
use state::{
    AdminSessionStore, AppState, DebugDataStore, DebugStreamHub, LoginRateLimiter,
    JSON_PAYLOAD_LIMIT,
};
use std::sync::Arc;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn print_usage() {
    println!(
        "LLM Wrapper v{}\n\n\
         Usage: llm-wrapper [OPTIONS]\n\n\
         Options:\n\
           -c, --config <PATH>  Config file path (default: config.yaml)\n\
           -a, --addr <ADDR>    Bind address (default: 0.0.0.0:3000)\n\
           -v, --version        Print version\n\
           -h, --help           Print help\n\
         \n\
         Environment Variables:\n\
           CONFIG_PATH          Same as --config\n\
           BIND_ADDR            Same as --addr\n\
           RUST_LOG             Log level (default: info)",
        env!("CARGO_PKG_VERSION")
    );
}

fn print_version() {
    println!("llm-wrapper {}", env!("CARGO_PKG_VERSION"));
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // 解析命令行参数
    let args: Vec<String> = std::env::args().collect();
    let mut config_path = None;
    let mut bind_addr = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-v" | "--version" => {
                print_version();
                std::process::exit(0);
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "-c" | "--config" => {
                i += 1;
                if i < args.len() {
                    config_path = Some(args[i].clone());
                }
            }
            "-a" | "--addr" => {
                i += 1;
                if i < args.len() {
                    bind_addr = Some(args[i].clone());
                }
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    // 初始化日志
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,actix_web::middleware::logger=info,actix_server=warn".into()
            }),
        )
        .with(tracing_subscriber::fmt::layer().with_timer(
            tracing_subscriber::fmt::time::OffsetTime::new(
                time::UtcOffset::UTC,
                time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z"),
            ),
        ))
        .init();

    // 打印版本信息
    info!("LLM Wrapper v{}", env!("CARGO_PKG_VERSION"));

    // 命令行参数优先，其次环境变量，最后默认值
    let config_path = config_path
        .or_else(|| std::env::var("CONFIG_PATH").ok())
        .unwrap_or_else(|| "config.yaml".to_string());
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

    // 初始化 CLIProxyAPI manager（始终初始化，进程启动按需触发）
    let cli_proxy_api_manager = {
        let config_snapshot = config_manager.get_config().await;

        let cli_proxy_api_dir = std::path::Path::new(&config_path)
            .parent()
            .map(|p| p.join("cli-proxy-api"))
            .unwrap_or_else(|| std::path::PathBuf::from("cli-proxy-api"));

        let manager = cli_proxy_api_manager::CliProxyApiManager::new(
            cli_proxy_api_dir.clone(),
            config_snapshot.cli_proxy_api_endpoint.clone(),
        );

        let mgr = Arc::new(manager);
        // Spawn monitor task for crash recovery
        mgr.clone().spawn_monitor();

        // 仅有账号文件时才启动进程
        if mgr.has_accounts().await {
            if let Err(e) = mgr.start().await {
                warn!(
                    "Failed to start CLIProxyAPI: {}. CLIProxyAPI upstreams will be unavailable.",
                    e
                );
            }
        } else {
            info!("No CLIProxyAPI accounts found. Login via WebUI to add an account.");
        }

        Some(mgr)
    };

    let debug_store = web::Data::new(DebugDataStore::default());
    let stream_hub = web::Data::new(DebugStreamHub::new());
    let admin_sessions = web::Data::new(AdminSessionStore::default());
    // 保留一份 manager 引用用于信号处理
    let cli_proxy_api_manager_for_shutdown = cli_proxy_api_manager.clone();
    let state = web::Data::new(AppState {
        config: config_manager,
        auth_manager,
        debug_data: debug_store.clone(),
        stream_hub: stream_hub.clone(),
        cli_proxy_api_manager,
        admin_sessions: admin_sessions.clone(),
        login_rate_limiter: LoginRateLimiter::default(),
    });

    // 启动服务器
    let addr = bind_addr.unwrap_or_else(|| {
        std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string())
    });

    info!(
        "LLM Wrapper v{} 启动在 http://{}",
        env!("CARGO_PKG_VERSION"),
        addr
    );
    info!("WebUI 访问 http://{}/", addr);
    info!("API 端点 http://{}/v1/chat/completions", addr);

    let server = HttpServer::new(move || {
        // /v1/* 客户端 API：Bearer 认证，允许任意源但不带凭据（无 Allow-Credentials）
        let v1_cors = Cors::default()
            .allow_any_origin()
            .allow_any_header()
            .allowed_methods(vec!["GET", "POST", "OPTIONS"])
            .max_age(3600);

        App::new()
            .app_data(state.clone())
            .app_data(web::JsonConfig::default().limit(JSON_PAYLOAD_LIMIT))
            .wrap(middleware::Logger::default())
            // /api/* 管理 API：不挂 CORS 中间件，配合 SameSite=Strict cookie 仅限同源
            .service(
                web::scope("/api")
                    // 配置 API
                    .route("/config", web::get().to(get_config))
                    .route("/config", web::put().to(update_config))
                    .route(
                        "/client-api-keys/{index}/reveal",
                        web::get().to(reveal_client_api_key),
                    )
                    .route("/upstream-models", web::get().to(get_upstream_models))
                    .route(
                        "/upstream-models/test",
                        web::post().to(test_upstream_models),
                    )
                    .route(
                        "/upstream-models/alias",
                        web::post().to(create_upstream_model_alias),
                    )
                    .route(
                        "/upstream-models/{upstream}",
                        web::get().to(get_upstream_models_by_name),
                    )
                    // 认证 API
                    .route("/auth/login/{upstream_name}", web::post().to(auth_login))
                    .route(
                        "/auth/token/{upstream_name}",
                        web::delete().to(auth_clear_token),
                    )
                    // CLIProxyAPI 认证 API
                    .route(
                        "/cli-proxy-api/login/{upstream_name}",
                        web::post().to(cli_proxy_api_login),
                    )
                    .route(
                        "/cli-proxy-api/complete-login/{upstream_name}",
                        web::post().to(cli_proxy_api_complete_login),
                    )
                    .route(
                        "/cli-proxy-api/login-stream/{upstream_name}",
                        web::get().to(cli_proxy_api_login_stream),
                    )
                    .route("/cli-proxy-api/status", web::get().to(cli_proxy_api_status))
                    .route("/cli-proxy-api/quota", web::get().to(cli_proxy_api_quota))
                    .route("/version", web::get().to(get_version))
                    // 管理员认证 API
                    .route("/admin/status", web::get().to(admin_status))
                    .route("/admin/setup", web::post().to(admin_setup))
                    .route("/admin/login", web::post().to(admin_login))
                    .route("/admin/logout", web::post().to(admin_logout))
                    .route("/debug", web::get().to(get_debug_data))
                    .route("/debug", web::delete().to(clear_debug_data))
                    .route("/debug/stream", web::get().to(debug_stream)),
            )
            // API v1 路由
            .service(
                web::scope("/v1")
                    .wrap(v1_cors)
                    .route("/chat/completions", web::post().to(chat_completions))
                    .route("/responses", web::post().to(responses))
                    .route("/messages", web::post().to(messages))
                    .route("/messages/{tail:.*}", web::post().to(messages))
                    .route("/models/", web::get().to(list_models))
                    .route("/models", web::get().to(list_models)),
            )
            // WebUI
            .route("/", web::get().to(webui_index))
            .service(fs::Files::new("/webui", "src/webui").index_file("index.html"))
    })
    .bind(&addr)?
    .run();

    let server_handle = server.handle();

    // 信号处理：SIGINT/SIGTERM 时优雅关闭 CLIProxyAPI 和 HTTP 服务器
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("Received shutdown signal, stopping services...");

        // 先停止 CLIProxyAPI 子进程
        if let Some(ref mgr) = cli_proxy_api_manager_for_shutdown {
            mgr.stop().await;
        }

        // 优雅关闭 HTTP 服务器
        server_handle.stop(false).await;
    });

    server.await
}
