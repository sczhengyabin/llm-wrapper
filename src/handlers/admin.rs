use actix_web::cookie::{Cookie, SameSite};
use actix_web::{web, HttpRequest, HttpResponse};
use argon2::password_hash::SaltString;
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use llm_wrapper::models::AppConfig;
use serde_json::json;

use crate::state::{client_ip, AppState, ADMIN_SESSION_TTL_HOURS};

fn hash_admin_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
        .to_string();
    Ok(hash)
}

fn verify_admin_password(password: &str, hash: &str) -> bool {
    let parsed_hash = match PasswordHash::new(hash) {
        Ok(h) => h,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok()
}

fn parse_admin_cookie(req: &HttpRequest) -> Option<String> {
    req.cookie("llm_wrapper_admin_session")
        .map(|c| c.value().to_string())
}

pub(crate) async fn require_admin(
    state: &web::Data<AppState>,
    req: &HttpRequest,
) -> Result<(), HttpResponse> {
    let config = state.config.get_config().await;
    if config.admin_password_hash.is_none() {
        return Err(HttpResponse::Forbidden().json(json!({"error": "管理密码未初始化"})));
    }

    if let Some(token) = parse_admin_cookie(req) {
        if state.admin_sessions.validate_session(&token).await {
            return Ok(());
        }
    }

    Err(HttpResponse::Unauthorized().json(json!({"error": "需要管理员登录"})))
}

/// cookie Secure 标志：配置显式指定优先，否则按请求 scheme 自动判断（反代需传 X-Forwarded-Proto）
fn cookie_secure_flag(config: &AppConfig, req: &HttpRequest) -> bool {
    config
        .cookie_secure
        .unwrap_or_else(|| req.connection_info().scheme() == "https")
}

fn admin_session_cookie(token: String, secure: bool) -> Cookie<'static> {
    Cookie::build("llm_wrapper_admin_session", token)
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Strict)
        .max_age(actix_web::cookie::time::Duration::hours(
            ADMIN_SESSION_TTL_HOURS,
        ))
        .finish()
}

fn expired_admin_session_cookie(secure: bool) -> Cookie<'static> {
    Cookie::build("llm_wrapper_admin_session", "")
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Strict)
        .max_age(actix_web::cookie::time::Duration::seconds(0))
        .finish()
}

pub(crate) async fn admin_status(state: web::Data<AppState>, req: HttpRequest) -> HttpResponse {
    let config = state.config.get_config().await;
    let setup_required = config.admin_password_hash.is_none();

    let authenticated = if setup_required {
        false
    } else if let Some(token) = parse_admin_cookie(&req) {
        state.admin_sessions.validate_session(&token).await
    } else {
        false
    };

    HttpResponse::Ok().json(json!({
        "setup_required": setup_required,
        "authenticated": authenticated
    }))
}

pub(crate) async fn admin_setup(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: HttpRequest,
) -> HttpResponse {
    let ip = client_ip(&req);
    if let Err(retry_after) = state.login_rate_limiter.check(&ip).await {
        return HttpResponse::TooManyRequests()
            .json(json!({"error": format!("尝试过于频繁，请 {} 秒后重试", retry_after)}));
    }

    let password = match body.get("password").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => {
            return HttpResponse::BadRequest().json(json!({"error": "password 不能为空"}));
        }
    };

    let current_config = state.config.get_config().await;
    if current_config.admin_password_hash.is_some() {
        state.login_rate_limiter.record_failure(&ip).await;
        return HttpResponse::Conflict().json(json!({"error": "管理员密码已设置"}));
    }

    let hash = match hash_admin_password(&password) {
        Ok(h) => h,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(json!({"error": format!("生成密码哈希失败: {}", e)}));
        }
    };

    let mut new_config = current_config.clone();
    new_config.admin_password_hash = Some(hash);
    if let Err(e) = state.config.update_config(new_config).await {
        return HttpResponse::InternalServerError()
            .json(json!({"error": format!("保存密码失败: {}", e)}));
    }

    let token = state.admin_sessions.create_session().await;
    let cookie = admin_session_cookie(token, cookie_secure_flag(&current_config, &req));

    HttpResponse::Ok()
        .cookie(cookie)
        .json(json!({"success": true}))
}

pub(crate) async fn admin_login(
    state: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
    req: HttpRequest,
) -> HttpResponse {
    let ip = client_ip(&req);
    if let Err(retry_after) = state.login_rate_limiter.check(&ip).await {
        return HttpResponse::TooManyRequests()
            .json(json!({"error": format!("尝试过于频繁，请 {} 秒后重试", retry_after)}));
    }

    let password = match body.get("password").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        _ => String::new(),
    };

    let config = state.config.get_config().await;
    let hash = match &config.admin_password_hash {
        Some(h) => h.clone(),
        None => return HttpResponse::Forbidden().json(json!({"error": "管理员密码未初始化"})),
    };

    if !verify_admin_password(&password, &hash) {
        state.login_rate_limiter.record_failure(&ip).await;
        return HttpResponse::Unauthorized().json(json!({"error": "密码错误"}));
    }

    state.login_rate_limiter.clear(&ip).await;
    let token = state.admin_sessions.create_session().await;
    let cookie = admin_session_cookie(token, cookie_secure_flag(&config, &req));

    HttpResponse::Ok()
        .cookie(cookie)
        .json(json!({"success": true}))
}

pub(crate) async fn admin_logout(state: web::Data<AppState>, req: HttpRequest) -> HttpResponse {
    if let Some(token) = parse_admin_cookie(&req) {
        state.admin_sessions.remove_session(&token).await;
    }

    let config = state.config.get_config().await;
    HttpResponse::Ok()
        .cookie(expired_admin_session_cookie(cookie_secure_flag(
            &config, &req,
        )))
        .json(json!({"success": true}))
}
