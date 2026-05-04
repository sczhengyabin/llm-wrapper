use crate::models::{CachedToken, DeviceAuthStatus, OAuthTokenResponse, UpstreamAuth};
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 以所有者独占权限写入文件（0600）
#[cfg(unix)]
async fn write_private_file<P: AsRef<std::path::Path>>(
    path: P,
    content: String,
) -> std::io::Result<()> {
    #[allow(unused_imports)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .await?;
    tokio::io::AsyncWriteExt::write_all(&mut file, content.as_bytes()).await
}

#[cfg(not(unix))]
async fn write_private_file<P: AsRef<std::path::Path>>(
    path: P,
    content: String,
) -> std::io::Result<()> {
    tokio::fs::write(path, content).await.map(drop)
}
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, warn};

/// 磁盘上的 token 缓存结构
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenCacheFile {
    #[serde(default)]
    pub tokens: HashMap<String, CachedToken>,
}

/// 正在进行的设备认证会话信息
#[derive(Debug, Clone)]
pub struct DeviceAuthSession {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub interval: u64,
}

/// OpenAI 自定义设备码：获取 usercode 响应
#[derive(Debug, Clone, Deserialize)]
struct OpenAiUserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "user_code", alias = "usercode")]
    usercode: String,
    #[serde(default, deserialize_with = "deserialize_interval")]
    interval: u64,
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    s.trim().parse().map_err(serde::de::Error::custom)
}

/// OpenAI 自定义设备码：轮询授权码响应
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct OpenAiAuthCodeResponse {
    authorization_code: String,
    code_challenge: String,
    code_verifier: String,
}

/// OAuth 认证管理器
#[derive(Clone)]
pub struct AuthManager {
    client: Client,
    token_cache_path: PathBuf,
    cache: Arc<Mutex<TokenCacheFile>>,
    refresh_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    completion_tx: Arc<broadcast::Sender<(String, DeviceAuthStatus)>>,
}

