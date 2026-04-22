# 功能特性景观

**领域:** LLM API 聚合网关
**调研时间:** 2026-04-22
**整体置信度:** MEDIUM（基于 LiteLLM、OpenRouter、vLLM 等主流方案的对比分析）

## 执行摘要

LLM API 聚合网关的核心价值在于为多个上游 LLM 服务提供统一入口，在入口层完成模型路由、参数注入、鉴权管理和可观测性。当前市场领导者 LiteLLM 已确立了 AI Gateway 的功能标准，支持 100+ LLM 提供商的统一 OpenAI 格式接口。

对于 `llm-wrapper` 这样的轻量级项目，功能范围应聚焦于**核心路由与参数管理**，而非追求企业级完整功能集。当前设计已覆盖 alias 路由 + 参数注入 + 配置热更新 + 可视化调试，这是适合本地部署/小团队场景的合理定位。

---

## Table Stakes（预期功能）

缺失这些功能会让产品感觉不完整。这些是用户期望的"最小可用网关"功能。

| 功能 | 为什么预期 | 复杂度 | 现状 | 备注 |
|------|-----------|--------|------|------|
| **多上游支持** | 网关存在的根本原因 | 低 | ✅ | 当前通过 UpstreamConfig 实现 |
| **模型别名映射** | 隐藏真实上游模型名，提供稳定接口 | 低 | ✅ | ModelAlias 已实现 |
| **统一 OpenAI 格式** | 客户端只需一套代码 | 中 | ✅ | 当前支持 /chat/completions, /responses, /messages |
| **参数覆盖 (Override)** | 强制特定参数（如 temperature） | 低 | ✅ | OverrideMode 已实现 |
| **参数默认 (Default)** | 用户提供默认值，可被覆盖 | 低 | ✅ | 与 Override 配合 |
| **配置热更新** | 无需重启即可修改上游/别名 | 中 | ✅ | ConfigManager + notify 已实现 |
| **配置持久化 (YAML)** | 人类可读、可版本控制的配置 | 低 | ✅ | 当前使用 YAML |
| **上游健康状态** | 知道哪些 upstream 可用 | 中 | ❌ | 缺失，但非紧急 |
| **基础日志/调试** | 查看请求/响应内容 | 低 | ✅ | DebugInfo + SSE 已实现 |

### 多上游支持的标准模式

从 LiteLLM 的 100+ 提供商支持可以看出，**上游类型**大致分为：

| 类型 | 示例 | 协议差异 |
|------|------|----------|
| OpenAI 原生 | OpenAI, Azure, OpenRouter | 完全 OpenAI 格式 |
| OpenAI 兼容 | vLLM, Ollama, LM Studio, vLLM | 基本兼容，可能缺少部分字段 |
| Anthropic | Anthropic, Vertex AI (Claude) | 需要协议转换 |
| Google | Gemini, Vertex AI (Gemini) | 部分兼容 |
| 其他 | Bedrock, Cohere, AI21 | 需要协议适配 |

**对于 `llm-wrapper` 的启示：**
- 当前设计以"JSON body + model 字段"为中心，天然适合 OpenAI 兼容上游
- 若要支持非 OpenAI 协议（如纯 Anthropic 格式），需要在 proxy 层增加协议转换逻辑

---

## 不同化功能（Differentiators）

这些功能不是用户预期的，但能提供显著价值。

| 功能 | 价值主张 | 复杂度 | 适合阶段 |
|------|----------|--------|----------|
| **参数策略模板** | 为不同场景预设参数组合（如 "cost-optimized", "high-accuracy"） | 中 | Phase 2+ |
| **请求/响应改写** | 修改 prompt 模板、添加系统消息、过滤敏感词 | 高 | Phase 3+ |
| **多模型融合路由** | 根据 prompt 复杂度自动选择上游模型 | 高 | Phase 3+ (需要 ML) |
| **成本估算与限制** | 根据 token 数估算成本，设置预算上限 | 中 | Phase 2+ |
| **A/B 测试路由** | 按百分比将请求分发到不同模型 | 低 | Phase 2 |
| **自动故障转移** | 上游失败时自动切换到备用 | 中 | Phase 2 |
| **请求缓存** | 缓存相同 prompt 的响应 | 中 | Phase 2 |
| **Rate Limit 按 Key** | 为不同虚拟 key 设置不同速率限制 | 中 | Phase 2+ |
| **Spend Tracking** | 追踪每个 key/team 的花费 | 高 | Phase 3 (需要数据库) |

---

## 应避免的功能（Anti-Features）

