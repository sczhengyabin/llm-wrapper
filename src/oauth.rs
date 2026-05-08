use crate::models::{CachedToken, UpstreamAuth};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

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

/// 磁盘上的 token 缓存结构
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenCacheFile {
    #[serde(default)]
    pub tokens: HashMap<String, CachedToken>,
}

/// OAuth 认证管理器（仅用于 token 缓存读写，CLIProxyAPI 管理实际认证）
#[derive(Clone)]
pub struct AuthManager {
    token_cache_path: PathBuf,
    cache: Arc<Mutex<TokenCacheFile>>,
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

        Self {
            token_cache_path: cache_path,
            cache: Arc::new(Mutex::new(TokenCacheFile::default())),
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
        _upstream_name: &str,
        auth: &UpstreamAuth,
    ) -> Option<String> {
        match auth {
            UpstreamAuth::ApiKey { key } => key.clone(),
            // CLIProxyAPI 管理的认证由 CLIProxyAPI 自身处理 token
            UpstreamAuth::AnthropicOAuth | UpstreamAuth::CodexOAuth => None,
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
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new(None)
    }
}
