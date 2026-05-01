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
        "#
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
            auth: UpstreamAuth::ApiKey { key: Some("key".to_string()) },
            enabled: true,
            support_openai: true,
            support_anthropic: false,
            models_url: None,
        }],
        aliases: vec![ModelAlias {
            alias: "alias1".to_string(),
            target_model: "model1".to_string(),
            upstream: "test".to_string(),
            param_overrides: vec![],
            source: ModelAliasSource::Manual,
            max_model_len: None,
        }],
    };

    save_config(&config_path, &original_config).unwrap();
    let loaded_config = load_config(&config_path).unwrap();

    assert_eq!(loaded_config.upstreams.len(), original_config.upstreams.len());
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
            support_openai: true,
            support_anthropic: false,
            models_url: None,
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
    };

    save_config(&config_path, &config).unwrap();
    let loaded = load_config(&config_path).unwrap();

    assert_eq!(loaded.aliases[0].param_overrides.len(), 1);
    assert_eq!(loaded.aliases[0].param_overrides[0].key, "temperature");
    assert_eq!(loaded.aliases[0].param_overrides[0].mode, OverrideMode::Default);
}
