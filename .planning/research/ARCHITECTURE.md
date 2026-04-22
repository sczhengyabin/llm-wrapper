# 架构模式研究

**领域**: LLM API 聚合网关
**调研日期**: 2026 年 4 月
**整体置信度**: MEDIUM

---

## 推荐架构

基于对当前 `llm-wrapper` 代码分析和 API 网关行业模式的综合研究，当前四层架构设计合理，可在此基础上优化。

```
┌─────────────────────────────────────────────────────────────────┐
│                        入口层 (main.rs)                          │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │ HTTP Routes  │  │ Middleware   │  │ AppState     │          │
│  │ - API端点    │  │ - Logger     │  │ - Config     │          │
│  │ - WebUI      │  │ - CORS       │  │ - DebugStore │          │
│  └──────────────┘  └──────────────┘  └──────────────┘          │
└────────────────────────┬────────────────────────────────────────┘
                         │ HTTP Request
                         ↓
┌─────────────────────────────────────────────────────────────────┐
│                        路由层 (router.rs)                        │
│  ┌────────────────────────────────────────────────────────┐    │
│  │ ModelRouter.route(model) → RouteResult                 │    │
│  │  1. 查找 Alias 匹配                                        │    │
│  │  2. 解析 Upstream                                        │    │
│  │  3. 构建参数策略 (override/default)                      │    │
│  └────────────────────────────────────────────────────────┘    │
└────────────────────────┬────────────────────────────────────────┘
                         │ RouteResult
                         ↓
┌─────────────────────────────────────────────────────────────────┐
│                        转发层 (proxy.rs)                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │ParamOverride │  │ HTTP Client  │  │ Stream Hub   │          │
│  │ - model 替换  │  │ - 请求构建   │  │ - SSE 广播    │          │
│  │ - 参数注入   │  │ - 鉴权附加   │  │ - Chunk 积累  │          │
│  └──────────────┘  └──────────────┘  └──────────────┘          │
└────────────────────────┬────────────────────────────────────────┘
                         │ Upstream Response
                         ↓
┌─────────────────────────────────────────────────────────────────┐
│                     上游服务 (External)                          │
│         OpenAI / Anthropic / vLLM / 其他兼容服务                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## 组件边界

### 当前设计分析

| 组件 | 当前职责 | 评价 |
|------|----------|------|
| **入口层 (main.rs)** | HTTP 路由注册、AppState 管理、Handler 实现 | 职责过重，Handler 逻辑与转发耦合 |
| **配置层 (config.rs)** | 配置加载、内存持有、热更新、文件监听 | 设计清晰，职责单一 |
| **路由层 (router.rs)** | Alias 匹配、Upstream 解析、RouteResult 构建 | 职责清晰，RouteResult 设计优秀 |
| **转发层 (proxy.rs)** | 参数改写、HTTP 转发、流式处理、调试采集 | 职责混合，包含过多协议细节 |

### 推荐的组件边界优化

```
当前问题：
- main.rs 中三个 Handler (chat_completions/responses/messages) 高度重复
- proxy.rs 同时处理 "变换" 和 "传输"
- 流式逻辑硬编码在转发层

建议调整：
┌──────────────────────────────────────────────────────┐
│  协议适配层 (新增)                                    │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐ │
│  │OpenAIAdapter│  │AnthropicAd  │  │协议工厂     │ │
│  └─────────────┘  └─────────────┘  └─────────────┘ │
└──────────────────────────────────────────────────────┘
              ↓ 标准化的 InternalRequest
┌──────────────────────────────────────────────────────┐
│  核心转发层 (proxy.rs 精简)                            │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐ │
│  │ParamEngine  │  │HttpClient   │  │StreamRelay  │ │
│  └─────────────┘  └─────────────┘  └─────────────┘ │
└──────────────────────────────────────────────────────┘
```

---

## 请求/响应数据流

### 当前数据流

```
客户端请求
    ↓
web::Json<serde_json::Value> (JSON body)
    ↓
提取 model 字段
    ↓
ModelRouter.route(model) → RouteResult
    ↓
Proxy.proxy_request_with_debug()
    ├─ 应用参数覆盖 (apply_param_overrides_inner)
    ├─ 构建上游 URL
    ├─ 附加 Bearer Token
    ├─ 发送请求
    └─ 处理响应
        ├─ 流式：bytes_stream() → SSE 广播
        └─ 非流式：bytes() → JSON
```

### 推荐改进的数据流

```
客户端请求
    ↓
