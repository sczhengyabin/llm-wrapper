use llm_wrapper::config::ConfigManager;
use llm_wrapper::router::ModelRouter;
use std::io::Write;
use tempfile::TempDir;

fn create_test_config(content: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.yaml");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(content.as_bytes()).unwrap();
    dir
}

#[tokio::test]
async fn test_route_alias_match() {
    let dir = create_test_config(
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
    let config = ConfigManager::new(&config_path).await.unwrap();
    let router = ModelRouter::new(config);

    let route = router.route("my-model").await;
    assert!(route.is_some());
    let route = route.unwrap();
    assert_eq!(route.upstream_base_url, "http://localhost:8080");
    assert_eq!(route.target_model, "gpt-4");
}

#[tokio::test]
async fn test_route_target_model_match() {
    let dir = create_test_config(
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
    let config = ConfigManager::new(&config_path).await.unwrap();
    let router = ModelRouter::new(config);

    // 使用 target_model 匹配
    let route = router.route("gpt-4").await;
    assert!(route.is_some());
    let route = route.unwrap();
    assert_eq!(route.target_model, "gpt-4");
}

#[tokio::test]
async fn test_route_upstream_direct() {
    let dir = create_test_config(
        r#"
        upstreams:
          - name: direct-upstream
            base_url: http://localhost:9090
            enabled: true
        aliases: []
        "#
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = ConfigManager::new(&config_path).await.unwrap();
    let router = ModelRouter::new(config);

    // 直接使用 upstream name
    let route = router.route("direct-upstream").await;
    assert!(route.is_some());
    let route = route.unwrap();
    assert_eq!(route.upstream_base_url, "http://localhost:9090");
    assert_eq!(route.target_model, "direct-upstream");
}

#[tokio::test]
async fn test_route_upstream_disabled() {
    let dir = create_test_config(
        r#"
        upstreams:
          - name: disabled-upstream
            base_url: http://localhost:9090
            enabled: false
        aliases: []
        "#
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = ConfigManager::new(&config_path).await.unwrap();
    let router = ModelRouter::new(config);

    // 上游未启用，应返回 None
    let route = router.route("disabled-upstream").await;
    assert!(route.is_none());
}

#[tokio::test]
async fn test_route_not_found() {
    let dir = create_test_config(
        r#"
        upstreams: []
        aliases: []
        "#
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = ConfigManager::new(&config_path).await.unwrap();
    let router = ModelRouter::new(config);

    let route = router.route("nonexistent").await;
    assert!(route.is_none());
}

#[tokio::test]
async fn test_route_override_params() {
    let dir = create_test_config(
        r#"
        upstreams:
          - name: test-upstream
            base_url: http://localhost:8080
            enabled: true
        aliases:
          - alias: my-model
            target_model: gpt-4
            upstream: test-upstream
            param_overrides:
              - key: temperature
                value: 0.9
                mode: override
        "#
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = ConfigManager::new(&config_path).await.unwrap();
    let router = ModelRouter::new(config);

    let route = router.route("my-model").await.unwrap();
    assert_eq!(route.override_params.get("temperature").unwrap(), &serde_json::json!(0.9));
    assert!(route.default_params.is_empty());
}

#[tokio::test]
async fn test_route_default_params() {
    let dir = create_test_config(
        r#"
        upstreams:
          - name: test-upstream
            base_url: http://localhost:8080
            enabled: true
        aliases:
          - alias: my-model
            target_model: gpt-4
            upstream: test-upstream
            param_overrides:
              - key: temperature
                value: 0.5
                mode: default
        "#
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = ConfigManager::new(&config_path).await.unwrap();
    let router = ModelRouter::new(config);

    let route = router.route("my-model").await.unwrap();
    assert_eq!(route.default_params.get("temperature").unwrap(), &serde_json::json!(0.5));
    assert!(route.override_params.is_empty());
}

#[tokio::test]
async fn test_route_upstream_not_found() {
    let dir = create_test_config(
        r#"
        upstreams: []
        aliases:
          - alias: my-model
            target_model: gpt-4
            upstream: nonexistent-upstream
        "#
    );
    let config_path = dir.path().join("config.yaml").to_string_lossy().to_string();
    let config = ConfigManager::new(&config_path).await.unwrap();
    let router = ModelRouter::new(config);

    // 上游不存在，应返回 None
    let route = router.route("my-model").await;
    assert!(route.is_none());
}
