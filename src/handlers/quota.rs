use actix_web::{web, HttpResponse};
use base64::Engine as _;
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::handlers::admin::require_admin;
use crate::state::AppState;

pub(crate) async fn cli_proxy_api_quota(
    state: web::Data<AppState>,
    req: actix_web::HttpRequest,
) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let manager = match &state.cli_proxy_api_manager {
        Some(m) => m,
        None => return HttpResponse::Ok().json(json!({"providers": {}})),
    };

    if !manager.is_running().await {
        if let Err(e) = manager.start().await {
            return HttpResponse::ServiceUnavailable().json(json!({
                "error": format!("CLIProxyAPI is not running: {}", e)
            }));
        }
    }

    let endpoint = manager.endpoint().await;
    let secret = manager.management_secret().await;
    let client = reqwest::Client::new();

    let body = match fetch_cli_proxy_api_auth_files(&client, &endpoint, &secret).await {
        Ok(body) => body,
        Err(error) => {
            return HttpResponse::BadGateway().json(json!({"error": error}));
        }
    };

    let mut grouped: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if let Some(files) = body.get("files").and_then(|files| files.as_array()) {
        for file in files {
            let provider = file.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            if provider != "codex" && provider != "claude" {
                continue;
            }
            let account =
                query_cli_proxy_api_quota_account(&client, &endpoint, &secret, file).await;
            grouped
                .entry(provider.to_string())
                .or_default()
                .push(account);
        }
    }

    let providers = grouped
        .into_iter()
        .map(|(provider, accounts)| {
            (
                provider,
                json!({
                    "account_count": accounts.len(),
                    "accounts": accounts,
                }),
            )
        })
        .collect::<serde_json::Map<String, Value>>();

    HttpResponse::Ok().json(json!({"providers": providers}))
}

