<p align="center">
  <img src="logo.svg" alt="LLM Wrapper Logo" width="180">
</p>

# LLM Wrapper

一个轻量级的 LLM API 聚合网关（Rust + actix-web），类似 litellm 但更精简。多上游聚合、协议转换、参数注入、OAuth 上游接入，配备可视化 WebUI 管理。

[English](README_en.md)

## 功能特点

- **多上游聚合**：统一入口聚合多个上游 LLM API
- **多协议支持**：Chat Completions、Responses、Anthropic Messages，可在协议间自动转换
- **模型别名与路由**：为上游模型定义本地别名，支持参数注入
- **参数注入**：`override`（强制覆盖）与 `default`（仅在用户未设置时生效）两种模式
- **OAuth 上游**：内置 CLIProxyAPI 侧车，管理 Claude / Codex 账号登录与 token 自动刷新
- **额度查询**：查询 CLIProxyAPI 账号的用量与额度
- **配置热更新**：修改配置文件或在 WebUI 保存后立即生效，无需重启
- **可视化 WebUI**：单文件管理界面，支持配置编辑、模型聚合、调试面板
- **管理员认证**：Argon2 密码哈希 + HttpOnly Session Cookie，保护管理后台
- **客户端 API Key**：可选的 `/v1/*` 接口鉴权（Bearer / x-api-key）
- **密钥脱敏**：管理接口返回的密钥自动脱敏
- **调试模式**：`X-Debug-Mode` 头返回完整的请求/响应链路数据，支持 SSE 实时流

## 快速开始

CLIProxyAPI 以 git 子模块集成，克隆时请包含子模块：

```bash
git clone --recursive <仓库地址>      # 或已克隆后执行 git submodule update --init
cargo build --release
./target/release/llm-wrapper           # 默认监听 0.0.0.0:3000
```

命令行参数（优先级高于环境变量）：

```bash
llm-wrapper -c config.yaml -a 0.0.0.0:3000
#   -c, --config <PATH>   配置文件路径（默认 config.yaml）
#   -a, --addr <ADDR>     监听地址（默认 0.0.0.0:3000）
```

首次访问 WebUI（`http://localhost:3000`）会引导设置管理员密码。

### Docker 部署

```bash
docker run -d --name llm-wrapper \
  -p 3000:3000 -p 8317:8317 \
  -v $(pwd)/config:/app/config \
  -v llm-wrapper-data:/app/.llm-wrapper \
  -e CONFIG_PATH=/app/config/config.yaml \
  sczhengyabin/llm-wrapper:latest
```

- 端口：`3000` 主 API 与 WebUI，`8317` CLIProxyAPI（Claude/Codex OAuth）
- 数据卷：`/app/config` 配置目录，`/app/.llm-wrapper` token 缓存与账号数据
- 或使用 `docker-compose up -d`

## 配置

复制 `config.yaml.example` 为 `config.yaml`，核心结构：

```yaml
upstreams:
  - name: vllm                       # 上游唯一标识
    base_url: http://127.0.0.1:30002
    auth:
      type: api_key                  # api_key / anthropic_oauth / codex_oauth
      key: null                      # api_key 时填写密钥，OAuth 时省略
    enabled: true
    support_chat_completions: true
    support_responses: false
    support_anthropic_messages: false

aliases:
  - alias: qwen                      # 请求中的 model 仅匹配此字段
    target_model: Qwen/Qwen3-...     # 转发时替换的真实模型名（不参与路由）
    upstream: vllm
    param_overrides:
      - key: temperature
        value: 0.7
        mode: default                # default 或 override
    source: manual                   # manual 手动 / auto 点击模型标签自动创建

# 可选：开启后上游不支持入口协议时自动转换
# allow_protocol_conversion: true

# 可选：开启 /v1/* 接口鉴权，客户端需携带 Authorization: Bearer <key>
# client_api_keys:
#   - name: "本机"
#     key: "your-client-api-key"
```

**认证类型**：`api_key`（静态密钥）、`anthropic_oauth`、`codex_oauth`（后两者由 CLIProxyAPI 管理，在 WebUI 一键登录，token 自动刷新）。

**路由规则**：请求的 `model` 仅匹配 `alias` 字段；`target_model` 仅用于转发替换，不参与路由。若想直接用上游模型名调用，在 WebUI 点击模型标签创建 auto alias 即可。

## 使用

```bash
# Chat Completions（兼容 OpenAI）
curl http://localhost:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model": "qwen", "messages": [{"role": "user", "content": "Hello"}]}'

# 模型列表 / Responses / Anthropic Messages
curl http://localhost:3000/v1/models
# POST /v1/responses          需上游支持 Responses 协议
# POST /v1/messages           需上游支持 Anthropic 协议
```

接口入口：`/v1/*` 客户端 API、`/api/*` 管理 API（需管理员登录）、`/` WebUI。

调试：请求加 `X-Debug-Mode: true` 头即可在响应中获得完整链路数据（客户端请求、上游 URL、注入后的上游请求、上游响应等），或在 WebUI 调试面板查看 SSE 实时流。
