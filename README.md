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
- **Auto Alias**：点击上游模型标签一键创建透传别名

## 快速开始

### 构建

```bash
cargo build --release
```

### 运行

```bash
./target/release/llm-wrapper
```

### Docker 部署

**使用 Docker 运行：**

```bash
docker run -d \
  --name llm-wrapper \
  -p 3000:3000 \
  -v $(pwd)/config:/app/config \
  -e BIND_ADDR=0.0.0.0:3000 \
  -e CONFIG_PATH=/app/config/config.yaml \
  llm-wrapper:latest
```

**使用 docker-compose：**

```bash
# 启动
docker-compose up -d

# 查看日志
docker-compose logs -f

# 停止
docker-compose down
```

**构建镜像：**

```bash
docker build -t llm-wrapper:latest .
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
    source: manual  # 手动创建的别名
```

## 路由规则

- **Alias 匹配**：请求中的 `model` 参数仅匹配 `alias` 字段
- **Target Model 不参与路由**：`target_model` 仅用于转发时替换模型名，不作为路由匹配条件
- **Upstream 直接调用**：如果未找到 alias 匹配，且 `model` 与某个启用的上游 `name` 相同，则直接使用该上游

这意味着：
- `alias: my-model -> target_model: gpt-4` 配置下，必须使用 `model: "my-model"` 调用
- 若需支持 `model: "gpt-4"` 调用，需要创建 `alias: gpt-4 -> target_model: gpt-4` 的 auto alias

## API 端点

### 配置管理

- `GET /api/config` - 获取当前配置
- `PUT /api/config` - 更新配置（同时保存到 YAML 文件）

### 上游模型管理

- `GET /api/upstream-models` - 获取所有上游的模型列表
- `POST /api/upstream-models/alias` - 创建上游模型的 auto alias

### OpenAI 兼容 API

- `POST /v1/chat/completions` - 聊天补全
- `POST /v1/responses` - Responses API（需上游支持）
- `POST /v1/messages` - Anthropic Messages API（需上游支持）
- `GET /v1/models` - 模型列表（返回所有 alias）

### 调试接口

- `GET /api/debug` - 获取最近一次调试信息
- `DELETE /api/debug` - 清空调试信息
- `GET /api/debug/stream` - SSE 流式调试信息

### WebUI

- `GET /` - WebUI 管理界面

## WebUI 功能

### 聚合模型列表

页面顶端展示所有可通过 `/v1/models` 获取的模型别名，按上游分组。

### 上游模型标签

- **蓝色虚线边框**：可用模型，点击创建 auto alias
- **绿色实线边框**：已启用 auto alias，点击可删除
- **红色背景**：alias 名冲突，无法创建

### Auto Alias

Auto alias 是透传别名：`alias = target_model = upstream model name`，无参数覆盖。

**创建方式：**
- WebUI：点击上游配置卡片中的模型标签
- API：`POST /api/upstream-models/alias`

**删除方式：**
- WebUI：点击已启用的绿色模型标签
- 手动删除 alias

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

### 创建 Auto Alias

```bash
curl -X POST http://localhost:3000/api/upstream-models/alias \
  -H "Content-Type: application/json" \
  -d '{
    "upstream": "qwen-test",
    "model": "Qwen/Qwen3.5-122B-A10B-GPTQ-Int4"
  }'
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

## 调试功能

通过 `X-Debug-Mode: true` 请求头启用调试模式，返回完整的请求/响应调试信息：

```bash
curl -X POST http://localhost:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Debug-Mode: true" \
  -d '{
    "model": "qwen",
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

响应包含：
- `client_request`：客户端发送到 Wrapper 的原始请求
- `endpoint`：调用的端点
- `upstream_request`：Wrapper 发送到上游的请求（已应用参数覆盖）
- `upstream_response`：上游返回的响应
