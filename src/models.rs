use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    /// 上游名称（用作唯一标识）
    pub name: String,
    /// 上游 API 基础 URL
    pub base_url: String,
    /// API 密钥
    #[serde(default)]
    pub api_key: Option<String>,
    /// 是否启用
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 是否支持 OpenAI 协议 (chat/completions, responses)
    #[serde(default = "default_true")]
    pub support_openai: bool,
    /// 是否支持 Anthropic 协议 (messages)
    #[serde(default = "default_false")]
    pub support_anthropic: bool,
    /// 获取模型列表的 URL（默认 {base_url}/v1/models）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_url: Option<String>,
}

fn default_false() -> bool {
    false
}

fn default_true() -> bool {
    true
}

impl UpstreamConfig {
    #[allow(dead_code)]
    pub fn new(name: String, base_url: String) -> Self {
        Self {
            name: name.clone(),
            base_url,
            api_key: None,
            enabled: true,
            support_openai: true,
            support_anthropic: false,
            models_url: None,
        }
    }

    /// 获取上游 ID（现在就是 name）
    #[allow(dead_code)]
    pub fn id(&self) -> &str {
        &self.name
    }

    /// 获取模型列表 URL
    pub fn get_models_url(&self) -> String {
        if let Some(url) = &self.models_url {
            url.clone()
        } else {
            format!("{}/v1/models", self.base_url)
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
