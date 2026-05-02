use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// API 类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum ApiType {
    #[default]
    #[serde(rename = "open_ai", alias = "open_a_i")]
    OpenAI,
    /// ChatGPT Codex 后端 (chatgpt.com/backend-api/codex/responses)
    #[serde(rename = "chatgpt_codex", alias = "chat_gpt_codex")]
    ChatGptCodex,
}

/// 上游认证方式
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UpstreamAuth {
    ApiKey {
        key: Option<String>,
    },
    #[serde(rename = "oauth_device")]
    OAuthDevice {
        client_id: String,
        device_auth_url: String,
        token_url: String,
        #[serde(default)]
        scope: Option<String>,
    },
}

impl Default for UpstreamAuth {
    fn default() -> Self {
        UpstreamAuth::ApiKey { key: None }
    }
}

/// RFC 8628 设备授权响应
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
}

/// OAuth 2.0 Token 响应
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

/// 持久化缓存的 Token
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedToken {
    pub access_token: String,
    pub token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// 设备认证状态（对外 API 使用）
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeviceAuthStatus {
    Pending {
        user_code: String,
        verification_uri: String,
        verification_uri_complete: Option<String>,
        expires_at: String,
    },
    Success {
        message: String,
        expires_at: Option<String>,
    },
    Failed {
        reason: String,
        message: String,
    },
    Expired {
        message: String,
    },
}

/// Alias 来源类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModelAliasSource {
    /// 跟随上游自动创建
    Auto,
    /// 用户手动添加
    #[default]
    Manual,
}

/// 参数覆盖模式
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OverrideMode {
    /// 强制覆盖，用户请求参数也被覆盖
    Override,
    /// 默认值，用户请求参数可以覆盖
    #[default]
    Default,
}

/// 参数覆盖配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamOverride {
    /// 参数名，如 "temperature"
    pub key: String,
    /// 参数值
    pub value: serde_json::Value,
    /// 覆盖模式
    #[serde(default)]
    pub mode: OverrideMode,
}

/// 上游配置
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamConfig {
    /// 上游名称（用作唯一标识）
    pub name: String,
    /// 上游 API 基础 URL
    pub base_url: String,
    /// API 类型（默认 OpenAI）
    #[serde(default)]
    pub api_type: ApiType,
    /// 认证配置（默认 ApiKey(None)）
    #[serde(default = "default_api_key_auth")]
    pub auth: UpstreamAuth,
    /// 是否启用
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 是否支持 Chat Completions 协议
    #[serde(default = "default_true")]
    pub support_chat_completions: bool,
    /// 是否支持 Responses 协议
    #[serde(default = "default_false")]
    pub support_responses: bool,
    /// 是否支持 Anthropic Messages 协议
    #[serde(default = "default_false")]
    pub support_anthropic_messages: bool,
    /// Anthropic 协议独立的 base URL（留空则使用 base_url）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_base_url: Option<String>,
}

#[allow(dead_code)]
fn default_api_key_auth() -> UpstreamAuth {
    UpstreamAuth::ApiKey { key: None }
}

