# LLM Wrapper

一个轻量级的 OpenAI 协议聚合 wrapper，类似 litellm 但功能更精简。

## 功能特点

- **多上游聚合**：支持配置多个上游 OpenAI 兼容 API
- **模型别名**：为上游模型定义本地别名
- **参数设置**：支持 `override`（覆盖）和 `default`（默认）两种模式
- **配置热更新**：WebUI 修改配置后无需重启立即生效
- **YAML 配置**：支持配置文件持久化
- **单文件 WebUI**：纯 HTML + JS 实现的管理界面
- **API 密钥脱敏**：管理接口返回的 API 密钥自动脱敏

## 快速开始

### 构建

```bash
cargo build --release
```

### 运行

```bash
./target/release/llm-wrapper
```

### 环境变量

- `CONFIG_PATH` - 配置文件路径（默认：config.yaml）
- `BIND_ADDR` - 监听地址（默认：127.0.0.1:3000）
- `ADMIN_API_KEY` - 管理接口认证密钥（可选）

## 配置示例

```yaml
# 上游配置（name 作为唯一标识）
upstreams:
  - name: qwen-test
    base_url: http://192.168.100.7:30002
    api_key: null  # 或 "your-api-key"
    enabled: true

# 模型别名配置
aliases:
  - alias: qwen
    target_model: Qwen/Qwen3.5-122B-A10B-GPTQ-Int4
    upstream: qwen-test
    param_overrides:
      - key: temperature
        value: 0.7
        mode: default  # 或 override
      # extra_body 单独配置
      - key: extra_body
        value:
          chat_template_kwargs:
            enable_thinking: false
        mode: default
```

## API 端点

### 配置管理

- `GET /api/config` - 获取当前配置
- `PUT /api/config` - 更新配置（同时保存到 YAML 文件）

### OpenAI 兼容 API

- `POST /v1/chat/completions` - 聊天补全
- `POST /v1/responses` - Responses API（需上游支持）
- `POST /v1/messages` - Anthropic Messages API（需上游支持）
- `GET /v1/models` - 模型列表

### WebUI

- `GET /` - WebUI 管理界面

## 使用示例

### 调用聊天补全

```bash
curl -X POST http://localhost:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen",
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

### 获取模型列表

```bash
curl http://localhost:3000/v1/models
```

### 调用 Responses API

```bash
curl -X POST http://localhost:3000/v1/responses \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen",
    "input": "Hello"
  }'
```

> 注意：Responses API 需要上游服务支持。如果上游仅支持 Chat Completions 协议，响应格式可能不符合 Responses API 规范。

### 调用 Anthropic Messages API

```bash
curl -X POST http://localhost:3000/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: your-anthropic-api-key" \
  -d '{
    "model": "claude-sonnet-4",
    "max_tokens": 1024,
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

> 注意：Messages API 需要上游服务支持 Anthropic 协议（如 Anthropic API）。如果上游不支持，将返回 404/405 错误。

## 项目结构

```
llm-wrapper/
├── Cargo.toml
├── config.yaml
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── router.rs
│   ├── proxy.rs
│   ├── models.rs
│   └── webui/
│       └── index.html
└── README.md
```

## 待办事项

- [ ] Docker 部署支持
- [ ] 上游健康检查
- [ ] 负载均衡策略
- [ ] 请求限流
- [ ] 日志持久化
