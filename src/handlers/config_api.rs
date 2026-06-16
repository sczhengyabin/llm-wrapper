use actix_web::{web, HttpRequest, HttpResponse};
use llm_wrapper::models::{AppConfig, UpstreamAuth};
use serde_json::json;

use crate::handlers::admin::require_admin;
use crate::handlers::normalize_client_api_keys;
use crate::state::AppState;

const MASK_CHAR: char = '•';
const MASK_FILL: &str = "••••••";

pub(crate) fn mask_secret(key: &str) -> String {
    if key.chars().count() <= 8 {
        MASK_FILL.to_string()
    } else {
        let chars: Vec<char> = key.chars().collect();
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{}{}{}", head, MASK_FILL, tail)
    }
}

pub(crate) fn is_masked(value: &str) -> bool {
    value.contains(MASK_CHAR)
}

/// 对返回给前端的配置脱敏（上游 key、客户端 key、CLIProxyAPI key）
pub(crate) fn mask_config_secrets(config: &mut AppConfig) {
    for upstream in &mut config.upstreams {
        if let UpstreamAuth::ApiKey { key: Some(k) } = &mut upstream.auth {
            if !k.is_empty() && k != "none" {
                *k = mask_secret(k);
            }
        }
    }
    for item in &mut config.client_api_keys {
        item.key = mask_secret(&item.key);
    }
    if let Some(k) = &mut config.cli_proxy_api_api_key {
        if !k.is_empty() {
            *k = mask_secret(k);
        }
    }
}

/// 将提交配置中的掩码值还原为当前存储的真实值；无法匹配时返回错误（避免掩码存盘）
pub(crate) fn restore_masked_secrets(
    new: &mut AppConfig,
    current: &AppConfig,
) -> Result<(), String> {
    for upstream in &mut new.upstreams {
        if let UpstreamAuth::ApiKey { key: Some(k) } = &mut upstream.auth {
            if is_masked(k) {
                let real = current
                    .upstreams
                    .iter()
                    .find(|u| u.name == upstream.name)
                    .or_else(|| {
                        current.upstreams.iter().find(|u| {
                            matches!(&u.auth, UpstreamAuth::ApiKey { key: Some(rk) } if mask_secret(rk) == *k)
                        })
                    })
                    .and_then(|u| match &u.auth {
                        UpstreamAuth::ApiKey { key } => key.clone(),
                        _ => None,
                    });
                match real {
                    Some(real_key) => *k = real_key,
                    None => {
                        return Err(format!(
                            "上游 '{}' 的 API Key 是掩码值但找不到原始值，请重新输入完整 Key",
                            upstream.name
                        ));
                    }
                }
            }
        }
    }

    let mask_lookup: std::collections::HashMap<String, String> = current
        .client_api_keys
        .iter()
        .map(|item| (mask_secret(&item.key), item.key.clone()))
        .collect();
    for item in &mut new.client_api_keys {
        if is_masked(&item.key) {
            match mask_lookup.get(&item.key) {
                Some(real) => item.key = real.clone(),
                None => {
                    return Err(
                        "客户端 API Key 是掩码值但找不到原始值，请重新输入完整 Key".to_string()
                    );
                }
            }
        }
    }

    if let Some(k) = &mut new.cli_proxy_api_api_key {
        if is_masked(k) {
            match &current.cli_proxy_api_api_key {
                Some(real) => *k = real.clone(),
                None => {
                    return Err(
                        "CLIProxyAPI API Key 是掩码值但找不到原始值，请重新输入完整 Key"
                            .to_string(),
                    );
                }
            }
        }
    }

    Ok(())
}

pub(crate) async fn get_config(state: web::Data<AppState>, req: HttpRequest) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }

    let mut config = state.config.get_config().await;
    normalize_client_api_keys(&mut config);
    config.admin_password_hash = None;
    mask_config_secrets(&mut config);
    HttpResponse::Ok().json(&config)
}

pub(crate) async fn update_config(
    state: web::Data<AppState>,
    body: web::Json<AppConfig>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }

    let current_config = state.config.get_config().await;
    let mut new_config = body.into_inner();
    new_config.admin_password_hash = current_config.admin_password_hash.clone();
    if let Err(e) = restore_masked_secrets(&mut new_config, &current_config) {
        return HttpResponse::BadRequest().json(json!({"success": false, "error": e}));
    }
    normalize_client_api_keys(&mut new_config);

    match state.config.update_config(new_config).await {
        Ok(_) => HttpResponse::Ok().json(json!({"success": true})),
        Err(e) => HttpResponse::InternalServerError()
            .json(json!({"success": false, "error": e.to_string()})),
    }
}

/// 取回单个客户端 API Key 明文（管理员鉴权，供 WebUI 复制功能使用）
pub(crate) async fn reveal_client_api_key(
    state: web::Data<AppState>,
    path: web::Path<usize>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }

    let index = path.into_inner();
    let mut config = state.config.get_config().await;
    normalize_client_api_keys(&mut config);

    match config.client_api_keys.get(index) {
        Some(item) => HttpResponse::Ok().json(json!({"key": item.key})),
        None => HttpResponse::NotFound().json(json!({"error": "API Key 不存在"})),
    }
}

