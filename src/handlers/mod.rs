pub mod admin;
pub mod chat;
pub mod cli_proxy;
pub mod config_api;
pub mod debug;
pub mod models_api;
pub mod quota;

use actix_web::{web, HttpRequest, HttpResponse};
use llm_wrapper::models::{AppConfig, ClientApiKeyConfig};
use serde_json::json;

use crate::state::AppState;

pub(crate) fn normalize_client_api_keys(config: &mut AppConfig) {
    if let Some(key) = config.client_api_key.take() {
        config.client_api_keys.push(ClientApiKeyConfig {
            name: String::new(),
            key,
        });
    }

    let mut keys = Vec::new();
    for item in config.client_api_keys.drain(..) {
        let key = item.key.trim().to_string();
        let name = item.name.trim().to_string();
        if !key.is_empty()
            && !keys
                .iter()
                .any(|existing: &ClientApiKeyConfig| existing.key == key)
        {
            keys.push(ClientApiKeyConfig { name, key });
        }
    }
    config.client_api_keys = keys;
}

pub(crate) async fn require_client_api_key(
    state: &web::Data<AppState>,
    req: &HttpRequest,
) -> Result<(), HttpResponse> {
    let mut config = state.config.get_config().await;
    normalize_client_api_keys(&mut config);
    if config.client_api_keys.is_empty() {
        return Ok(());
    }

    if let Some(header_value) = req.headers().get("Authorization") {
        if let Ok(value) = header_value.to_str() {
            if let Some(token) = value.strip_prefix("Bearer ") {
                if config.client_api_keys.iter().any(|item| item.key == token) {
                    return Ok(());
                }
            }
        }
    }

    if let Some(header_value) = req.headers().get("x-api-key") {
        if let Ok(value) = header_value.to_str() {
            if config.client_api_keys.iter().any(|item| item.key == value) {
                return Ok(());
            }
        }
    }

    Err(HttpResponse::Unauthorized()
        .json(json!({"error": {"message": "Invalid or missing API key"}})))
}
