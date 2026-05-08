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

    /// 解析 CLIProxyAPI 日志行并重写为 tracing 格式
    /// CLIProxyAPI 格式: [YYYY-MM-DD HH:MM:SS] [--------] [level ] [file:line] message
    fn parse_cli_proxy_api_log(line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }

        // 找前四个 ']' 的位置来提取字段，第四个 ']' 之后的都是消息
        let mut close_brackets = Vec::new();
        for (i, c) in line.char_indices() {
            if c == ']' {
                close_brackets.push(i);
                if close_brackets.len() == 4 {
                    break;
                }
            }
        }

        if close_brackets.len() >= 4 {
            let ts_str = line[1..close_brackets[0]].trim();
            let _ctx = line[close_brackets[0]+3..close_brackets[1]].trim(); // request ID
            let level_str = line[close_brackets[1]+3..close_brackets[2]].trim();
            let src = line[close_brackets[2]+3..close_brackets[3]].trim();
            let msg = line[close_brackets[3]+1..].trim();

            if msg.is_empty() {
                return; // 无消息内容，跳过
            }

            let message = format!("[{}] {}", src, msg);

            let level = match level_str.to_lowercase().as_str() {
                "error" | "err" => tracing::Level::ERROR,
                "warn"  => tracing::Level::WARN,
                "info"  => tracing::Level::INFO,
                "debug" => tracing::Level::DEBUG,
                _       => tracing::Level::TRACE,
            };

            let iso_ts = ts_str.replace(' ', "T");
            let (level_tag, color) = match level {
                tracing::Level::ERROR => ("ERROR", "\x1b[31m"),  // red
                tracing::Level::WARN  => ("WARN",  "\x1b[33m"),  // yellow
                tracing::Level::INFO  => ("INFO",  "\x1b[32m"),  // green
                tracing::Level::DEBUG => ("DEBUG", "\x1b[36m"),  // cyan
                tracing::Level::TRACE => ("TRACE", "\x1b[90m"),  // dim
            };
            let reset = "\x1b[0m";
            let dim = "\x1b[2m";
            let blue = "\x1b[34m";
            eprintln!("{}{}Z{}  {}{}{} {}{}cli_proxy_api{}: {}", dim, iso_ts, reset, color, level_tag, reset, dim, blue, reset, message);
            return;
        }

        // Fallback: 不是标准格式，直接输出
        debug!("[cli_proxy_api] {}", line);
    }

    /// 在后台消费并重写 CLIProxyAPI 日志格式（使用 BufReader 按行读取，避免跨 chunk 断行）
    fn spawn_log_drain(
        stdout: tokio::process::ChildStdout,
        stderr: tokio::process::ChildStderr,
        pid: u32,
    ) {
        info!("CLIProxyAPI[{}] starting log drain", pid);

        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => Self::parse_cli_proxy_api_log(&line),
                    Err(_) => break,
                }
            }
        });

        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut reader = tokio::io::BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => Self::parse_cli_proxy_api_log(&line),
                    Err(_) => break,
                }
            }
        });
    }

    fn find_binary(cli_proxy_api_dir: &PathBuf) -> anyhow::Result<PathBuf> {
        let candidates: Vec<PathBuf> = [
            // 配置文件同级的 cli-proxy-api 目录
            cli_proxy_api_dir.join("CLIProxyAPI"),
            cli_proxy_api_dir.join("CLIProxyAPI.exe"),
            // Docker 等部署环境的固定路径
            PathBuf::from("/app/cli-proxy-api/CLIProxyAPI"),
        ]
        .to_vec();
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

    /// 启动 CLIProxyAPI 进程（严格管理生命周期，不借用外部进程）
    pub async fn start(&self) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        if state.running {
            return Ok(());
        }

        let endpoint = state.endpoint.clone();

        // 杀掉所有可能占用端口的进程（自有 child + 外部残留如登录自动启动的）
        if let Some(mut old_child) = state.child.take() {
            let _ = old_child.kill().await;
            let _ = old_child.wait().await;
        }

        // 如果端口仍有服务在响应（外部进程），查找并杀掉
        if Self::health_check_http(&endpoint).await.unwrap_or(false) {
            info!("CLIProxyAPI detected on {}, killing external process", endpoint);
            if let Err(e) = Self::kill_process_on_port(&endpoint).await {
                warn!("Failed to kill external CLIProxyAPI: {}", e);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        state.shutdown = false;

        let config_path = state.config_path.clone();
        let cli_proxy_api_dir = state.cli_proxy_api_dir.clone();

        let binary = Self::find_binary(&cli_proxy_api_dir)?;

        info!(
            "Starting CLIProxyAPI: {} --config={}",
            binary.display(),
            config_path.display()
        );

        let mut child = tokio::process::Command::new(&binary)
            .arg(format!("--config={}", config_path.display()))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn CLIProxyAPI process")?;

        let pid = child.id().unwrap_or(0);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Self::spawn_log_drain(
            stdout.expect("stdout piped"),
            stderr.expect("stderr piped"),
            pid,
        );

        let ready = self.wait_ready(&endpoint).await;

        let child_alive = child.try_wait().ok().flatten().is_none();
        if ready && child_alive {
            state.child = Some(child);
            state.running = true;
            info!("CLIProxyAPI started successfully on {}", endpoint);
        } else {
            let _ = child.kill().await;
            state.running = false;
            let reason = if child_alive {
                "failed to become ready within timeout"
            } else {
                "process exited before ready"
            };
            return Err(anyhow::anyhow!("CLIProxyAPI {}", reason));
        }

        Ok(())
    }

    /// 查找并杀掉占用端口的进程
    async fn kill_process_on_port(endpoint: &str) -> anyhow::Result<()> {
        let port = endpoint
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .rsplit(':')
            .next()
            .unwrap_or("8317");

        // 优先尝试 fuser（procps-ng，Docker 友好）
        let output = tokio::process::Command::new("fuser")
            .args(["-k", "-9", &format!(":{}", port)])
            .output()
            .await;

        match output {
            Ok(result) if result.status.success() => {
                info!("Killed process on port {} via fuser", port);
                return Ok(());
            }
            _ => {}
        }

        // 尝试 lsof + kill（macOS / 完整系统）
        let output2 = tokio::process::Command::new("lsof")
            .args(["-ti", &format!(":{}", port)])
            .output()
            .await;

        match output2 {
            Ok(result) if result.status.success() => {
                let pids = String::from_utf8_lossy(&result.stdout);
                for pid_str in pids.lines() {
                    if let Ok(pid) = pid_str.trim().parse::<i32>() {
                        info!("Killing external CLIProxyAPI process {}", pid);
                        let _ = tokio::process::Command::new("kill")
                            .arg("-9")
                            .arg(pid.to_string())
                            .output()
                            .await;
                    }
                }
                return Ok(());
            }
            _ => {}
        }

        // 最后尝试 pkill
        let output3 = tokio::process::Command::new("pkill")
            .arg("-9")
            .arg("-f")
            .arg("CLIProxyAPI")
            .output()
            .await;

        match output3 {
            Ok(r) if r.status.success() => {
                info!("Killed CLIProxyAPI via pkill");
                Ok(())
            }
            _ => Err(anyhow::anyhow!("cannot find process on port {}", port)),
        }
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

                // 在锁内决定是否需要重启，并取出旧 child 引用
                let restart_info = {
                    let mut s = state.lock().await;
                    if s.shutdown {
                        break;
                    }
                    if !s.running {
                        continue;
                    }

                    let needs_restart = if let Some(ref mut child) = s.child {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                warn!("CLIProxyAPI exited with status {}. Restarting...", status);
                                s.running = false;
                                true
                            }
                            Ok(None) => {
                                // 进程存活，检查 HTTP 健康
                                match Self::health_check_http(&s.endpoint).await {
                                    Ok(false) => {
                                        warn!("CLIProxyAPI health check failed, restarting...");
                                        s.running = false;
                                        true
                                    }
                                    _ => false,
                                }
                            }
                            Err(e) => {
                                error!("Error checking CLIProxyAPI process: {}", e);
                                s.running = false;
                                true
                            }
                        }
                    } else {
                        // child = None 但 running = true → 进程丢失，需要重启
                        warn!("CLIProxyAPI process lost, restarting...");
                        s.running = false;
                        true
                    };

                    if needs_restart {
                        // 取出旧 child 引用（解锁后由 restart_inner 负责 kill）
                        let old_child = s.child.take();
                        let endpoint = s.endpoint.clone();
                        let cli_proxy_api_dir = s.cli_proxy_api_dir.clone();
                        let config_path = s.config_path.clone();
                        Some((old_child, endpoint, cli_proxy_api_dir, config_path))
                    } else {
                        None
                    }
                };

                if let Some((old_child, endpoint, cli_proxy_api_dir, config_path)) = restart_info {
                    match Self::restart_inner(
                        &cli_proxy_api_dir,
                        &config_path,
                        &endpoint,
                        old_child,
                    )
                    .await
                    {
                        Ok(new_child) => {
                            let mut s = state.lock().await;
                            s.child = Some(new_child);
                            s.running = true;
                        }
                        Err(e) => {
                            error!("Failed to restart CLIProxyAPI: {}", e);
                        }
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
        old_child: Option<tokio::process::Child>,
    ) -> anyhow::Result<tokio::process::Child> {
        // 先杀掉旧进程，释放端口
        if let Some(mut old) = old_child {
            let _ = old.kill().await;
            let _ = old.wait().await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // 如果端口仍有外部进程（如登录自动启动的），也杀掉
        if Self::health_check_http(endpoint).await.unwrap_or(false) {
            info!("External CLIProxyAPI still on port during restart, killing...");
            let _ = Self::kill_process_on_port(endpoint).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        let binary = Self::find_binary(cli_proxy_api_dir)?;

        let mut child = tokio::process::Command::new(&binary)
            .arg(format!("--config={}", config_path.display()))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn CLIProxyAPI process")?;

        let pid = child.id().unwrap_or(0);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Self::spawn_log_drain(
            stdout.expect("stdout piped"),
            stderr.expect("stderr piped"),
            pid,
        );

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

        let child_alive = child.try_wait().ok().flatten().is_none();
        if ready && child_alive {
            info!("CLIProxyAPI restarted successfully");
            Ok(child)
        } else {
            let _ = child.kill().await;
            let reason = if child_alive {
                "failed to become ready within timeout"
            } else {
                "process exited before ready"
            };
            Err(anyhow::anyhow!("CLIProxyAPI restart: {}", reason))
        }
    }
}
