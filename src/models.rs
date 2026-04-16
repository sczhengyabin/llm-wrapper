use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    /// 上游唯一标识（自动生成或手动指定）
    pub id: String,
    /// 上游名称
    pub name: String,
    /// 上游 API 基础 URL
    pub base_url: String,
    /// API 密钥
    #[serde(default)]
    pub api_key: Option<String>,
    /// 是否启用
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl UpstreamConfig {
    pub fn new(name: String, base_url: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            base_url,
            api_key: None,
            enabled: true,
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
    /// 上游 ID
    pub upstream_id: String,
    /// 参数覆盖列表
    #[serde(default)]
    pub param_overrides: Vec<ParamOverride>,
}

impl ModelAlias {
    pub fn new(alias: String, target_model: String, upstream_id: String) -> Self {
        Self {
            alias,
            target_model,
            upstream_id,
            param_overrides: Vec::new(),
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
}

impl AppConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

/// OpenAI Chat Completion 请求模型
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// OpenAI Model 响应模型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}
