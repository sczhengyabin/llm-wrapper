use crate::config::ConfigManager;
use crate::models::{ApiType, ModelAlias, OverrideMode, UpstreamAuth};
use std::collections::HashMap;

/// 路由信息，包含选中的上游和需要应用的参数覆盖
#[derive(Debug, Clone)]
pub struct RouteResult {
    /// 上游基础 URL
    pub upstream_base_url: String,
    /// 上游名称
    pub upstream_name: String,
    /// 上游认证配置
    pub upstream_auth: UpstreamAuth,
    /// API 类型
    pub api_type: ApiType,
    /// 实际要使用的模型名称
    pub target_model: String,
    /// 需要强制覆盖的参数（override 模式）
    pub override_params: HashMap<String, serde_json::Value>,
    /// 需要作为默认值的参数（default 模式）
    pub default_params: HashMap<String, serde_json::Value>,
    /// 是否支持 OpenAI 协议
    pub support_openai: bool,
    /// 是否支持 Anthropic 协议
    pub support_anthropic: bool,
    /// Anthropic 协议独立的 base URL（留空则使用 upstream_base_url）
    pub anthropic_base_url: Option<String>,
}

/// 路由器，处理模型到上游的映射
pub struct ModelRouter {
    config: ConfigManager,
}

impl ModelRouter {
    pub fn new(config: ConfigManager) -> Self {
        Self { config }
    }

    /// 根据模型名或别名查找路由
    pub async fn route(&self, model: &str) -> Option<RouteResult> {
        let config = self.config.get_config().await;

        // 查找别名（只匹配 alias 字段，target_model 不参与路由）
        if let Some(alias) = config.aliases.iter().find(|a| {
            a.alias == model
        }) {
            return self.build_route_for_alias(alias, &config).await;
        }

        // 如果没有找到别名，尝试直接使用 model 作为 upstream name
        // 这允许用户直接通过 upstream name 来路由
        if let Some(upstream) = config.upstreams.iter().find(|u| {
            u.name == model
        }).filter(|u| u.enabled) {
            return Some(RouteResult {
                upstream_base_url: upstream.base_url.clone(),
                upstream_name: upstream.name.clone(),
                upstream_auth: upstream.auth.clone(),
                api_type: upstream.api_type.clone(),
                target_model: model.to_string(),
                override_params: HashMap::new(),
                default_params: HashMap::new(),
                support_openai: upstream.support_openai,
                support_anthropic: upstream.support_anthropic,
                anthropic_base_url: upstream.anthropic_base_url.clone(),
            });
        }

        None
    }

    /// 为别名构建路由结果
    async fn build_route_for_alias(
        &self,
        alias: &ModelAlias,
        config: &crate::models::AppConfig,
    ) -> Option<RouteResult> {
        let upstream = config.upstreams.iter()
            .find(|u| u.name == alias.upstream && u.enabled)?;

        let mut override_params = HashMap::new();
        let mut default_params = HashMap::new();

        // 处理参数覆盖
        for param_override in &alias.param_overrides {
            match param_override.mode {
                OverrideMode::Override => {
                    override_params.insert(param_override.key.clone(), param_override.value.clone());
                }
                OverrideMode::Default => {
                    default_params.insert(param_override.key.clone(), param_override.value.clone());
                }
            }
        }

        Some(RouteResult {
            upstream_base_url: upstream.base_url.clone(),
            upstream_name: upstream.name.clone(),
            upstream_auth: upstream.auth.clone(),
            api_type: upstream.api_type.clone(),
            target_model: alias.target_model.clone(),
            override_params,
            default_params,
            support_openai: upstream.support_openai,
            support_anthropic: upstream.support_anthropic,
            anthropic_base_url: upstream.anthropic_base_url.clone(),
        })
    }

    /// 获取所有可用模型列表
    pub async fn get_models(&self) -> Vec<String> {
        self.config.get_available_models().await
    }
}