async fn fetch_cli_proxy_api_auth_files(
    client: &reqwest::Client,
    endpoint: &str,
    secret: &str,
) -> Result<Value, String> {
    let resp = client
        .get(format!("{}/v0/management/auth-files", endpoint))
        .bearer_auth(secret)
        .send()
        .await
        .map_err(|e| format!("Failed to call CLIProxyAPI auth-files: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("CLIProxyAPI auth-files HTTP {}: {}", status, text));
    }

    resp.json::<Value>()
        .await
        .map_err(|e| format!("Failed to parse CLIProxyAPI auth-files: {}", e))
}

async fn query_cli_proxy_api_quota_account(
    client: &reqwest::Client,
    endpoint: &str,
    secret: &str,
    file: &Value,
) -> Value {
    let provider = file.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    match provider {
        "codex" => query_codex_quota_account(client, endpoint, secret, file).await,
        "claude" => query_claude_quota_account(client, endpoint, secret, file).await,
        _ => json!({}),
    }
}

async fn query_codex_quota_account(
    client: &reqwest::Client,
    endpoint: &str,
    secret: &str,
    file: &Value,
) -> Value {
    let mut account = base_quota_account(file, "codex");
    let auth_index = string_field(file, "auth_index");
    let account_id = codex_account_id(file);

    if auth_index.is_empty() || account_id.is_empty() {
        account["status"] = json!("missing");
        account["error"] = json!("missing auth_index or ChatGPT account id");
        return account;
    }

    let payload = json!({
        "auth_index": auth_index,
        "method": "GET",
        "url": "https://chatgpt.com/backend-api/wham/usage",
        "header": {
            "Authorization": "Bearer $TOKEN$",
            "Content-Type": "application/json",
            "User-Agent": "codex_cli_rs/0.128.0 (macos; arm64)",
            "Chatgpt-Account-Id": account_id,
        }
    });

    match cli_proxy_api_call(client, endpoint, secret, payload).await {
        Ok(body) => {
            account["plan_type"] = first_string(&[body.get("plan_type"), body.get("planType")])
                .or_else(|| codex_plan_type(file))
                .map(Value::from)
                .unwrap_or(Value::Null);
            let windows = codex_quota_windows(&body);
            account["windows"] = json!(windows);
            account["status"] = json!(quota_status(&windows));
        }
        Err(error) => {
            account["status"] = json!("error");
            account["error"] = json!(error);
        }
    }

    account
}

async fn query_claude_quota_account(
    client: &reqwest::Client,
    endpoint: &str,
    secret: &str,
    file: &Value,
) -> Value {
    let mut account = base_quota_account(file, "claude");
    let auth_index = string_field(file, "auth_index");

    if auth_index.is_empty() {
        account["status"] = json!("missing");
        account["error"] = json!("missing auth_index");
        return account;
    }

    let payload = json!({
        "auth_index": auth_index,
        "method": "GET",
        "url": "https://api.anthropic.com/api/oauth/usage",
        "header": {
            "Accept": "application/json",
            "Authorization": "Bearer $TOKEN$",
            "anthropic-beta": "oauth-2025-04-20",
        }
    });

    match cli_proxy_api_call(client, endpoint, secret, payload).await {
        Ok(body) => {
            let windows = claude_quota_windows(&body);
            account["windows"] = json!(windows);
            account["status"] = json!(quota_status(&windows));
        }
        Err(error) => {
            account["status"] = json!("error");
            account["error"] = json!(error);
        }
    }

    account
}

async fn cli_proxy_api_call(
    client: &reqwest::Client,
    endpoint: &str,
    secret: &str,
    payload: Value,
) -> Result<Value, String> {
    let resp = client
        .post(format!("{}/v0/management/api-call", endpoint))
        .bearer_auth(secret)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Failed to call CLIProxyAPI api-call: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("CLIProxyAPI api-call HTTP {}: {}", status, text));
    }

    let response = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("Failed to parse CLIProxyAPI api-call: {}", e))?;
    let status_code = response
        .get("status_code")
        .or_else(|| response.get("statusCode"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let body = response.get("body").and_then(|v| v.as_str()).unwrap_or("");

    if !(200..300).contains(&status_code) {
        return Err(if body.trim().is_empty() {
            format!("quota request HTTP {}", status_code)
        } else {
            body.trim().to_string()
        });
    }

    serde_json::from_str::<Value>(body).map_err(|_| "empty or invalid quota payload".to_string())
}

fn base_quota_account(file: &Value, provider: &str) -> Value {
    json!({
        "provider": provider,
        "name": string_field(file, "name"),
        "email": string_field(file, "email"),
        "status": "unknown",
        "windows": [],
    })
}

fn codex_quota_windows(body: &Value) -> Vec<Value> {
    let rate_limit = body
        .get("rate_limit")
        .or_else(|| body.get("rateLimit"))
        .unwrap_or(&Value::Null);
    let primary = rate_limit
        .get("primary_window")
        .or_else(|| rate_limit.get("primaryWindow"));
    let secondary = rate_limit
        .get("secondary_window")
        .or_else(|| rate_limit.get("secondaryWindow"));
    let mut five_hour = None;
    let mut weekly = None;

    for window in [primary, secondary].into_iter().flatten() {
        match quota_window_seconds(window) {
            Some(18_000) => five_hour = Some(window),
            Some(604_800) => weekly = Some(window),
            _ => {}
        }
    }

    let limit_reached =
        bool_field(rate_limit, "limit_reached") || bool_field(rate_limit, "limitReached");
    let allowed = rate_limit.get("allowed").and_then(|v| v.as_bool());
    let mut windows = Vec::new();
    if let Some(window) = quota_window_json(
        "five_hour",
        "5h",
        five_hour.or(primary),
        limit_reached,
        allowed,
    ) {
        windows.push(window);
    }
    if let Some(window) =
        quota_window_json("weekly", "7d", weekly.or(secondary), limit_reached, allowed)
    {
        windows.push(window);
    }
    windows
}

fn claude_quota_windows(body: &Value) -> Vec<Value> {
    let mut windows = Vec::new();
    if let Some(window) = claude_quota_window_json("five_hour", "5h", body.get("five_hour")) {
        windows.push(window);
    }
    if let Some(window) = claude_quota_window_json("weekly", "7d", body.get("seven_day")) {
        windows.push(window);
    }
    if let Some(window) =
        claude_quota_window_json("weekly_sonnet", "Sonnet 7d", body.get("seven_day_sonnet"))
    {
        windows.push(window);
    }
    if let Some(window) =
        claude_quota_window_json("weekly_opus", "Opus 7d", body.get("seven_day_opus"))
    {
        windows.push(window);
    }
    windows
}

fn quota_window_json(
    id: &str,
    label: &str,
    window: Option<&Value>,
    limit_reached: bool,
    allowed: Option<bool>,
) -> Option<Value> {
    let window = window?;
    let used = number_field(window, "used_percent").or_else(|| number_field(window, "usedPercent"));
    let exhausted_hint = limit_reached || allowed == Some(false);
    let used_percent = used.or(if exhausted_hint { Some(100.0) } else { None });
    let remaining_percent = used_percent.map(|v| (100.0 - v).clamp(0.0, 100.0));
    Some(json!({
        "id": id,
        "label": label,
        "used_percent": used_percent,
        "remaining_percent": remaining_percent,
        "reset_at": quota_reset_label(window),
        "exhausted": remaining_percent == Some(0.0),
    }))
}

fn claude_quota_window_json(id: &str, label: &str, window: Option<&Value>) -> Option<Value> {
    let window = window?;
    let used_percent = number_field(window, "utilization")?;
    let remaining_percent = (100.0 - used_percent).clamp(0.0, 100.0);
    Some(json!({
        "id": id,
        "label": label,
        "used_percent": used_percent,
        "remaining_percent": remaining_percent,
        "reset_at": string_field(window, "resets_at"),
        "exhausted": remaining_percent == 0.0,
    }))
}

fn quota_status(windows: &[Value]) -> &'static str {
    let mut lowest = None;
    for window in windows {
        if let Some(remaining) = number_field(window, "remaining_percent") {
            lowest = Some(lowest.map_or(remaining, |current: f64| current.min(remaining)));
        }
    }
    match lowest {
        Some(v) if v <= 0.0 => "exhausted",
        Some(v) if v <= 30.0 => "low",
        Some(_) => "ok",
        None => "unknown",
    }
}

