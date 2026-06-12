use actix_web::{web, HttpResponse};
use llm_wrapper::models::{self, ModelAliasSource, UpstreamAuth, UpstreamConfig};
use serde_json::json;
use std::sync::Arc;

use crate::handlers::admin::require_admin;
use crate::handlers::require_client_api_key;
use crate::state::{
    AppState, MODELS_FETCH_TIMEOUT_AGGREGATE, MODELS_FETCH_TIMEOUT_SINGLE, UPSTREAM_TEST_TIMEOUT,
};

#[derive(Debug, serde::Deserialize)]
pub(crate) struct TestUpstreamModelsRequest {
    base_url: String,
    api_key: Option<String>,
    /// 已保存上游的名称：api_key 为掩码值时用存储的真实 key 测试
    upstream: Option<String>,
}

#[derive(Debug)]
pub(crate) enum UpstreamFetchError {
    CliProxyNotRunning,
    CliProxyNotConfigured,
    Timeout,
    Http(String),
}

impl std::fmt::Display for UpstreamFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CliProxyNotRunning => write!(f, "CLIProxyAPI 服务未运行"),
            Self::CliProxyNotConfigured => write!(f, "CLIProxyAPI 未配置"),
            Self::Timeout => write!(f, "请求上游超时"),
            Self::Http(e) => write!(f, "{}", e),
        }
    }
}

pub(crate) async fn list_models(
    state: web::Data<AppState>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_client_api_key(&state, &req).await {
        return resp;
    }
    let config = state.config.get_config().await;
    let aliases = config.aliases;

    // 收集需要查询的上游（去重）
    let unique_upstreams: std::collections::HashSet<_> =
        aliases.iter().map(|a| a.upstream.as_str()).collect();

    // 并发获取所有上游的模型列表
    let cli_proxy_api_manager = state.cli_proxy_api_manager.clone();
    let futures: Vec<_> = unique_upstreams
        .iter()
        .filter_map(|upstream_name| {
            let up = config
                .upstreams
                .iter()
                .find(|up| up.name == *upstream_name && up.enabled)?;
            let cli_proxy_api_manager = cli_proxy_api_manager.clone();
            Some(async move {
                match fetch_upstream_models(
                    up,
                    cli_proxy_api_manager.as_ref(),
                    MODELS_FETCH_TIMEOUT_AGGREGATE,
                )
                .await
                {
                    Ok(models) => models
                        .into_iter()
                        .map(|(id, max_len)| (up.name.clone(), id, max_len))
                        .collect(),
                    Err(e) => {
                        tracing::warn!("获取上游 {} 模型列表失败：{}", up.name, e);
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
        if let Err(e) = state.config.update_config(cfg).await {
            tracing::warn!("持久化 max_model_len 失败: {}", e);
        }
    }

    HttpResponse::Ok().json(json!({
        "object": "list",
        "data": model_objects
    }))
}

pub(crate) async fn test_upstream_models(
    state: web::Data<AppState>,
    body: web::Json<TestUpstreamModelsRequest>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }

    let base_url = body.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "基础 URL 不能为空"
        }));
    }

    let mut api_key = body
        .api_key
        .as_ref()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty() && k != "none");

    // 掩码值不能用于上游认证，回退到已保存上游的真实 key
    if api_key
        .as_deref()
        .is_some_and(crate::handlers::config_api::is_masked)
    {
        let stored = match &body.upstream {
            Some(name) => {
                let config = state.config.get_config().await;
                config.upstreams.iter().find_map(|u| {
                    if u.name != *name {
                        return None;
                    }
                    match &u.auth {
                        UpstreamAuth::ApiKey { key } => key.clone(),
                        _ => None,
                    }
                })
            }
            None => None,
        };
        match stored {
            Some(real) => api_key = Some(real),
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "error": "API Key 是掩码值且找不到已保存的原始值，请重新输入完整 Key"
                }));
            }
        }
    }

    let mut request = reqwest::Client::new()
        .get(format!("{}/v1/models", base_url))
        .timeout(UPSTREAM_TEST_TIMEOUT);

    if let Some(key) = api_key.as_deref().filter(|k| !k.is_empty() && *k != "none") {
        request = request.bearer_auth(key);
    }

    match request.send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                return HttpResponse::BadGateway().json(json!({
                    "error": format!("上游请求失败：{}", status.as_u16())
                }));
            }

            match resp.json::<serde_json::Value>().await {
                Ok(body) => {
                    let models: Vec<String> = extract_models_from_upstream_response(&body)
                        .into_iter()
                        .map(|(id, _)| id)
                        .collect();
                    HttpResponse::Ok().json(json!({
                        "object": "list",
                        "data": models
                    }))
                }
                Err(e) => HttpResponse::BadGateway().json(json!({
                    "error": format!("解析上游模型列表失败: {}", e)
                })),
            }
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