/// 自定义反序列化以兼容旧的 api_key 字段和旧的 support_openai/support_anthropic
impl<'de> serde::Deserialize<'de> for UpstreamConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        #[derive(Deserialize)]
        struct Raw {
            name: Option<String>,
            base_url: Option<String>,
            api_type: Option<ApiType>,
            api_key: Option<String>,
            auth: Option<UpstreamAuth>,
            enabled: Option<bool>,
            // Old fields (backward compat)
            support_openai: Option<bool>,
            support_anthropic: Option<bool>,
            // New fields
            support_chat_completions: Option<bool>,
            support_responses: Option<bool>,
            support_anthropic_messages: Option<bool>,
            anthropic_base_url: Option<String>,
        }

        let raw: Raw = serde::Deserialize::deserialize(deserializer)?;

        let name = raw.name.ok_or_else(|| D::Error::missing_field("name"))?;
        let base_url = raw
            .base_url
            .ok_or_else(|| D::Error::missing_field("base_url"))?;
        let api_type = raw.api_type.unwrap_or_default();

        let auth = match (raw.auth, raw.api_key) {
            (Some(a), _) => a, // 新格式优先
            (None, Some(k)) => UpstreamAuth::ApiKey { key: Some(k) },
            (None, None) => UpstreamAuth::ApiKey { key: None },
        };

        // 协议能力：新字段优先，旧字段迁移
        let has_new_fields = raw.support_chat_completions.is_some()
            || raw.support_responses.is_some()
            || raw.support_anthropic_messages.is_some();

        let (support_chat_completions, support_responses, support_anthropic_messages) =
            if has_new_fields {
                (
                    raw.support_chat_completions.unwrap_or(true),
                    raw.support_responses.unwrap_or(false),
                    raw.support_anthropic_messages.unwrap_or(false),
                )
            } else {
                // 从旧字段迁移
                let support_openai = raw.support_openai.unwrap_or(true);
                let support_anthropic = raw.support_anthropic.unwrap_or(false);
                (support_openai, support_openai, support_anthropic)
            };

        // Codex 强制只支持 responses
        let (support_chat_completions, support_responses, support_anthropic_messages) =
            if api_type == ApiType::ChatGptCodex {
                (false, true, false)
            } else {
                (
                    support_chat_completions,
                    support_responses,
                    support_anthropic_messages,
                )
            };

        Ok(UpstreamConfig {
            name,
            base_url,
            api_type,
            auth,
            enabled: raw.enabled.unwrap_or(true),
            support_chat_completions,
            support_responses,
            support_anthropic_messages,
            anthropic_base_url: raw.anthropic_base_url,
        })
    }
}

#[allow(dead_code)]
fn default_false() -> bool {
    false
}

#[allow(dead_code)]
fn default_true() -> bool {
    true
}

impl UpstreamConfig {
    #[allow(dead_code)]
    pub fn new(name: String, base_url: String) -> Self {
        Self {
            name: name.clone(),
            base_url,
            api_type: ApiType::default(),
            auth: UpstreamAuth::ApiKey { key: None },
            enabled: true,
            support_chat_completions: true,
            support_responses: false,
            support_anthropic_messages: false,
            anthropic_base_url: None,
        }
    }

    /// 获取上游 ID（现在就是 name）
    #[allow(dead_code)]
    pub fn id(&self) -> &str {
        &self.name
    }

    /// 获取模型列表 URL
    pub fn get_models_url(&self) -> String {
        if self.api_type == ApiType::ChatGptCodex {
            format!("{}/codex/models", self.base_url)
        } else {
            format!("{}/v1/models", self.base_url)
        }
    }

    /// 是否为 OAuth 认证
    pub fn is_oauth(&self) -> bool {
        matches!(self.auth, UpstreamAuth::OAuthDevice { .. })
    }

    /// 获取 API Key（仅 ApiKey 类型）
    #[allow(dead_code)]
    pub fn api_key_value(&self) -> Option<&str> {
        match &self.auth {
            UpstreamAuth::ApiKey { key } => key.as_deref(),
            _ => None,
        }
    }
}

/// 模型别名配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAlias {
    /// 别名
    pub alias: String,
    /// 目标模型名称
    pub target_model: String,
    /// 上游名称
    pub upstream: String,
    /// 参数覆盖列表
    #[serde(default)]
    pub param_overrides: Vec<ParamOverride>,
    /// 来源类型
    #[serde(default)]
    pub source: ModelAliasSource,
    /// 最大上下文长度（从上游自动获取，也可手动设置）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_model_len: Option<u32>,
}

impl ModelAlias {
    #[allow(dead_code)]
    pub fn new(alias: String, target_model: String, upstream: String) -> Self {
        Self {
            alias,
            target_model,
            upstream,
            param_overrides: Vec::new(),
            source: ModelAliasSource::Manual,
            max_model_len: None,
        }
    }
}

