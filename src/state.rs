use actix_web::{web, Error, HttpRequest};
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt};
use llm_wrapper::oauth::AuthManager;
use llm_wrapper::proxy::DebugInfo;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::cli_proxy_api_manager;
use llm_wrapper::config::ConfigManager;

/// JSON body 限制：32MB，支持 256K token 上下文
pub(crate) const JSON_PAYLOAD_LIMIT: usize = 32 * 1024 * 1024;
const DEBUG_BROADCAST_CAPACITY: usize = 100;
pub(crate) const ADMIN_SESSION_TTL_HOURS: i64 = 24;
const LOGIN_FAIL_WINDOW_SECS: i64 = 60;
const LOGIN_FAIL_MAX_PER_IP: u32 = 5;
const LOGIN_FAIL_MAX_GLOBAL: u32 = 20;
const LOGIN_RATE_LIMIT_MAX_ENTRIES: usize = 10_000;
/// 聚合 /v1/models 时单个上游的拉取超时（要快，慢上游直接跳过）
pub(crate) const MODELS_FETCH_TIMEOUT_AGGREGATE: std::time::Duration =
    std::time::Duration::from_secs(2);
/// 查询单个上游模型列表的超时
pub(crate) const MODELS_FETCH_TIMEOUT_SINGLE: std::time::Duration =
    std::time::Duration::from_secs(5);
pub(crate) const UPSTREAM_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// CLIProxyAPI 登录 SSE 轮询：150 次 × 2s ≈ 5 分钟
pub(crate) const LOGIN_POLL_MAX_ATTEMPTS: u16 = 150;
pub(crate) const LOGIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub(crate) struct AppState {
    pub(crate) config: ConfigManager,
    pub(crate) auth_manager: AuthManager,
    pub(crate) debug_data: web::Data<DebugDataStore>,
    pub(crate) stream_hub: web::Data<DebugStreamHub>,
    pub(crate) cli_proxy_api_manager: Option<Arc<cli_proxy_api_manager::CliProxyApiManager>>,
    pub(crate) admin_sessions: web::Data<AdminSessionStore>,
    pub(crate) login_rate_limiter: LoginRateLimiter,
}

/// 调试数据存储
#[derive(Clone, Default)]
pub(crate) struct DebugDataStore {
    pub(crate) data: Arc<RwLock<Option<DebugInfo>>>,
}

impl DebugDataStore {
    pub(crate) async fn get(&self) -> Option<DebugInfo> {
        let guard = self.data.read().await;
        guard.clone()
    }
}

/// 调试流式广播中心
#[derive(Clone)]
pub(crate) struct DebugStreamHub {
    pub(crate) sender: Arc<tokio::sync::broadcast::Sender<String>>,
}

impl DebugStreamHub {
    pub(crate) fn new() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(DEBUG_BROADCAST_CAPACITY);
        Self {
            sender: Arc::new(sender),
        }
    }

    /// 创建 SSE 流
    pub(crate) fn create_stream(
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

#[derive(Clone, Default)]
pub(crate) struct AdminSessionStore {
    sessions: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
}

impl AdminSessionStore {
    pub(crate) async fn create_session(&self) -> String {
        let token = Uuid::new_v4().to_string();
        let mut guard = self.sessions.write().await;
        let now = Utc::now();
        guard
            .retain(|_, created| now - *created < chrono::Duration::hours(ADMIN_SESSION_TTL_HOURS));
        guard.insert(token.clone(), now);
        token
    }

    pub(crate) async fn validate_session(&self, token: &str) -> bool {
        let mut guard = self.sessions.write().await;
        match guard.get(token) {
            Some(created)
                if Utc::now() - *created < chrono::Duration::hours(ADMIN_SESSION_TTL_HOURS) =>
            {
                true
            }
            Some(_) => {
                guard.remove(token);
                false
            }
            None => false,
        }
    }

    pub(crate) async fn remove_session(&self, token: &str) {
        let mut guard = self.sessions.write().await;
        guard.remove(token);
    }
}

/// key → (窗口内失败次数, 窗口起始时间)
type LoginAttemptMap = HashMap<String, (u32, DateTime<Utc>)>;

/// 内存级登录失败限速：per-IP + 全局兜底（防 X-Forwarded-For 伪造绕过）
#[derive(Clone, Default)]
pub(crate) struct LoginRateLimiter {
    attempts: Arc<RwLock<LoginAttemptMap>>,
}

impl LoginRateLimiter {
    /// 通过返回 Ok(())；被限速返回 Err(剩余秒数)
    pub(crate) async fn check(&self, ip: &str) -> Result<(), i64> {
        let now = Utc::now();
        let window = chrono::Duration::seconds(LOGIN_FAIL_WINDOW_SECS);
        let guard = self.attempts.read().await;
        for (key, limit) in [
            (ip, LOGIN_FAIL_MAX_PER_IP),
            ("global", LOGIN_FAIL_MAX_GLOBAL),
        ] {
            if let Some((count, first)) = guard.get(key) {
                if now - *first < window && *count >= limit {
                    return Err((window - (now - *first)).num_seconds().max(1));
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn record_failure(&self, ip: &str) {
        let now = Utc::now();
        let window = chrono::Duration::seconds(LOGIN_FAIL_WINDOW_SECS);
        let mut guard = self.attempts.write().await;
        if guard.len() > LOGIN_RATE_LIMIT_MAX_ENTRIES {
            guard.retain(|_, (_, first)| now - *first < window);
        }
        for key in [ip, "global"] {
            let entry = guard.entry(key.to_string()).or_insert((0, now));
            if now - entry.1 >= window {
                *entry = (0, now);
            }
            entry.0 += 1;
        }
    }

    pub(crate) async fn clear(&self, ip: &str) {
        let mut guard = self.attempts.write().await;
        guard.remove(ip);
    }
}

pub(crate) fn client_ip(req: &HttpRequest) -> String {
    req.connection_info()
        .realip_remote_addr()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "global".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_admin_session_ttl() {
        let store = AdminSessionStore::default();
        let token = store.create_session().await;
        assert!(store.validate_session(&token).await);

        // 手动把创建时间拨回到过期
        {
            let mut guard = store.sessions.write().await;
            let created = guard.get_mut(&token).unwrap();
            *created = Utc::now() - chrono::Duration::hours(ADMIN_SESSION_TTL_HOURS + 1);
        }
        assert!(!store.validate_session(&token).await);
    }

    #[tokio::test]
    async fn test_login_rate_limiter() {
        let limiter = LoginRateLimiter::default();
        assert!(limiter.check("1.2.3.4").await.is_ok());

        for _ in 0..LOGIN_FAIL_MAX_PER_IP {
            limiter.record_failure("1.2.3.4").await;
        }
        assert!(limiter.check("1.2.3.4").await.is_err());
        // 其他 IP 不受影响（未达全局阈值）
        assert!(limiter.check("5.6.7.8").await.is_ok());

        limiter.clear("1.2.3.4").await;
        assert!(limiter.check("1.2.3.4").await.is_ok());
    }

    #[tokio::test]
    async fn test_login_rate_limiter_global_fallback() {
        let limiter = LoginRateLimiter::default();
        // 不同 IP 累计失败达到全局阈值后，全部被锁
        for i in 0..LOGIN_FAIL_MAX_GLOBAL {
            limiter.record_failure(&format!("10.0.0.{}", i)).await;
        }
        assert!(limiter.check("99.99.99.99").await.is_err());
    }
}
