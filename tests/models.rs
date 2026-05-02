use llm_wrapper::models::*;

#[test]
fn test_model_alias_source_serialization() {
    let source = ModelAliasSource::Auto;
    let serialized = serde_json::to_string(&source).unwrap();
    assert_eq!(serialized, "\"auto\"");

    let source = ModelAliasSource::Manual;
    let serialized = serde_json::to_string(&source).unwrap();
    assert_eq!(serialized, "\"manual\"");
}

#[test]
fn test_model_alias_source_deserialization() {
    let source: ModelAliasSource = serde_json::from_str("\"auto\"").unwrap();
    assert_eq!(source, ModelAliasSource::Auto);

    let source: ModelAliasSource = serde_json::from_str("\"manual\"").unwrap();
    assert_eq!(source, ModelAliasSource::Manual);
}

#[test]
fn test_model_alias_with_source() {
    let alias = ModelAlias {
        alias: "test".to_string(),
        target_model: "gpt-4".to_string(),
        upstream: "upstream1".to_string(),
        param_overrides: vec![],
        source: ModelAliasSource::Auto,
        max_model_len: None,
    };

    let serialized = serde_json::to_string(&alias).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();

    assert_eq!(parsed["alias"], "test");
    assert_eq!(parsed["source"], "auto");
}

#[test]
fn test_override_mode_serialization() {
    let override_mode = OverrideMode::Override;
    let serialized = serde_json::to_string(&override_mode).unwrap();
    assert_eq!(serialized, "\"override\"");

    let default_mode = OverrideMode::Default;
    let serialized = serde_json::to_string(&default_mode).unwrap();
    assert_eq!(serialized, "\"default\"");
}

#[test]
fn test_override_mode_deserialization() {
    let mode: OverrideMode = serde_json::from_str("\"override\"").unwrap();
    assert_eq!(mode, OverrideMode::Override);

    let mode: OverrideMode = serde_json::from_str("\"default\"").unwrap();
    assert_eq!(mode, OverrideMode::Default);
}

#[test]
fn test_param_override_serialization() {
    let param_override = ParamOverride {
        key: "temperature".to_string(),
        value: serde_json::json!(0.7),
        mode: OverrideMode::Default,
    };

    let serialized = serde_json::to_string(&param_override).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();

    assert_eq!(parsed["key"], "temperature");
    assert_eq!(parsed["value"], serde_json::json!(0.7));
    assert_eq!(parsed["mode"], "default");
}

#[test]
fn test_param_override_deserialization() {
    let json = r#"{
        "key": "temperature",
        "value": 0.9,
        "mode": "override"
    }"#;

    let param_override: ParamOverride = serde_json::from_str(json).unwrap();

    assert_eq!(param_override.key, "temperature");
    assert_eq!(param_override.value, serde_json::json!(0.9));
    assert_eq!(param_override.mode, OverrideMode::Override);
}

#[test]
fn test_model_alias_new() {
    let alias = ModelAlias::new(
        "my-alias".to_string(),
        "gpt-4".to_string(),
        "test-upstream".to_string(),
    );

    assert_eq!(alias.alias, "my-alias");
    assert_eq!(alias.target_model, "gpt-4");
    assert_eq!(alias.upstream, "test-upstream");
    assert!(alias.param_overrides.is_empty());
}

#[test]
fn test_upstream_config_new() {
    let upstream = UpstreamConfig::new(
        "test-upstream".to_string(),
        "http://localhost:8080".to_string(),
    );

    assert_eq!(upstream.name, "test-upstream");
    assert_eq!(upstream.base_url, "http://localhost:8080");
    assert!(upstream.api_key_value().is_none());
    assert!(upstream.enabled);
}

#[test]
fn test_upstream_config_id() {
    let upstream = UpstreamConfig::new(
        "test-upstream".to_string(),
        "http://localhost:8080".to_string(),
    );

    assert_eq!(upstream.id(), "test-upstream");
}

#[test]
fn test_app_config_new() {
    let config = AppConfig::new();

    assert!(config.upstreams.is_empty());
    assert!(config.aliases.is_empty());
}

#[test]
fn test_app_config_default() {
    let config = AppConfig::default();

    assert!(config.upstreams.is_empty());
    assert!(config.aliases.is_empty());
}

#[test]
fn test_chat_completion_request_serialization() {
    use std::collections::HashMap;

    let request = ChatCompletionRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: "Hello".to_string(),
        }],
        stream: false,
        temperature: Some(0.7),
        top_p: None,
        max_tokens: Some(100),
        stop: None,
        frequency_penalty: None,
        presence_penalty: None,
        seed: None,
        extra: HashMap::new(),
    };

    let serialized = serde_json::to_string_pretty(&request).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();

    assert_eq!(parsed["model"], "gpt-4");
    assert_eq!(parsed["messages"][0]["role"], "user");
    assert_eq!(parsed["messages"][0]["content"], "Hello");
    assert_eq!(parsed["temperature"], 0.7);
    assert!(parsed["top_p"].is_null());
    assert_eq!(parsed["max_tokens"], 100);
}

#[test]
fn test_upstream_config_protocol_fields() {
    let upstream = UpstreamConfig::new("test".to_string(), "http://localhost:8080".to_string());
    assert!(upstream.support_chat_completions);
    assert!(!upstream.support_responses);
    assert!(!upstream.support_anthropic_messages);
}

#[test]
fn test_old_support_openai_json_migration() {
    // 旧格式 JSON 中 support_openai 应迁移为新字段
    let json = r#"{
        "upstreams": [{
            "name": "test",
            "base_url": "http://localhost:8080",
            "enabled": true,
            "support_openai": true,
            "support_anthropic": true
        }]
    }"#;
    let config: AppConfig = serde_json::from_str(json).expect("JSON parse failed");
    assert_eq!(config.upstreams[0].support_chat_completions, true);
    assert_eq!(config.upstreams[0].support_responses, true);
    assert_eq!(config.upstreams[0].support_anthropic_messages, true);
}

#[test]
fn test_codex_forces_responses_json() {
    let json = r#"{
        "upstreams": [{
            "name": "codex",
            "base_url": "https://chatgpt.com/backend-api",
            "api_type": "chatgpt_codex",
            "enabled": true,
            "support_chat_completions": true,
            "support_anthropic_messages": true
        }]
    }"#;
    let config: AppConfig = serde_json::from_str(json).expect("JSON parse failed");
    // Codex 强制只支持 responses
    assert_eq!(config.upstreams[0].support_chat_completions, false);
    assert_eq!(config.upstreams[0].support_responses, true);
    assert_eq!(config.upstreams[0].support_anthropic_messages, false);
}