pub(crate) async fn get_version() -> HttpResponse {
    HttpResponse::Ok().json(json!({
        "version": env!("CARGO_PKG_VERSION")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_wrapper::models::ClientApiKeyConfig;

    fn config_with_upstream_key(name: &str, key: Option<&str>) -> AppConfig {
        let json = json!({
            "upstreams": [{
                "name": name,
                "base_url": "http://localhost:9999",
                "auth": {"type": "api_key", "key": key}
            }]
        });
        serde_json::from_value(json).unwrap()
    }

    fn upstream_key(config: &AppConfig) -> Option<&str> {
        match &config.upstreams[0].auth {
            UpstreamAuth::ApiKey { key } => key.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn test_mask_secret() {
        assert_eq!(mask_secret("short"), "••••••");
        assert_eq!(mask_secret("12345678"), "••••••");
        assert_eq!(mask_secret("sk-abcdefghijklxy12"), "sk-a••••••xy12");
        assert!(is_masked(&mask_secret("sk-abcdefghijklxy12")));
        assert!(!is_masked("sk-abcdefghijklxy12"));
    }

    #[test]
    fn test_mask_config_secrets() {
        let mut config = config_with_upstream_key("up1", Some("sk-abcdefghijklxy12"));
        config.client_api_keys.push(ClientApiKeyConfig {
            name: "k1".to_string(),
            key: "ck-abcdefghijklxy34".to_string(),
        });
        config.cli_proxy_api_api_key = Some("cp-abcdefghijklxy56".to_string());

        mask_config_secrets(&mut config);
        assert_eq!(upstream_key(&config), Some("sk-a••••••xy12"));
        assert_eq!(config.client_api_keys[0].key, "ck-a••••••xy34");
        assert_eq!(
            config.cli_proxy_api_api_key.as_deref(),
            Some("cp-a••••••xy56")
        );
    }

    #[test]
    fn test_restore_masked_secrets_roundtrip() {
        let current = config_with_upstream_key("up1", Some("sk-abcdefghijklxy12"));
        let mut submitted = config_with_upstream_key("up1", Some("sk-abcdefghijklxy12"));
        mask_config_secrets(&mut submitted);

        assert!(restore_masked_secrets(&mut submitted, &current).is_ok());
        assert_eq!(upstream_key(&submitted), Some("sk-abcdefghijklxy12"));
    }

    #[test]
    fn test_restore_masked_secrets_new_value_passthrough() {
        let current = config_with_upstream_key("up1", Some("sk-abcdefghijklxy12"));
        let mut submitted = config_with_upstream_key("up1", Some("sk-newkey-plaintext"));

        assert!(restore_masked_secrets(&mut submitted, &current).is_ok());
        assert_eq!(upstream_key(&submitted), Some("sk-newkey-plaintext"));
    }

    #[test]
    fn test_restore_masked_secrets_renamed_upstream_matched_by_mask() {
        let current = config_with_upstream_key("up1", Some("sk-abcdefghijklxy12"));
        let mut submitted = config_with_upstream_key("renamed", Some("sk-a••••••xy12"));

        assert!(restore_masked_secrets(&mut submitted, &current).is_ok());
        assert_eq!(upstream_key(&submitted), Some("sk-abcdefghijklxy12"));
    }

    #[test]
    fn test_restore_masked_secrets_unknown_mask_rejected() {
        let current = config_with_upstream_key("up1", Some("sk-abcdefghijklxy12"));
        let mut submitted = config_with_upstream_key("renamed", Some("sk-x••••••zz99"));

        assert!(restore_masked_secrets(&mut submitted, &current).is_err());
    }

    #[test]
    fn test_restore_masked_client_keys() {
        let mut current = AppConfig::default();
        current.client_api_keys.push(ClientApiKeyConfig {
            name: String::new(),
            key: "ck-abcdefghijklxy34".to_string(),
        });
        let mut submitted = AppConfig::default();
        submitted.client_api_keys.push(ClientApiKeyConfig {
            name: String::new(),
            key: mask_secret("ck-abcdefghijklxy34"),
        });

        assert!(restore_masked_secrets(&mut submitted, &current).is_ok());
        assert_eq!(submitted.client_api_keys[0].key, "ck-abcdefghijklxy34");

        // 伪造的掩码值（无对应原始值）应被拒绝
        let mut forged = AppConfig::default();
        forged.client_api_keys.push(ClientApiKeyConfig {
            name: String::new(),
            key: "ck-x••••••zz99".to_string(),
        });
        assert!(restore_masked_secrets(&mut forged, &current).is_err());
    }

    #[tokio::test]
    async fn test_get_version_shape() {
        let resp = get_version().await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::OK);
    }
}
