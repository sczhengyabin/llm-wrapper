use crate::models::AppConfig;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};
use std::path::Path;
use notify::Watcher;

/// 配置管理器，支持热更新
#[derive(Clone)]
pub struct ConfigManager {
    inner: Arc<RwLock<ConfigManagerInner>>,
    runtime_handle: Arc<tokio::runtime::Handle>,
}

struct ConfigManagerInner {
    config: AppConfig,
    config_path: String,
}

impl ConfigManager {
    /// 创建新的配置管理器
    pub async fn new(config_path: &str) -> Result<Self> {
        let config = load_config(config_path)?;

        let inner = ConfigManagerInner {
            config,
            config_path: config_path.to_string(),
        };

        let runtime_handle = tokio::runtime::Handle::current();

        let manager = Self {
            inner: Arc::new(RwLock::new(inner)),
            runtime_handle: Arc::new(runtime_handle),
        };

        // 启动文件监听
        manager.start_file_watcher(config_path.to_string());

        Ok(manager)
    }

    /// 获取当前配置
    pub async fn get_config(&self) -> AppConfig {
        let guard = self.inner.read().await;
        guard.config.clone()
    }

    /// 更新配置并保存到文件
    pub async fn update_config(&self, config: AppConfig) -> Result<()> {
        let mut guard = self.inner.write().await;
        let config_path = guard.config_path.clone();
        guard.config = config.clone();

        // 保存到文件
        crate::config::save_config(&config_path, &config)?;

        info!("配置已更新并保存到 {}", config_path);
        Ok(())
    }

    /// 启动文件监听器
    fn start_file_watcher(&self, config_path: String) {
        let manager = self.clone();
        let handle = self.runtime_handle.clone();

        std::thread::spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel();

            let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
                let _ = tx.send(res);
            })
            .expect("无法创建文件监听器");

            watcher
                .watch(Path::new(&config_path), notify::RecursiveMode::NonRecursive)
                .expect("无法监听配置文件");

            info!("已启动配置文件监听：{}", config_path);

            // 等待文件变化
            loop {
                match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                    Ok(Ok(event)) if event.kind.is_modify() || event.kind.is_create() => {
                        info!("配置文件发生变化，重新加载...");
                        let manager = manager.clone();
                        let handle = handle.clone();
                        let path = config_path.clone();
                        handle.spawn(async move {
                            if let Err(e) = manager.reload_from_file(&path).await {
                                warn!("重新加载配置文件失败：{}", e);
                            }
                        });
                    }
                    Ok(_) => {}
                    Err(_) => {} // 超时或错误，继续循环
                }
            }
        });
    }

    /// 从文件重新加载配置
    pub async fn reload_from_file(&self, config_path: &str) -> Result<()> {
        let config = load_config(config_path)?;
        let mut guard = self.inner.write().await;
        guard.config = config;
        info!("配置已从文件重新加载：{}", config_path);
        Ok(())
    }

    /// 获取上游配置
    #[allow(dead_code)]
    pub async fn get_upstream(&self, upstream_name: &str) -> Option<crate::models::UpstreamConfig> {
        let guard = self.inner.read().await;
        guard.config.upstreams.iter()
            .find(|u| u.name == upstream_name && u.enabled)
            .cloned()
    }

    /// 获取所有可用模型列表
    pub async fn get_available_models(&self) -> Vec<String> {
        let guard = self.inner.read().await;
        guard.config.aliases.iter().map(|a| a.alias.clone()).collect()
    }
}

/// 从文件加载配置
fn load_config(path: &str) -> Result<AppConfig> {
    if Path::new(path).exists() {
        let content = std::fs::read_to_string(path)?;
        let config: AppConfig = serde_yaml::from_str(&content)?;
        info!("已从文件加载配置：{}", path);
        Ok(config)
    } else {
        warn!("配置文件不存在，使用默认配置：{}", path);
        Ok(AppConfig::default())
    }
}

/// 保存配置到文件
pub fn save_config(path: &str, config: &AppConfig) -> Result<()> {
    let content = serde_yaml::to_string(config)?;
    std::fs::write(path, content)?;
    Ok(())
}
