use llm_wrapper::config::{load_config, save_config};
use llm_wrapper::models::*;
use std::io::Write;
use tempfile::TempDir;

fn create_temp_file(content: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.yaml");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(content.as_bytes()).unwrap();
    dir
}

#[test]
fn test_load_valid_config() {
    let dir = create_temp_file(
        r#"
        upstreams:
          - name: test-upstream
            base_url: http://localhost:8080
            enabled: true
        aliases:
          - alias: my-model
            target_model: gpt-4
            upstream: test-upstream
        "#,
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = load_config(&config_path).unwrap();

    assert_eq!(config.upstreams.len(), 1);
    assert_eq!(config.upstreams[0].name, "test-upstream");
    assert_eq!(config.aliases.len(), 1);
    assert_eq!(config.aliases[0].alias, "my-model");
}

#[test]
fn test_load_empty_config() {
    let dir = create_temp_file("");
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = load_config(&config_path).unwrap();

    assert!(config.upstreams.is_empty());
    assert!(config.aliases.is_empty());
}

#[test]
fn test_load_nonexistent_file() {
    let config = load_config("/nonexistent/path/config.yaml").unwrap();

    // 文件不存在应返回默认配置
    assert!(config.upstreams.is_empty());
    assert!(config.aliases.is_empty());
}

#[test]
fn test_save_and_load_config() {
    let dir = TempDir::new().unwrap();
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();

    let original_config = AppConfig {
        upstreams: vec![UpstreamConfig {
            name: "test".to_string(),
            base_url: "http://localhost:8080".to_string(),
            api_type: ApiType::default(),
            auth: UpstreamAuth::ApiKey {
                key: Some("key".to_string()),
            },
            enabled: true,
            support_chat_completions: true,
            support_responses: false,
            support_anthropic_messages: false,
            anthropic_base_url: None,
        }],
        aliases: vec![ModelAlias {
            alias: "alias1".to_string(),
            target_model: "model1".to_string(),
            upstream: "test".to_string(),
            param_overrides: vec![],
            source: ModelAliasSource::Manual,
            max_model_len: None,
        }],
        allow_protocol_conversion: false,
    };

    save_config(&config_path, &original_config).unwrap();
    let loaded_config = load_config(&config_path).unwrap();

    assert_eq!(
        loaded_config.upstreams.len(),
        original_config.upstreams.len()
    );
    assert_eq!(loaded_config.aliases.len(), original_config.aliases.len());
    assert_eq!(loaded_config.upstreams[0].name, "test");
    assert_eq!(loaded_config.aliases[0].alias, "alias1");
}

#[test]
fn test_save_config_with_param_overrides() {
    let dir = TempDir::new().unwrap();
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();

    let config = AppConfig {
        upstreams: vec![UpstreamConfig {
            name: "test".to_string(),
            base_url: "http://localhost:8080".to_string(),
            api_type: ApiType::default(),
            auth: UpstreamAuth::ApiKey { key: None },
            enabled: true,
            support_chat_completions: true,
            support_responses: false,
            support_anthropic_messages: false,
            anthropic_base_url: None,
        }],
        aliases: vec![ModelAlias {
            alias: "my-model".to_string(),
            target_model: "gpt-4".to_string(),
            upstream: "test".to_string(),
            param_overrides: vec![ParamOverride {
                key: "temperature".to_string(),
                value: serde_json::json!(0.7),
                mode: OverrideMode::Default,
            }],
            source: ModelAliasSource::Manual,
            max_model_len: None,
        }],
        allow_protocol_conversion: false,
    };

    save_config(&config_path, &config).unwrap();
    let loaded = load_config(&config_path).unwrap();

    assert_eq!(loaded.aliases[0].param_overrides.len(), 1);
    assert_eq!(loaded.aliases[0].param_overrides[0].key, "temperature");
    assert_eq!(
        loaded.aliases[0].param_overrides[0].mode,
        OverrideMode::Default
    );
}

#[test]
fn test_old_support_openai_migration() {
    let dir = create_temp_file(
        r#"
        upstreams:
          - name: test-upstream
            base_url: http://localhost:8080
            enabled: true
            support_openai: true
            support_anthropic: false
        aliases: []
        "#,
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = load_config(&config_path).unwrap();

    // 旧 support_openai=true 应迁移为 support_chat_completions=true + support_responses=true
    assert_eq!(config.upstreams[0].support_chat_completions, true);
    assert_eq!(config.upstreams[0].support_responses, true);
    assert_eq!(config.upstreams[0].support_anthropic_messages, false);
}

#[test]
fn test_old_support_anthropic_migration() {
    let dir = create_temp_file(
        r#"
        upstreams:
          - name: test-upstream
            base_url: http://localhost:8080
            enabled: true
            support_openai: false
            support_anthropic: true
        aliases: []
        "#,
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = load_config(&config_path).unwrap();

    assert_eq!(config.upstreams[0].support_chat_completions, false);
    assert_eq!(config.upstreams[0].support_responses, false);
    assert_eq!(config.upstreams[0].support_anthropic_messages, true);
}

#[test]
fn test_new_fields_roundtrip() {
    let dir = create_temp_file(
        r#"
        upstreams:
          - name: test-upstream
            base_url: http://localhost:8080
            enabled: true
            support_chat_completions: true
            support_responses: true
            support_anthropic_messages: true
        aliases: []
        "#,
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = load_config(&config_path).unwrap();

    assert_eq!(config.upstreams[0].support_chat_completions, true);
    assert_eq!(config.upstreams[0].support_responses, true);
    assert_eq!(config.upstreams[0].support_anthropic_messages, true);
}

#[test]
fn test_codex_forces_responses_only() {
    let dir = create_temp_file(
        r#"
        upstreams:
          - name: codex-upstream
            base_url: https://chatgpt.com/backend-api
            api_type: chatgpt_codex
            enabled: true
            support_chat_completions: true
            support_anthropic_messages: true
        aliases: []
        "#,
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = load_config(&config_path).unwrap();

    // Codex 强制只支持 responses
    assert_eq!(config.upstreams[0].support_chat_completions, false);
    assert_eq!(config.upstreams[0].support_responses, true);
    assert_eq!(config.upstreams[0].support_anthropic_messages, false);
}

#[test]
fn test_allow_protocol_conversion_default() {
    let dir = create_temp_file(
        r#"
        upstreams: []
        aliases: []
        "#,
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = load_config(&config_path).unwrap();

    assert_eq!(config.allow_protocol_conversion, false);
}

#[test]
fn test_allow_protocol_conversion_true() {
    let dir = create_temp_file(
        r#"
        upstreams: []
        aliases: []
        allow_protocol_conversion: true
        "#,
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = load_config(&config_path).unwrap();

    assert_eq!(config.allow_protocol_conversion, true);
}