impl AuthManager {
    pub fn new(cache_dir: Option<&std::path::Path>) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join("tokens.json"),
            None => dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".llm-wrapper")
                .join("tokens.json"),
        };

        let (completion_tx, _) = broadcast::channel(32);

        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("Failed to create HTTP client"),
            token_cache_path: cache_path,
            cache: Arc::new(Mutex::new(TokenCacheFile::default())),
            refresh_locks: Arc::new(Mutex::new(HashMap::new())),
            completion_tx: Arc::new(completion_tx),
        }
    }

    /// 启动时从磁盘加载 token 缓存
    pub async fn load_cache(&self) {
        if self.token_cache_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&self.token_cache_path) {
                match serde_json::from_str::<TokenCacheFile>(&content) {
                    Ok(cache) => {
                        let mut guard = self.cache.lock().await;
                        *guard = cache;
                        info!(
                            "Loaded {} cached tokens from {}",
                            guard.tokens.len(),
                            self.token_cache_path.display()
                        );
                    }
                    Err(e) => warn!("Failed to parse token cache: {}", e),
                }
            }
        }
    }

    /// 持久化 token 缓存到磁盘
    async fn save_cache(&self) {
        let guard = self.cache.lock().await;
        if let Some(parent) = self.token_cache_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if let Ok(json) = serde_json::to_string_pretty(&*guard) {
            if let Err(e) = write_private_file(&self.token_cache_path, json).await {
                warn!("Failed to save token cache: {}", e);
            }
        }
    }

    /// 获取有效的访问令牌
    pub async fn get_access_token(
        &self,
        upstream_name: &str,
        auth: &UpstreamAuth,
    ) -> Option<String> {
        match auth {
            UpstreamAuth::ApiKey { key } => key.clone(),
            UpstreamAuth::OAuthDevice {
                client_id,
                token_url,
                ..
            } => {
                self.get_oauth_token(upstream_name, client_id, token_url)
                    .await
            }
        }
    }

    async fn get_oauth_token(
        &self,
        upstream_name: &str,
        client_id: &str,
        token_url: &str,
    ) -> Option<String> {
        let cache = self.cache.lock().await;

        if let Some(token) = cache.tokens.get(upstream_name) {
            let now = Utc::now();
            let margin = chrono::Duration::seconds(60);

            if now < token.expires_at - margin {
                return Some(token.access_token.clone());
            }

            if let Some(ref rt) = token.refresh_token {
                let rt = rt.clone();
                drop(cache);
                return self
                    .try_refresh(upstream_name, client_id, token_url, &rt)
                    .await;
            }

            return None;
        }

        None
    }

    async fn try_refresh(
        &self,
        upstream_name: &str,
        client_id: &str,
        token_url: &str,
        refresh_token: &str,
    ) -> Option<String> {
        let lock = self.get_refresh_lock(upstream_name).await;
        let _guard = lock.lock().await;

        // 双重检查
        {
            let cache = self.cache.lock().await;
            if let Some(token) = cache.tokens.get(upstream_name) {
                let now = Utc::now();
                let margin = chrono::Duration::seconds(60);
                if now < token.expires_at - margin {
                    return Some(token.access_token.clone());
                }
            }
        }

        // Detect OpenAI: token_url is the polling URL, actual token endpoint is /oauth/token
        let refresh_url = if token_url.contains("auth.openai.com")
            && (token_url.contains("deviceauth") || token_url.contains("device"))
        {
            "https://auth.openai.com/oauth/token".to_string()
        } else {
            token_url.to_string()
        };

        let response = self
            .client
            .post(&refresh_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .ok()?;

        if !response.status().is_success() {
            warn!(
                "Token refresh failed for {}: {}",
                upstream_name,
                response.status()
            );
            return None;
        }

        let token_resp: OAuthTokenResponse = response.json().await.ok()?;
        let expires_at = Utc::now() + chrono::Duration::seconds(token_resp.expires_in as i64);

        let new_token = CachedToken {
            access_token: token_resp.access_token.clone(),
            token_type: token_resp.token_type,
            refresh_token: token_resp.refresh_token,
            expires_at,
        };

        let mut cache = self.cache.lock().await;
        cache.tokens.insert(upstream_name.to_string(), new_token);
        drop(cache);
        self.save_cache().await;

        info!("Token refreshed for {}", upstream_name);
        Some(token_resp.access_token)
    }

    async fn get_refresh_lock(&self, upstream_name: &str) -> Arc<Mutex<()>> {
        let mut locks = self.refresh_locks.lock().await;
        locks
            .entry(upstream_name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// 发起设备码认证流程
    pub async fn initiate_device_auth(
        &self,
        upstream_name: &str,
        client_id: &str,
        device_auth_url: &str,
        token_url: &str,
        scope: Option<&str>,
    ) -> Result<DeviceAuthSession, String> {
        // Detect OpenAI custom flow
        let is_openai =
            device_auth_url.contains("auth.openai.com") && device_auth_url.contains("deviceauth");

        if is_openai {
            self.initiate_openai_device_auth(upstream_name, client_id, device_auth_url, token_url)
                .await
        } else {
            self.initiate_standard_device_auth(
                upstream_name,
                client_id,
                device_auth_url,
                token_url,
                scope,
            )
            .await
        }
    }

    /// OpenAI 自定义设备码流程
    async fn initiate_openai_device_auth(
        &self,
        upstream_name: &str,
        client_id: &str,
        device_auth_url: &str,
        token_url: &str,
    ) -> Result<DeviceAuthSession, String> {
        // Step 1: Request user code
        let response = self
            .client
            .post(device_auth_url)
            .header("Content-Type", "application/json")
            .body(serde_json::json!({"client_id": client_id}).to_string())
            .send()
            .await
            .map_err(|e| format!("Failed to request user code: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("User code request failed: {} - {}", status, body));
        }

        let user_code_resp: OpenAiUserCodeResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse user code response: {}", e))?;

        let expires_at = Utc::now() + chrono::Duration::minutes(15);
        let verification_uri = "https://auth.openai.com/codex/device".to_string();

        let session = DeviceAuthSession {
            user_code: user_code_resp.usercode.clone(),
            verification_uri: verification_uri.clone(),
            verification_uri_complete: Some(format!(
                "https://auth.openai.com/codex/device?code={}",
                user_code_resp.usercode
            )),
            expires_at,
            interval: user_code_resp.interval,
        };

        // Notify frontend
        let _ = self.completion_tx.send((
            upstream_name.to_string(),
            DeviceAuthStatus::Pending {
                user_code: session.user_code.clone(),
                verification_uri: session.verification_uri.clone(),
                verification_uri_complete: session.verification_uri_complete.clone(),
                expires_at: session.expires_at.to_rfc3339(),
            },
        ));

        // Spawn poller for OpenAI flow
        self.spawn_openai_poller(
            upstream_name.to_string(),
            user_code_resp.device_auth_id,
            user_code_resp.usercode,
            token_url.to_string(),
            client_id.to_string(),
            user_code_resp.interval,
            expires_at,
        );

        Ok(session)
    }

    /// 标准 RFC 8628 设备码流程
    async fn initiate_standard_device_auth(
        &self,
        upstream_name: &str,
        client_id: &str,
        device_auth_url: &str,
        token_url: &str,
        scope: Option<&str>,
    ) -> Result<DeviceAuthSession, String> {
        let mut form = vec![("client_id", client_id)];
        if let Some(s) = scope {
            form.push(("scope", s));
        }

        let response = self
            .client
            .post(device_auth_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| format!("Failed to initiate device auth: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("Device auth failed: {} - {}", status, body));
        }

        #[derive(Deserialize)]
        struct StdDeviceResp {
            device_code: String,
            user_code: String,
            verification_uri: String,
            #[serde(default)]
            verification_uri_complete: Option<String>,
            expires_in: u64,
            #[serde(default)]
            interval: Option<u64>,
        }

        let dev_resp: StdDeviceResp = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse device auth response: {}", e))?;

        let expires_at = Utc::now() + chrono::Duration::seconds(dev_resp.expires_in as i64);
        let interval = dev_resp.interval.unwrap_or(5);

        let session = DeviceAuthSession {
            user_code: dev_resp.user_code.clone(),
            verification_uri: dev_resp.verification_uri.clone(),
            verification_uri_complete: dev_resp.verification_uri_complete.clone(),
            expires_at,
            interval,
        };

        let _ = self.completion_tx.send((
            upstream_name.to_string(),
            DeviceAuthStatus::Pending {
                user_code: session.user_code.clone(),
                verification_uri: session.verification_uri.clone(),
                verification_uri_complete: session.verification_uri_complete.clone(),
                expires_at: session.expires_at.to_rfc3339(),
            },
        ));

        self.spawn_standard_poller(
            upstream_name.to_string(),
            dev_resp.device_code,
            token_url.to_string(),
            client_id.to_string(),
            scope.map(|s| s.to_string()),
            interval,
            expires_at,
        );

        Ok(session)
    }

    /// OpenAI 自定义轮询：先获取 authorization_code，再交换 token
    fn spawn_openai_poller(
        &self,
        upstream_name: String,
        device_id: String,
        user_code: String,
        poll_url: String,
        client_id: String,
        interval: u64,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) {
        let client = self.client.clone();
        let cache = self.cache.clone();
        let cache_path = self.token_cache_path.clone();
        let tx = self.completion_tx.clone();

        tokio::spawn(async move {
            let current_interval = interval;

            loop {
                let now = chrono::Utc::now();
                if now >= expires_at {
                    let _ = tx.send((
                        upstream_name.clone(),
                        DeviceAuthStatus::Expired {
                            message: "Device code has expired".to_string(),
                        },
                    ));
                    return;
                }

                // Step 2: Poll for authorization code
                let poll_body = serde_json::json!({
                    "device_auth_id": device_id,
                    "user_code": user_code
                });

                let response = match client
                    .post(&poll_url)
                    .header("Content-Type", "application/json")
                    .body(poll_body.to_string())
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        warn!("Poll request failed: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(current_interval)).await;
                        continue;
                    }
                };

                if !response.status().is_success() {
                    // 403/404 means still pending
                    tokio::time::sleep(std::time::Duration::from_secs(current_interval)).await;
                    continue;
                }

                // Step 3: Got authorization code, exchange for tokens
                let auth_code_resp: OpenAiAuthCodeResponse = match response.json().await {
                    Ok(r) => r,
                    Err(e) => {
                        warn!("Failed to parse auth code response: {}", e);
                        return;
                    }
                };

                let token_response = client
                    .post("https://auth.openai.com/oauth/token")
                    .form(&[
                        ("grant_type", "authorization_code"),
                        ("client_id", &client_id),
                        ("code", &auth_code_resp.authorization_code),
                        ("code_verifier", &auth_code_resp.code_verifier),
                        (
                            "redirect_uri",
                            "https://auth.openai.com/deviceauth/callback",
                        ),
                    ])
                    .send()
                    .await;

                match token_response {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.json::<OAuthTokenResponse>().await {
                            Ok(token_resp) => {
                                let expires_at = Utc::now()
                                    + chrono::Duration::seconds(token_resp.expires_in as i64);

                                let new_token = CachedToken {
                                    access_token: token_resp.access_token.clone(),
                                    token_type: token_resp.token_type,
                                    refresh_token: token_resp.refresh_token,
                                    expires_at,
                                };

                                let mut cache_guard = cache.lock().await;
                                cache_guard.tokens.insert(upstream_name.clone(), new_token);
                                drop(cache_guard);

                                if let Some(parent) = cache_path.parent() {
                                    let _ = tokio::fs::create_dir_all(parent).await;
                                }
                                let cache_guard = cache.lock().await;
                                if let Ok(json) = serde_json::to_string_pretty(&*cache_guard) {
                                    let _ = write_private_file(&cache_path, json).await;
                                }

                                info!("OpenAI device auth completed for {}", upstream_name);
                                let _ = tx.send((
                                    upstream_name.clone(),
                                    DeviceAuthStatus::Success {
                                        message: format!(
                                            "Authorization successful for {}",
                                            upstream_name
                                        ),
                                        expires_at: Some(expires_at.to_rfc3339()),
                                    },
                                ));
                            }
                            Err(e) => {
                                warn!("Failed to parse token response: {}", e);
                            }
                        }
                    }
                    Ok(resp) => {
                        warn!("Token exchange failed: {}", resp.status());
                    }
                    Err(e) => {
                        warn!("Token exchange request failed: {}", e);
                    }
                }

                return;
            }
        });
    }

    /// 标准 RFC 8628 轮询
    fn spawn_standard_poller(
        &self,
        upstream_name: String,
        device_code: String,
        token_url: String,
        client_id: String,
        scope: Option<String>,
        interval: u64,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) {
        let client = self.client.clone();
        let cache = self.cache.clone();
        let cache_path = self.token_cache_path.clone();
        let tx = self.completion_tx.clone();

        tokio::spawn(async move {
            let mut current_interval = interval;

            loop {
                let now = chrono::Utc::now();
                if now >= expires_at {
                    let _ = tx.send((
                        upstream_name.clone(),
                        DeviceAuthStatus::Expired {
                            message: "Device code has expired".to_string(),
                        },
                    ));
                    return;
                }

                let mut form = vec![
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", &client_id),
                    ("device_code", &device_code),
                ];
                if let Some(ref s) = scope {
                    form.push(("scope", s));
                }

                let response = match client.post(&token_url).form(&form).send().await {
                    Ok(r) => r,
                    Err(e) => {
                        warn!("Poll request failed: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(current_interval)).await;
                        continue;
                    }
                };

                if !response.status().is_success() {
                    let body: serde_json::Value =
                        response.json().await.unwrap_or(serde_json::Value::Null);
                    let error_code = body.get("error").and_then(|e| e.as_str()).unwrap_or("");

                    match error_code {
                        "authorization_pending" => {}
                        "slow_down" => {
                            current_interval = (current_interval + 5).min(30);
                            warn!(
                                "OAuth server requested slow down, increasing interval to {}s",
                                current_interval
                            );
                        }
                        "access_denied" => {
                            let _ = tx.send((
                                upstream_name.clone(),
                                DeviceAuthStatus::Failed {
                                    reason: "access_denied".to_string(),
                                    message: "User denied the authorization request".to_string(),
                                },
                            ));
                            return;
                        }
                        "expired_token" => {
                            let _ = tx.send((
                                upstream_name.clone(),
                                DeviceAuthStatus::Expired {
                                    message: "Device code expired during polling".to_string(),
                                },
                            ));
                            return;
                        }
                        _ => {
                            debug!("Unknown OAuth poll error: {}", error_code);
                        }
                    }
                } else {
                    if let Ok(token_resp) = response.json::<OAuthTokenResponse>().await {
                        let expires_at =
                            Utc::now() + chrono::Duration::seconds(token_resp.expires_in as i64);

                        let new_token = CachedToken {
                            access_token: token_resp.access_token.clone(),
                            token_type: token_resp.token_type,
                            refresh_token: token_resp.refresh_token,
                            expires_at,
                        };

                        let mut cache_guard = cache.lock().await;
                        cache_guard.tokens.insert(upstream_name.clone(), new_token);
                        drop(cache_guard);

                        if let Some(parent) = cache_path.parent() {
                            let _ = tokio::fs::create_dir_all(parent).await;
                        }
                        let cache_guard = cache.lock().await;
                        if let Ok(json) = serde_json::to_string_pretty(&*cache_guard) {
                            let _ = write_private_file(&cache_path, json).await;
                        }

                        info!("Device auth completed for {}", upstream_name);
                        let _ = tx.send((
                            upstream_name.clone(),
                            DeviceAuthStatus::Success {
                                message: format!("Authorization successful for {}", upstream_name),
                                expires_at: Some(expires_at.to_rfc3339()),
                            },
                        ));
                        return;
                    }
                }

                tokio::time::sleep(std::time::Duration::from_secs(current_interval)).await;
            }
        });
    }

    /// 获取 token 状态
    pub async fn get_token_status(&self, upstream_name: &str) -> DeviceAuthStatus {
        let cache = self.cache.lock().await;

        if let Some(token) = cache.tokens.get(upstream_name) {
            let now = Utc::now();
            if now < token.expires_at {
                DeviceAuthStatus::Success {
                    message: format!(
                        "Valid until {}",
                        token.expires_at.format("%Y-%m-%d %H:%M:%S UTC")
                    ),
                    expires_at: Some(token.expires_at.to_rfc3339()),
                }
            } else {
                DeviceAuthStatus::Expired {
                    message: "Token has expired".to_string(),
                }
            }
        } else {
            DeviceAuthStatus::Failed {
                reason: "no_token".to_string(),
                message: "No token available. Initiate device auth flow first.".to_string(),
            }
        }
    }

    /// 清除指定上游的 token
    pub async fn clear_token(&self, upstream_name: &str) {
        let mut cache = self.cache.lock().await;
        cache.tokens.remove(upstream_name);
        drop(cache);
        self.save_cache().await;
        info!("Cleared token for {}", upstream_name);
    }

    /// 订阅设备认证完成事件
    pub fn subscribe_completion(&self) -> broadcast::Receiver<(String, DeviceAuthStatus)> {
        self.completion_tx.subscribe()
    }
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new(None)
    }
}