/// 应用配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    /// 上游列表
    #[serde(default)]
    pub upstreams: Vec<UpstreamConfig>,
    /// 别名列表
    #[serde(default)]
    pub aliases: Vec<ModelAlias>,
    /// 当上游不支持入口协议时，允许自动转换为上游支持的协议
    #[serde(default)]
    pub allow_protocol_conversion: bool,
}

impl AppConfig {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }
}

/// OpenAI Chat Completion 请求模型
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// OpenAI Model 响应模型
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oauth_auth_json_roundtrip() {
        let json = r#"{
            "upstreams": [{
                "name": "test-oauth",
                "base_url": "https://api.openai.com",
                "enabled": true,
                "auth": {
                    "type": "oauth_device",
                    "client_id": "codex",
                    "device_auth_url": "https://auth.openai.com/oauth/authorize",
                    "token_url": "https://auth.openai.com/oauth/token",
                    "scope": "offline"
                }
            }]
        }"#;
        let config: AppConfig = serde_json::from_str(json).expect("JSON parse failed");
        assert!(config.upstreams[0].is_oauth());
    }

    #[test]
    fn test_yaml_roundtrip() {
        let json = r#"{
            "upstreams": [{
                "name": "test-oauth",
                "base_url": "https://api.openai.com",
                "enabled": true,
                "auth": {
                    "type": "oauth_device",
                    "client_id": "codex",
                    "device_auth_url": "https://auth.openai.com/oauth/authorize",
                    "token_url": "https://auth.openai.com/oauth/token",
                    "scope": "offline"
                }
            }, {
                "name": "vllm",
                "base_url": "http://127.0.0.1:30002",
                "enabled": true,
                "api_key": null
            }],
            "aliases": []
        }"#;
        let config: AppConfig = serde_json::from_str(json).expect("JSON parse failed");

        // Serialize to YAML
        let yaml = serde_yaml::to_string(&config).expect("YAML serialize failed");
        eprintln!("YAML:\n{}", yaml);

        // Parse YAML back
        let config2: AppConfig = serde_yaml::from_str(&yaml).expect("YAML parse failed");
        assert_eq!(config2.upstreams.len(), 2);
        assert!(config2.upstreams[0].is_oauth());
        assert!(!config2.upstreams[1].is_oauth());
        // 旧格式默认 support_openai=true → chat_completions=true + responses=true
        assert!(config2.upstreams[1].support_chat_completions);
        assert!(config2.upstreams[1].support_responses);
    }

    #[test]
    fn test_old_support_fields_migration() {
        // 旧字段 support_openai=true, support_anthropic=true
        let json = r#"{
            "upstreams": [{
                "name": "test",
                "base_url": "http://localhost:8080",
                "support_openai": true,
                "support_anthropic": true
            }]
        }"#;
        let config: AppConfig = serde_json::from_str(json).expect("JSON parse failed");
        assert!(config.upstreams[0].support_chat_completions);
        assert!(config.upstreams[0].support_responses);
        assert!(config.upstreams[0].support_anthropic_messages);
    }

    #[test]
    fn test_codex_forces_responses() {
        let json = r#"{
            "upstreams": [{
                "name": "codex",
                "base_url": "https://chatgpt.com/backend-api",
                "api_type": "chatgpt_codex",
                "support_chat_completions": true,
                "support_anthropic_messages": true
            }]
        }"#;
        let config: AppConfig = serde_json::from_str(json).expect("JSON parse failed");
        assert!(!config.upstreams[0].support_chat_completions);
        assert!(config.upstreams[0].support_responses);
        assert!(!config.upstreams[0].support_anthropic_messages);
    }

    #[test]
    fn test_old_api_key_format() {
        let json = r#"{
            "upstreams": [{
                "name": "vllm",
                "base_url": "http://127.0.0.1:30002",
                "enabled": true,
                "api_key": null
            }]
        }"#;
        let config: AppConfig = serde_json::from_str(json).expect("Old format parse failed");
        assert!(!config.upstreams[0].is_oauth());
    }
}