明确**不构建**的功能，避免项目范围膨胀。

| 功能 | 为什么避免 | 应该做什么 |
|------|-----------|-----------|
| **自建 LLM 推理** | 这是 vLLM/oLLama 的职责 | 专注路由，不自研推理引擎 |
| **完整的管理 Dashboard** | 复杂度高，WebUI 应保持轻量 | 提供基础 WebUI + API 即可 |
| **复杂权限系统** | RBAC/ABAC 是独立产品领域 | 简单的 API key 鉴权足够 |
| **数据库集成** | 增加部署复杂度 | 保持无状态或文件存储 |
| **多租户隔离** | 企业级功能，不符合轻量定位 | 单租户或简单虚拟 key |
| **自定义协议转换** | 协议适配应向上游或专用库 | 专注于 OpenAI 兼容协议 |
| **LLM 训练/微调** | 与网关职责无关 | 明确排除 |

---

## 功能依赖关系

```
基础路由 (Alias → Upstream)
    ↓
参数覆盖 (Override/Default)
    ↓
请求改写 (model 替换 + 参数注入)
    ↓
上游转发 (Proxy)
    ↓
响应处理 (JSON/SSE)
    ↓
调试信息 (DebugInfo)
```

**关键依赖链：**
1. **路由层** 依赖 **配置层**（Alias 定义）
2. **代理层** 依赖 **路由层**（RouteResult）
3. **调试功能** 依赖 **代理层**（请求/响应数据）

---

## 竞品功能对比

### LiteLLM（市场领导者）

**核心能力：**
- 100+ LLM 提供商支持
- Python SDK + AI Gateway (Proxy Server)
- 虚拟 key 管理、花费追踪
- 负载均衡、速率限制
- Guardrails（内容过滤）
- MCP 服务器集成
- A2A Agent 网关

**定位：** 企业级完整解决方案

### OpenRouter

**核心能力：**
- 聚合多个 LLM 提供商
- 统一定价模型
- 模型路由与自动故障转移
- 内容过滤与安全层

**定位：** 消费级模型聚合服务

### vLLM

**核心能力：**
- 专注于 LLM 推理与 serving
- PagedAttention、连续批处理
- 高吞吐、低延迟
- OpenAI 兼容 API 服务器

**定位：** 模型推理引擎，而非网关

---

## MVP 推荐

基于 `llm-wrapper` 当前状态和轻量定位，推荐优先构建：

### Phase 1（当前已覆盖）
1. 多上游支持 ✅
2. 模型别名映射 ✅
3. 参数覆盖 (Override/Default) ✅
4. 配置热更新 ✅
5. 基础调试功能 ✅

### Phase 2（建议添加）
1. **自动故障转移** - 上游失败时切换到备用
2. **A/B 测试路由** - 按百分比分发请求
3. **上游健康检查** - 定期探测上游可用性

### Phase 3（可选增强）
1. **请求缓存** - 减少重复请求
2. **成本估算** - 简单的 token 数估算
3. **虚拟 Key 管理** - 多用户基础支持

### 延迟构建
- **Spend Tracking** - 需要数据库，复杂度上升
- **Guardrails** - 需要额外的 ML 模型或 API
- **复杂权限系统** - 不符合轻量定位

---

## 功能缺口分析

与 LiteLLM 等成熟方案对比，`llm-wrapper` 的差距主要在：

| 类别 | LiteLLM | llm-wrapper | 优先级 |
|------|---------|-------------|--------|
| 上游数量 | 100+ | 依赖配置 | 低（配置驱动） |
| 虚拟 Key 管理 | ✅ | ❌ | 中 |
| 负载均衡 | ✅ (轮询/权重) | ❌ | 中 |
| 速率限制 | ✅ | ❌ | 低 |
| 花费追踪 | ✅ | ❌ | 低 |
| Guardrails | ✅ | ❌ | 低 |
| 健康检查 | ✅ | ❌ | 中 |
| 自动故障转移 | ✅ | ❌ | 中 |
| 请求缓存 | ✅ | ❌ | 低 |
| WebUI | ✅ (Admin Dashboard) | ✅ (基础) | - |

**结论：** `llm-wrapper` 的核心价值不在于功能数量，而在于**轻量、易部署、易理解**。功能缺口大多是企业级功能，对于本地部署/小团队场景非必需。

---

## 来源

- LiteLLM GitHub: https://github.com/BerriAI/litellm
- LiteLLM 官网：https://litellm.ai/
- vLLM GitHub: https://github.com/vllm-project/vllm
- OpenRouter API: https://openrouter.ai/docs