fn quota_window_seconds(window: &Value) -> Option<i64> {
    window
        .get("limit_window_seconds")
        .or_else(|| window.get("limitWindowSeconds"))
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|n| n as i64)))
}

fn quota_reset_label(window: &Value) -> String {
    if let Some(ts) = window
        .get("reset_at")
        .or_else(|| window.get("resetAt"))
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|n| n as i64)))
    {
        return ts.to_string();
    }
    if let Some(secs) = window
        .get("reset_after_seconds")
        .or_else(|| window.get("resetAfterSeconds"))
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|n| n as i64)))
    {
        return format!("+{}s", secs);
    }
    String::new()
}

fn codex_account_id(file: &Value) -> String {
    for candidate in [file.get("id_token"), file.get("idToken")]
        .into_iter()
        .flatten()
    {
        if let Some(account_id) = account_id_from_token_value(candidate) {
            return account_id;
        }
    }
    string_field(file, "account_id")
}

fn codex_plan_type(file: &Value) -> Option<String> {
    for candidate in [file.get("id_token"), file.get("idToken")]
        .into_iter()
        .flatten()
    {
        if let Some(plan_type) = plan_type_from_token_value(candidate) {
            return Some(plan_type);
        }
    }
    first_string(&[file.get("plan_type"), file.get("planType")])
}

fn account_id_from_token_value(value: &Value) -> Option<String> {
    token_payload(value).and_then(|payload| {
        first_string(&[
            payload.get("chatgpt_account_id"),
            payload
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id")),
        ])
    })
}

fn plan_type_from_token_value(value: &Value) -> Option<String> {
    token_payload(value).and_then(|payload| {
        first_string(&[
            payload.get("plan_type"),
            payload.get("chatgpt_plan_type"),
            payload
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_plan_type")),
        ])
    })
}

fn token_payload(value: &Value) -> Option<Value> {
    if value.is_object() {
        return Some(value.clone());
    }
    let token = value.as_str()?.trim();
    let payload = token.split('.').nth(1)?;
    let mut padded = payload.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(padded)
        .ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()
}

fn first_string(values: &[Option<&Value>]) -> Option<String> {
    values.iter().flatten().find_map(|value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
    })
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn number_field(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_i64().map(|n| n as f64))
            .or_else(|| v.as_u64().map(|n| n as f64))
    })
}