ProtocolAdapter.adapt() → InternalRequest
    ├─ 验证 request
    ├─ 标准化 model 位置 (body/query/header)
    └─ 提取统一字段
    ↓
Router.route(InternalRequest.model) → RouteResult
    ↓
Proxy.execute(InternalRequest, RouteResult) → Response
    ├─ ParamEngine.apply(route, request)
    ├─ HttpClient.send(transformed_request)
    └─ StreamRelay.proxy(upstream_stream)
```

**关键改进点**:
1. **协议适配与转发解耦**：Handler 不再重复路由和转发逻辑
2. **InternalRequest 抽象**：支持未来扩展 (multipart、binary)
3. **ParamEngine 独立**：参数覆写逻辑可测试、可复用

---

## 配置管理架构

### 当前设计评价

当前 `ConfigManager` 设计符合热更新模式：

```rust
// 当前实现
ConfigManager {
    inner: Arc<RwLock<ConfigManagerInner>>,  // 内存状态
    runtime_handle: Arc<tokio::runtime::Handle>,  // 异步上下文
}
// + notify 文件监听 → 热重载
```

**优点**:
- 内存快照 + 文件监听，读写分离
- `Arc<RwLock>` 保证并发安全
- Runtime handle 允许在 watcher 中 spawn 异步任务

**潜在问题**:
1. **文件监听在同步线程**：notify 阻塞线程，虽已用 `std::thread::spawn` 隔离，但异常处理较弱
2. **热重载时没有锁升级保护**：写锁期间可能阻塞读请求
3. **配置版本无追踪**：无法判断两次获取的配置是否同一版本

### 推荐增强模式

```rust
// 推荐：带版本追踪的配置管理
struct ConfigSnapshot {
    version: u64,
    config: AppConfig,
    timestamp: Instant,
}

struct ConfigManager {
    inner: Arc<_rwlock<ConfigState>>,
    // 使用 ArcSwap 实现无锁读
    current_config: ArcSwap<ConfigSnapshot>,
    // 文件监听
    watcher: Option<notify::RecommendedWatcher>,
}

// 读路径：ArcSwap.load() - 无锁
// 写路径：文件变化 → 新 ConfigSnapshot → ArcSwap.store()
```

**备选方案对比**:

| 方案 | 读性能 | 写性能 | 实现复杂度 | 适用场景 |
|------|--------|--------|------------|----------|
| `Arc<RwLock>` (当前) | 中 (需加锁) | 中 | 低 | 当前规模足够 |
| `ArcSwap` | 高 (无锁) | 高 | 中 | 高频读取场景 |
| `DashMap` | 高 (分片锁) | 高 | 中 | 多配置项独立更新 |

**建议**: 当前规模保持 `Arc<RwLock>`，待 QPS > 1000 时考虑 `ArcSwap` 优化。

---

## 流式处理架构

### 当前实现分析

当前流式处理围绕 SSE 文本流设计：

```rust
// 当前实现
let stream = response.bytes_stream()
    .map(move |item| {
        // 广播到前端
        if let Ok(text) = std::str::from_utf8(chunk) {
            tokio::spawn(async move {
                let _ = hub.send(text);
            });
        }
        item
    });
```

**关键假设**:
1. 上游响应是 UTF-8 文本
2. chunk 可以直接作为 SSE 内容
3. `stream=true` 触发流式逻辑

### 架构挑战

| 挑战 | 当前处理 | 潜在问题 |
|------|----------|----------|
| **背压 (Backpressure)** | 无处理 | 上游快于下游时内存膨胀 |
| **错误传播** | map 中忽略错误 | chunk 丢失不中断 |
| **连接中断** | broadcast 可能失败 | 前端无重连机制 |
| **二进制流** | 强制 UTF-8 转换 | 不支持音频/图片流 |

### 推荐流式架构模式

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ Upstream    │────▶│ Relay       │────▶│ Client      │
│ Stream      │     │ Buffer      │     │ Stream      │
└─────────────┘     └─────────────┘     └─────────────┘
                          │
                          ▼
                    ┌─────────────┐
                    │ Broadcast   │
                    │ Hub (可选)  │
                    └─────────────┘

关键设计:
1. Relay 层实现背压：buffer 满时暂停上游读取
2. 错误传播：上游错误直接中断下游
3. 协议抽象：StreamRelay 不假设内容类型
```

**Rust 实现建议**:

```rust
// 使用 channel 实现背压友好的 relay
async fn proxy_stream_with_backpressure(
    upstream: reqwest::Response,
    client_tx: actix_web::body::Sender,
    debug_hub: Option<broadcast::Sender<String>>,
) -> Result<(), StreamError> {
    let mut stream = upstream.bytes_stream();
    
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        
        // 1. 发送给用户 (阻塞式，实现背压)
        client_tx.send(bytes.clone().into()).await?;
        
        // 2. 异步广播到调试 Hub (不阻塞主流)
        if let Some(hub) = &debug_hub {
            let _ = hub.send(String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    Ok(())
}
```

**关键点**:
- 使用 `Sender.send().await` 而非 `spawn`，实现自然背压
- 广播到 Hub 用 `_ = ` 忽略失败，避免影响主流程
- 不强制 UTF-8 转换，保留二进制能力

---

## 可扩展性考虑

### 当前扩展边界

| 扩展方向 | 当前支持度 | 所需改动 |
|----------|------------|----------|
| **新增端点** (如 `/v1/audio/transcriptions`) | 低 | 需复制 Handler 模板 |
| **非 JSON 协议** (multipart、binary) | 不支持 | 需重构入口层 |
| **WebSocket 上游** | 不支持 | 需新增协议适配 |
| **多上游负载均衡** | 不支持 | 需扩展 Router 逻辑 |
| **请求/响应修改** | 部分支持 (param override) | ParamEngine 可复用 |

### 推荐扩展架构

```
入口层扩展方向:

当前:
  /v1/chat/completions  ──┐
  /v1/responses     ──────┼── 复制的 Handler
  /v1/messages      ──────┘

推荐:
  ProtocolMatcher.match(request) → Protocol
      ├── OpenAI
      ├── Anthropic
      └── Custom
  
  ProtocolHandler.handle(protocol, route) → Response
      ├── 统一路由调用
      └── 协议特定处理
```

**实施建议**:

1. **短期 (保持当前)**：
   - 继续用复制式 Handler，规模小时足够
   - 将公共逻辑提取为 `proxy_request_with_route()`

2. **中期 (协议抽象)**：
   ```rust
   trait ProtocolHandler {
       fn adapt(&self, raw_request: RawRequest) -> InternalRequest;
       fn route_key(&self, request: &InternalRequest) -> String;
       fn transform_response(&self, response: UpstreamResponse) -> Response;
   }
   ```

3. **长期 (插件化)**：
   - 上游特定逻辑用 plugin 实现
   - Core 仅处理通用转发

### 并发与性能考虑

| 规模 | 当前架构 | 建议优化 |
|------|----------|----------|
| **100 QPS** | 完全足够 | 无 |
| **1K QPS** | 基本足够 | 考虑 ArcSwap 配置 |
| **10K QPS** | 可能瓶颈 | 拆分 Router/Proxy 为独立服务 |
| **100K QPS** | 需重构 | 引入负载均衡层、缓存层 |

**当前架构的性能特点**:
- **配置读取**：`RwLock` 读锁，低并发下无问题
- **流式并发**：每个请求独立 tokio task，受限于 CPU/内存
- **HTTP 客户端**：单个 Client 实例，连接池默认配置

---

## 设计模式总结

### 应保留的核心模式

| 模式 | 位置 | 价值 |
|------|------|------|
| **RouteResult 携带策略** | router.rs | 路由与转发解耦 |
| **ConfigManager 热重载** | config.rs | 运行时可配置 |
| **DebugInfo 完整追踪** | proxy.rs | 可观测性强 |
| **SSE 调试广播** | main.rs | 实时调试体验 |

### 应引入的新模式

| 模式 | 位置 | 价值 |
|------|------|------|
| **ProtocolAdapter** | 新增 | 协议多样性支持 |
| **ParamEngine 独立** | 从 proxy 拆分 | 可测试、可复用 |
| **StreamRelay 抽象** | proxy.rs 重构 | 背压友好、协议无关 |

### 应避免的架构反模式

| 反模式 | 风险 | 替代方案 |
|--------|------|----------|
| **在 Handler 中直接转发** | 逻辑重复、难测试 | 提取为独立方法 |
| **流式硬编码 UTF-8** | 无法扩展二进制 | 保留字节流抽象 |
| **配置无版本追踪** | 难以调试竞争 | 增加 version 字段 |
| **每个请求 new Proxy** | 连接池浪费 | 复用 Client 实例 |

---

## 来源

- actix-web 官方文档: https://docs.rs/actix-web/latest/actix_web/
- 当前代码分析: `src/router.rs`, `src/proxy.rs`, `src/config.rs`, `src/main.rs`
