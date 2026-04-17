use llm_wrapper::proxy::apply_param_overrides_inner;
use llm_wrapper::router::RouteResult;
use std::collections::HashMap;

fn create_test_route(
    override_params: HashMap<String, serde_json::Value>,
    default_params: HashMap<String, serde_json::Value>,
) -> RouteResult {
    RouteResult {
        upstream_base_url: "http://localhost:8080".to_string(),
        upstream_api_key: Some("test-key".to_string()),
        target_model: "gpt-4-turbo".to_string(),
        override_params,
        default_params,
    }
}

#[test]
fn test_apply_override_mode_forces_coverage() {
    let mut override_params = HashMap::new();
    override_params.insert("temperature".to_string(), serde_json::json!(0.9));
    let route = create_test_route(override_params, HashMap::new());

    let mut body = serde_json::json!({
        "model": "my-model",
        "temperature": 0.3,
        "messages": [{"role": "user", "content": "Hello"}]
    });

    apply_param_overrides_inner(&mut body, &route);

    // override 模式应该强制覆盖用户设置
    assert_eq!(body["temperature"], serde_json::json!(0.9));
    // model 应该被替换为目标模型
    assert_eq!(body["model"], serde_json::json!("gpt-4-turbo"));
}

#[test]
fn test_apply_default_mode_when_not_set() {
    let mut default_params = HashMap::new();
    default_params.insert("temperature".to_string(), serde_json::json!(0.7));
    let route = create_test_route(HashMap::new(), default_params);

    let mut body = serde_json::json!({
        "model": "my-model",
        "messages": [{"role": "user", "content": "Hello"}]
    });

    apply_param_overrides_inner(&mut body, &route);

    // default 模式：用户未设置时应应用
    assert_eq!(body["temperature"], serde_json::json!(0.7));
}

#[test]
fn test_apply_default_mode_when_already_set() {
    let mut default_params = HashMap::new();
    default_params.insert("temperature".to_string(), serde_json::json!(0.7));
    let route = create_test_route(HashMap::new(), default_params);

    let mut body = serde_json::json!({
        "model": "my-model",
        "temperature": 0.3,
        "messages": [{"role": "user", "content": "Hello"}]
    });

    apply_param_overrides_inner(&mut body, &route);

    // default 模式：用户已设置时不应覆盖
    assert_eq!(body["temperature"], serde_json::json!(0.3));
}

#[test]
fn test_apply_extra_body_expand() {
    let mut override_params = HashMap::new();
    override_params.insert(
        "extra_body".to_string(),
        serde_json::json!({
            "chat_template_kwargs": {
                "enable_thinking": false
            }
        })
    );
    let route = create_test_route(override_params, HashMap::new());

    let mut body = serde_json::json!({
        "model": "my-model",
        "messages": [{"role": "user", "content": "Hello"}]
    });

    apply_param_overrides_inner(&mut body, &route);

    // extra_body 应该展开到请求体顶层
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], serde_json::json!(false));
    // extra_body 键本身不应该存在
    assert!(body.get("extra_body").is_none());
}

#[test]
fn test_apply_model_replacement() {
    let route = create_test_route(HashMap::new(), HashMap::new());

    let mut body = serde_json::json!({
        "model": "my-alias",
        "messages": [{"role": "user", "content": "Hello"}]
    });

    apply_param_overrides_inner(&mut body, &route);

    // model 应该被替换为目标模型
    assert_eq!(body["model"], serde_json::json!("gpt-4-turbo"));
}

#[test]
fn test_apply_both_override_and_default() {
    let mut override_params = HashMap::new();
    let mut default_params = HashMap::new();

    override_params.insert("temperature".to_string(), serde_json::json!(0.9));
    default_params.insert("top_p".to_string(), serde_json::json!(0.9));

    let route = create_test_route(override_params, default_params);

    let mut body = serde_json::json!({
        "model": "my-model",
        "temperature": 0.3,
        "messages": [{"role": "user", "content": "Hello"}]
    });

    apply_param_overrides_inner(&mut body, &route);

    // override 强制覆盖
    assert_eq!(body["temperature"], serde_json::json!(0.9));
    // default 应用（因为用户没设置）
    assert_eq!(body["top_p"], serde_json::json!(0.9));
}

#[test]
fn test_apply_empty_body() {
    let route = create_test_route(HashMap::new(), HashMap::new());

    let mut body = serde_json::json!({});

    apply_param_overrides_inner(&mut body, &route);

    // 即使空 body，也应该设置 model
    assert_eq!(body["model"], serde_json::json!("gpt-4-turbo"));
}