pub(crate) async fn get_upstream_models(
    state: web::Data<AppState>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let config = state.config.get_config().await;

    // 并发获取所有启用上游的模型列表
    let cli_proxy_api_manager = state.cli_proxy_api_manager.clone();
    let futures: Vec<_> = config
        .upstreams
        .iter()
        .filter(|u| u.enabled)
        .map(|upstream| {
            let cli_proxy_api_manager = cli_proxy_api_manager.clone();
            async move {
                match fetch_upstream_models(
                    upstream,
                    cli_proxy_api_manager.as_ref(),
                    MODELS_FETCH_TIMEOUT_SINGLE,
                )
                .await
                {
                    Ok(models) => models.into_iter().map(|(id, _)| id).collect(),
                    Err(e) => {
                        tracing::warn!("获取上游 {} 模型列表失败：{}", upstream.name, e);
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

/// 获取指定上游的模型列表（支持 OAuth token 注入）
pub(crate) async fn get_upstream_models_by_name(
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

    match fetch_upstream_models(
        upstream,
        state.cli_proxy_api_manager.as_ref(),
        MODELS_FETCH_TIMEOUT_SINGLE,
    )
    .await
    {
        Ok(models) => {
            let ids: Vec<String> = models.into_iter().map(|(id, _)| id).collect();
            HttpResponse::Ok().json(json!({
                "object": "list",
                "data": ids
            }))
        }
        Err(UpstreamFetchError::CliProxyNotRunning) => HttpResponse::BadRequest().json(json!({
            "error": format!("上游 '{}' 的 CLIProxyAPI 服务未运行", upstream.name)
        })),
        Err(UpstreamFetchError::CliProxyNotConfigured) => HttpResponse::BadRequest().json(json!({
            "error": format!("上游 '{}' 的 CLIProxyAPI 未配置", upstream.name)
        })),
        Err(UpstreamFetchError::Timeout) => HttpResponse::GatewayTimeout().json(json!({
            "error": "请求上游超时"
        })),
        Err(e) => HttpResponse::BadGateway().json(json!({
            "error": format!("请求上游失败: {}", e)
        })),
    }
}

fn cli_proxy_api_model_matches_auth(auth: &UpstreamAuth, model_id: &str) -> bool {
    match auth {
        UpstreamAuth::AnthropicOAuth => model_id.starts_with("claude-"),
        UpstreamAuth::CodexOAuth => !model_id.starts_with("claude-"),
        UpstreamAuth::ApiKey { .. } => true,
    }
}

/// 拉取单个上游的模型列表，统一处理 ApiKey / CLIProxyAPI(OAuth) 两种认证分支
pub(crate) async fn fetch_upstream_models(
    upstream: &UpstreamConfig,
    cli_proxy_api_manager: Option<&Arc<crate::cli_proxy_api_manager::CliProxyApiManager>>,
    timeout: std::time::Duration,
) -> Result<Vec<(String, Option<u32>)>, UpstreamFetchError> {
    let request = match &upstream.auth {
        UpstreamAuth::ApiKey { key } => {
            let mut request = reqwest::Client::new()
                .get(upstream.get_models_url())
                .timeout(timeout);
            if let Some(k) = key.as_ref().filter(|k| *k != "none" && !k.is_empty()) {
                request = request.bearer_auth(k);
            }
            request
        }
        // CLIProxyAPI 上游的模型列表从 CLIProxyAPI 服务获取
        UpstreamAuth::AnthropicOAuth | UpstreamAuth::CodexOAuth => {
            let Some(mgr) = cli_proxy_api_manager else {
                return Err(UpstreamFetchError::CliProxyNotConfigured);
            };
            if !mgr.is_running().await {
                return Err(UpstreamFetchError::CliProxyNotRunning);
            }
            let api_key = mgr.api_key().await;
            let endpoint = mgr.endpoint().await;
            let mut request = reqwest::Client::new()
                .get(format!("{}/v1/models", endpoint))
                .timeout(timeout)
                .bearer_auth(&api_key);
            if matches!(&upstream.auth, UpstreamAuth::AnthropicOAuth) {
                request = request.header("User-Agent", "claude-cli/llm-wrapper");
            }
            request
        }
    };

    let resp = request.send().await.map_err(|e| {
        if e.is_timeout() {
            UpstreamFetchError::Timeout
        } else {
            UpstreamFetchError::Http(e.to_string())
        }
    })?;
    let body = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| UpstreamFetchError::Http(format!("解析模型列表失败: {}", e)))?;

    Ok(extract_models_from_upstream_response(&body)
        .into_iter()
        .filter(|(id, _)| cli_proxy_api_model_matches_auth(&upstream.auth, id))
        .collect())
}

pub(crate) fn extract_models_from_upstream_response(
    body: &serde_json::Value,
) -> Vec<(String, Option<u32>)> {
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
                    .get("max_model_len")
                    .or_else(|| model.get("context_window"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                models.push((slug.to_string(), max_len));
            }
        }
    }

    models
}

/// 创建上游模型 alias 的请求
#[derive(Debug, serde::Deserialize)]
pub(crate) struct CreateAliasRequest {
    upstream: String,
    model: String,
}

/// 创建上游模型 auto alias
pub(crate) async fn create_upstream_model_alias(
    state: web::Data<AppState>,
    body: web::Json<CreateAliasRequest>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let mut config = state.config.get_config().await;

    // 验证上游存在且启用
    let Some(upstream) = config
        .upstreams
        .iter()
        .find(|u| u.name == body.upstream && u.enabled)
    else {
        return HttpResponse::BadRequest().json(json!({
            "error": "上游不存在或未启用"
        }));
    };

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
