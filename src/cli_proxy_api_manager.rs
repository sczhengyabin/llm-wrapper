#![allow(dead_code)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// 登录结果
pub struct LoginResult {
    pub auth_url: String,
    pub device_code: Option<String>,
}

/// 进行中的登录会话
struct PendingLogin {
    child_pid: u32,
}

/// CLIProxyAPI 进程管理器
pub struct CliProxyApiManager {
    state: Arc<Mutex<ManagerState>>,
}

struct ManagerState {
    config_path: PathBuf,
    cli_proxy_api_dir: PathBuf,
    auth_dir: PathBuf,
    api_key: String,
    management_secret: String,
    endpoint: String,
    child: Option<tokio::process::Child>,
    running: bool,
    shutdown: bool,
    pending_logins: std::collections::HashMap<String, PendingLogin>,
}

impl CliProxyApiManager {
    /// 创建新的 CLIProxyAPI 管理器
    pub fn new(cli_proxy_api_dir: PathBuf, endpoint: String) -> Self {
        let llm_wrapper_dir = cli_proxy_api_dir.parent().unwrap().join(".llm-wrapper");
        let _ = std::fs::create_dir_all(&llm_wrapper_dir);
        let config_path = llm_wrapper_dir.join("cli-proxy-api-config.yaml");

        let api_key = Self::load_or_create_api_key(&config_path);
        // 优先从配置文件读取管理密钥（CLIProxyAPI 可能已哈希化）
        let management_secret =
            Self::load_or_create_management_secret(&config_path, &llm_wrapper_dir);

        Self::generate_config(&cli_proxy_api_dir, &endpoint, &api_key, &management_secret);
        let auth_dir = llm_wrapper_dir.join("cli-proxy-api");
        Self {
            state: Arc::new(Mutex::new(ManagerState {
                config_path,
                cli_proxy_api_dir,
                auth_dir,
                api_key,
                management_secret,
                endpoint,
                child: None,
                running: false,
                shutdown: false,
                pending_logins: std::collections::HashMap::new(),
            })),
        }
    }

    /// 从配置文件加载管理密钥，不存在则创建
    fn load_or_create_management_secret(
        config_path: &PathBuf,
        llm_wrapper_dir: &PathBuf,
    ) -> String {
        if config_path.exists() {
            if let Ok(content) = std::fs::read_to_string(config_path) {
                if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    if let Some(secret) = doc
                        .get("remote-management")
                        .and_then(|r| r.get("secret-key"))
                        .and_then(|s| s.as_str())
                    {
                        return secret.to_string();
                    }
                }
            }
        }
        let secret = format!("sec-{}", uuid::Uuid::new_v4().as_hyphenated());
        let secret_file = llm_wrapper_dir.join("cli-proxy-api-secret");
        let _ = std::fs::write(&secret_file, &secret);
        secret
    }

    /// 从配置文件加载 API key，不存在则创建
    fn load_or_create_api_key(config_path: &PathBuf) -> String {
        if config_path.exists() {
            if let Ok(content) = std::fs::read_to_string(config_path) {
                if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    if let Some(keys) = doc.get("api-keys").and_then(|v| v.as_sequence()) {
                        if let Some(first) = keys.first().and_then(|v| v.as_str()) {
                            return first.to_string();
                        }
                    }
                }
            }
        }
        format!("sk-{}", uuid::Uuid::new_v4().as_hyphenated())
    }

    /// 生成 CLIProxyAPI 配置文件（幂等：如果配置已存在则不覆盖）
    fn generate_config(
        cli_proxy_api_dir: &PathBuf,
        endpoint: &str,
        api_key: &str,
        management_secret: &str,
    ) -> PathBuf {
        let llm_wrapper_dir = cli_proxy_api_dir.parent().unwrap().join(".llm-wrapper");
        let _ = std::fs::create_dir_all(&llm_wrapper_dir);
        let config_path = llm_wrapper_dir.join("cli-proxy-api-config.yaml");

        if config_path.exists() {
            return config_path;
        }

        let addr = endpoint
            .strip_prefix("http://")
            .unwrap_or(endpoint);
        let (host, port) = match addr.split_once(':') {
            Some((h, p)) => (h, p),
            None => ("127.0.0.1", "8317"),
        };

        let yaml = format!(
            r#"host: "{}"
port: {}
auth-dir: "{}/cli-proxy-api"
api-keys:
  - "{}"
remote-management:
  secret-key: "{}"
debug: false
codex-header-defaults:
  user-agent: "codex_cli_rs/0.128.0 (macos; arm64)"
"#,
            host,
            port.parse::<u16>().unwrap_or(8317),
            llm_wrapper_dir.display(),
            api_key,
            management_secret,
        );

        if let Ok(mut f) = std::fs::File::create(&config_path) {
            let _ = f.write_all(yaml.as_bytes());
        }

        config_path
    }

    /// 获取 CLIProxyAPI API key
    pub async fn api_key(&self) -> String {
        self.state.lock().await.api_key.clone()
    }

    /// 获取管理密钥
    pub async fn management_secret(&self) -> String {
        self.state.lock().await.management_secret.clone()
    }

    /// 获取 CLIProxyAPI 端点
    pub async fn endpoint(&self) -> String {
        self.state.lock().await.endpoint.clone()
    }

    /// 检查是否存在账号文件
    pub async fn has_accounts(&self) -> bool {
        let auth_dir = self.state.lock().await.auth_dir.clone();
        Self::check_auth_dir(&auth_dir, None)
    }

    /// 获取账号状态（用于 CLIProxyAPI 未运行时返回状态）
    pub async fn get_account_status(&self) -> serde_json::Value {
        let auth_dir = self.state.lock().await.auth_dir.clone();
        let mut providers = serde_json::Map::new();

        if !auth_dir.is_dir() {
            return serde_json::json!({"providers": providers});
        }

        for entry in std::fs::read_dir(&auth_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
        {
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if !name.ends_with(".json") {
                continue;
            }

            // 确定 provider
            let provider = if name.starts_with("claude") {
                "claude"
            } else if name.starts_with("codex") {
                "codex"
            } else {
                continue;
            };

            // 读取 token 文件获取 email 等信息
            let file_path = entry.path();
            let email = std::fs::read_to_string(&file_path)
                .ok()
                .and_then(|content| {
                    serde_json::from_str::<serde_json::Value>(&content)
                        .ok()
                        .and_then(|v| v.get("email").and_then(|e| e.as_str()).map(|s| s.to_string()))
                })
                .unwrap_or_default();
            let expired = std::fs::read_to_string(&file_path)
                .ok()
                .and_then(|content| {
                    serde_json::from_str::<serde_json::Value>(&content)
                        .ok()
                        .and_then(|v| v.get("expired").and_then(|e| e.as_str()).map(|s| s.to_string()))
                })
                .unwrap_or_default();

            let account = serde_json::json!({
                "email": email,
                "expiresAt": expired,
                "status": "active"
            });

            let entry_val = providers
                .get(provider)
                .unwrap_or(&serde_json::json!({}))
                .clone();
            let mut accounts: Vec<serde_json::Value> = entry_val
                .get("accounts")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default();
            accounts.push(account);
            let mut new_entry = serde_json::Map::new();
            new_entry.insert(
                "account_count".to_string(),
                serde_json::json!(accounts.len()),
            );
            new_entry.insert("accounts".to_string(), serde_json::json!(accounts));
            providers.insert(provider.to_string(), serde_json::Value::Object(new_entry));
        }

        serde_json::json!({"providers": providers})
    }

    fn check_auth_dir(auth_dir: &std::path::Path, provider: Option<&str>) -> bool {
        if !auth_dir.is_dir() {
            return false;
        }
        std::fs::read_dir(auth_dir).map_or(false, |mut entries| {
            entries.any(|entry| match entry {
                Ok(e) => {
                    let name = e.file_name();
                    let name = name.to_string_lossy().to_string();
                    if !name.ends_with(".json") {
                        return false;
                    }
                    if let Some(p) = provider {
                        name.starts_with(p)
                    } else {
                        name.starts_with("claude") || name.starts_with("codex")
                    }
                }
                Err(_) => false,
            })
        })
    }

    fn count_provider_accounts(auth_dir: &std::path::Path, prefix: &str) -> usize {
        if !auth_dir.is_dir() {
            return 0;
        }
        std::fs::read_dir(auth_dir).map_or(0, |entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy().to_string();
                    name.ends_with(".json") && name.starts_with(prefix)
                })
                .count()
        })
    }

    fn find_binary(cli_proxy_api_dir: &PathBuf) -> anyhow::Result<PathBuf> {
        let candidates = [
            cli_proxy_api_dir.join("CLIProxyAPI"),
            cli_proxy_api_dir.join("CLIProxyAPI.exe"),
        ];
        for candidate in &candidates {
            if candidate.exists() {
                return Ok(candidate.to_path_buf());
            }
        }
        Err(anyhow::anyhow!(
            "CLIProxyAPI binary not found in {}. Please build and place the binary in this directory.",
            cli_proxy_api_dir.display()
        ))
    }

    /// 启动登录流程：spawn CLIProxyAPI --{provider}-login --no-browser
    pub async fn start_login(&self, provider: &str) -> anyhow::Result<LoginResult> {
        use tokio::io::AsyncBufReadExt;

        let state = self.state.lock().await;
        let config_path = state.config_path.clone();
        let cli_proxy_api_dir = state.cli_proxy_api_dir.clone();
        drop(state);

        let binary = Self::find_binary(&cli_proxy_api_dir)?;

        let (login_flag, is_device) = match provider {
            "claude" => ("claude-login", false),
            "codex" => ("codex-login", false),
            "codex-device" => ("codex-device-login", true),
            _ => return Err(anyhow::anyhow!("Unsupported provider: {}", provider)),
        };

        info!(
            "Starting CLIProxyAPI {} login for provider: {}",
            if is_device { "device" } else { "OAuth" },
            provider
        );

        let mut child = tokio::process::Command::new(&binary)
            .arg(format!("--config={}", config_path.display()))
            .arg(format!("--{}", login_flag))
            .arg("--no-browser")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn CLIProxyAPI login process")?;

        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow::anyhow!("Failed to take stdout from login process")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            anyhow::anyhow!("Failed to take stderr from login process")
        })?;

        let child_pid = child.id().unwrap_or(0);

        // 逐行读取 stdout 提取 auth URL
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut auth_url = None;
        let mut device_code = None;
        let mut buf = String::new();

        while let Ok(n) = reader.read_line(&mut buf).await {
            if n == 0 {
                break;
            }
            let trimmed = buf.trim().to_string();
            debug!("CLIProxyAPI login output: {}", trimmed);

            if is_device {
                if trimmed.starts_with("Codex device URL: ") {
                    auth_url = Some(trimmed.trim_start_matches("Codex device URL: ").trim().to_string());
                }
                if trimmed.starts_with("Codex device code: ") {
                    device_code = Some(trimmed.trim_start_matches("Codex device code: ").trim().to_string());
                }
                if auth_url.is_some() {
                    break;
                }
            } else {
                if trimmed.contains("Visit the following URL to continue authentication:") {
                    buf.clear();
                    if let Ok(m) = reader.read_line(&mut buf).await {
                        if m > 0 {
                            auth_url = Some(buf.trim().to_string());
                            break;
                        }
                    }
                }
            }
            buf.clear();
        }

        let url = auth_url.ok_or_else(|| {
            anyhow::anyhow!("Auth URL not found in login process output")
        })?;

        info!(
            "CLIProxyAPI login auth_url obtained for provider '{}', storing pending session",
            provider
        );

        // 在后台持续消耗 stdout/stderr，防止缓冲区满导致进程阻塞
        let stdout = reader.into_inner();
        Self::spawn_output_drain(stdout, stderr, child_pid);

        // 存储待完成的登录会话
        let mut state = self.state.lock().await;
        state.pending_logins.insert(
            provider.to_string(),
            PendingLogin { child_pid },
        );

        Ok(LoginResult {
            auth_url: url,
            device_code,
        })
    }

    /// 在后台持续消耗子进程的 stdout 和 stderr，防止缓冲区满导致进程阻塞
    fn spawn_output_drain(
        mut stdout: tokio::process::ChildStdout,
        mut stderr: tokio::process::ChildStderr,
        pid: u32,
    ) {
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let stdout_task = tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                loop {
                    match stdout.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                                debug!("CLIProxyAPI[{}] stdout: {}", pid, text.trim());
                            }
                        }
                        Err(e) => {
                            debug!("CLIProxyAPI[{}] stdout read error: {}", pid, e);
                            break;
                        }
                    }
                }
            });

            let stderr_task = tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                                debug!("CLIProxyAPI[{}] stderr: {}", pid, text.trim());
                            }
                        }
                        Err(e) => {
                            debug!("CLIProxyAPI[{}] stderr read error: {}", pid, e);
                            break;
                        }
                    }
                }
            });

            let _ = (stdout_task.await, stderr_task.await);
            debug!("CLIProxyAPI[{}] output drain finished", pid);
        });
    }

    /// 完成登录：直接 HTTP GET 回调 URL，让 CLIProxyAPI 接收回调
    pub async fn complete_login(
        &self,
        provider: &str,
        callback_url: &str,
    ) -> anyhow::Result<()> {
        let _ = {
            let mut state = self.state.lock().await;
            info!(
                "complete_login for '{}', existing pending: {:?}",
                provider,
                state.pending_logins.keys().collect::<Vec<_>>()
            );
            state.pending_logins.remove(provider)
        };

        info!(
            "Completing login for '{}' by fetching callback URL: {}",
            provider, callback_url
        );

        let auth_dir = self.state.lock().await.auth_dir.clone();
        let state_clone = Arc::clone(&self.state);

        // 非阻塞：在后台执行回调 + 等待 token 保存 + 启动服务
        let callback_url = callback_url.to_string();
        let provider = provider.to_string();
        tokio::spawn(async move {
            // 直接 HTTP GET 回调 URL，CLIProxyAPI 在 1455 端口监听回调
            // 使用 redirect::none() 防止自动跟随 302，我们自己处理
            let resp = reqwest::get(&callback_url).await;
            match resp {
                Ok(r) => {
                    info!(
                        "Callback request returned status {}: {}",
                        r.status(),
                        provider
                    );
                    // 读取响应体，查看是否有错误信息
                    let body = r.text().await.unwrap_or_default();
                    if !body.is_empty() && body.len() < 500 {
                        debug!("Callback response body: {}", body);
                    }
                }
                Err(e) => {
                    // 回调 URL 可能返回 302 重定向，reqwest 默认跟随
                    // 如果连接被拒绝，说明 CLIProxyAPI 的 OAuth 服务器可能已经关闭
                    warn!(
                        "Callback request failed (may be normal if CLIProxyAPI already processed it): {}",
                        e
                    );
                }
            }

            // 等待 token 文件出现，然后启动服务
            for i in 0..30 {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if Self::check_auth_dir(&auth_dir, Some(&provider)) {
                    info!(
                        "Auth file detected for '{}' after {} seconds, starting service",
                        provider,
                        (i + 1) * 2
                    );
                    let mgr = CliProxyApiManager { state: state_clone };
                    if !mgr.is_running().await {
                        if let Err(e) = mgr.start().await {
                            warn!("Failed to start CLIProxyAPI after login: {}", e);
                        }
                    }
                    return;
                }
            }

            warn!(
                "Auth file not detected for '{}' after 60 seconds. Login may have failed.",
                provider
            );
        });

        Ok(())
    }

    /// 启动 CLIProxyAPI 进程
    pub async fn start(&self) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        if state.running {
            return Ok(());
        }
        state.shutdown = false;

        let config_path = state.config_path.clone();
        let cli_proxy_api_dir = state.cli_proxy_api_dir.clone();
        let endpoint = state.endpoint.clone();

        let binary = Self::find_binary(&cli_proxy_api_dir)?;

        info!(
            "Starting CLIProxyAPI: {} --config={}",
            binary.display(),
            config_path.display()
        );

        let mut child = tokio::process::Command::new(&binary)
            .arg(format!("--config={}", config_path.display()))
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .context("Failed to spawn CLIProxyAPI process")?;

        let ready = self.wait_ready(&endpoint).await;

        if ready {
            state.child = Some(child);
            state.running = true;
            info!("CLIProxyAPI started successfully on {}", endpoint);
        } else {
            let _ = child.kill().await;
            state.running = false;
            return Err(anyhow::anyhow!(
                "CLIProxyAPI failed to become ready within timeout"
            ));
        }

        Ok(())
    }

    /// 等待 CLIProxyAPI 就绪
    async fn wait_ready(&self, endpoint: &str) -> bool {
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Ok(true) = Self::health_check_http(endpoint).await {
                return true;
            }
        }
        false
    }

    /// 停止 CLIProxyAPI 进程
    pub async fn stop(&self) {
        let mut state = self.state.lock().await;
        state.shutdown = true;
        if let Some(mut child) = state.child.take() {
            info!("Stopping CLIProxyAPI process...");
            let _ = child.kill().await;
            let _ = child.wait().await;
            state.running = false;
            info!("CLIProxyAPI stopped");
        }
    }

    /// 健康检查
    pub async fn health_check(&self) -> bool {
        let state = self.state.lock().await;
        Self::health_check_http(&state.endpoint).await.unwrap_or(false)
    }

    async fn health_check_http(endpoint: &str) -> anyhow::Result<bool> {
        let health_url = format!("{}/healthz", endpoint);
        match reqwest::get(&health_url).await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    /// 是否正在运行
    pub async fn is_running(&self) -> bool {
        self.state.lock().await.running
    }

    /// 启动后台监控任务（崩溃自动重启）
    pub fn spawn_monitor(self: Arc<Self>) {
        let state = self.state.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;

                let should_restart = {
                    let mut s = state.lock().await;
                    if s.shutdown {
                        break;
                    }
                    if !s.running {
                        continue;
                    }

                    let alive = if let Some(ref mut child) = s.child {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                warn!("CLIProxyAPI exited with status {}. Restarting...", status);
                                s.running = false;
                                false
                            }
                            Ok(None) => true,
                            Err(e) => {
                                error!("Error checking CLIProxyAPI process: {}", e);
                                false
                            }
                        }
                    } else {
                        false
                    };

                    if !alive {
                        true
                    } else if let Ok(false) = Self::health_check_http(&s.endpoint).await {
                        warn!("CLIProxyAPI health check failed, restarting...");
                        s.running = false;
                        if let Some(mut child) = s.child.take() {
                            let _ = child.kill().await;
                        }
                        true
                    } else {
                        false
                    }
                };

                if should_restart {
                    let endpoint = state.lock().await.endpoint.clone();
                    let cli_proxy_api_dir = state.lock().await.cli_proxy_api_dir.clone();
                    let config_path = state.lock().await.config_path.clone();

                    if let Err(e) =
                        Self::restart_inner(&cli_proxy_api_dir, &config_path, &endpoint).await
                    {
                        error!("Failed to restart CLIProxyAPI: {}", e);
                    }
                }
            }
            debug!("CLIProxyAPI monitor stopped");
        });
    }

    async fn restart_inner(
        cli_proxy_api_dir: &PathBuf,
        config_path: &PathBuf,
        endpoint: &str,
    ) -> anyhow::Result<()> {
        let binary = Self::find_binary(cli_proxy_api_dir)?;

        let mut child = tokio::process::Command::new(&binary)
            .arg(format!("--config={}", config_path.display()))
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .context("Failed to spawn CLIProxyAPI process")?;

        let mut ready = false;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Ok(ok) = Self::health_check_http(endpoint).await {
                if ok {
                    ready = true;
                    break;
                }
            }
        }

        if ready {
            info!("CLIProxyAPI restarted successfully");
            Ok(())
        } else {
            let _ = child.kill().await;
            Err(anyhow::anyhow!("CLIProxyAPI restart failed to become ready"))
        }
    }
}
