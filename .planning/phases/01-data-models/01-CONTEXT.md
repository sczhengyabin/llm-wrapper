# Phase 1: 数据模型扩展 - Context

** gathered:** 2026-04-22
**Status:** Ready for planning

<domain>
## Phase Boundary

**Phase 1 交付**: 扩展 `src/models.rs` 中的配置模型，添加以下新类型和字段：

1. `AliasSource` 枚举 - 区分 auto/manual 创建的 alias
2. `ProtocolSupport` 结构体 - 控制上游支持的协议
3. `ModelAlias.source` 字段 - 记录 alias 来源
4. `UpstreamConfig.protocols` 字段 - 配置协议支持

**边界**: 仅修改数据模型和配置加载/保存逻辑，不涉及路由、WebUI 或 API 变更。

</domain>

<decisions>
## 实现决策

### 序列化格式
- **D-01**: `AliasSource` 使用小写字符串序列化（`auto` / `manual`）
  - 与现有 `OverrideMode` 保持一致
  - YAML 示例：`source: auto`

- **D-02**: `ProtocolSupport` 的两个字段使用布尔值
  - YAML 示例：
    ```yaml
    protocols:
      openai: true
      anthropic: false
    ```

### 默认值策略
- **D-03**: `ProtocolSupport` 默认双协议启用
  - 旧配置文件没有 `protocols` 字段时，自动视为 `openai: true, anthropic: true`
  - 使用 `#[serde(default)]` 配合 `default_protocols()` 函数实现

- **D-04**: `ModelAlias.source` 默认值为 `manual`
  - 从 YAML 手动加载的 alias 默认是 manual
  - 使用 `#[serde(default)]` 配合 `Default` trait 实现

### 向后兼容
- **D-05**: 新增字段使用 `#[serde(default)]` 处理缺失值
  - 旧配置文件可以无缝加载，不会报错
  - 保存时新字段会写入 YAML，逐步更新配置

### Claude's Discretion
- 具体的函数命名风格（`default_protocols()` vs `protocols_default()`）
- `AliasSource` 是否实现 `Default` trait
- 是否需要添加构造函数辅助创建

</decisions>

<canonical_refs>
## Canonical References

**下游 agents 必须在规划或实现前阅读这些文档。**

### 项目规划文档
- `../PROJECT.md` — 项目愿景和需求概述
- `../REQUIREMENTS.md` — 详细需求文档（F2: Alias 标签系统，F4: 协议支持开关）
- `../ROADMAP.md` — Phase 1 任务清单

### 代码参考
- `src/models.rs` — 现有数据模型定义
- `src/config.rs` — 配置加载/保存逻辑

</canonical_refs>

<code_context>
## Existing Code Insights

### 现有模式
- **序列化风格**: 使用 `#[serde(rename_all = "lowercase")]` 和 `#[serde(default)]`
- **默认值函数**: `fn default_true() -> bool { true }` 模式
- **构造函数**: `::new()` 方法提供便捷创建

### 可复用资产
- `OverrideMode` 枚举 — 可参考其序列化设计
- `UpstreamConfig::new()` — 可添加类似构造函数给新类型

### 集成点
- 修改 `AppConfig` 后，`config.rs` 中的 `load_config()` 和 `save_config()` 会自动支持
- `router.rs` 和 `main.rs` 会在后续 phase 中使用新字段

</code_context>

<specifics>
## 具体想法

无特定要求 — 遵循现有代码风格即可。

</specifics>

<deferred>
## 延迟的想法

**Phase 1 范围外**（属于后续 phases）:
- 协议路由检查逻辑（Phase 2）
- Alias 管理 API（Phase 3）
- WebUI 交互（Phase 4-6）

**Reviewed Todos (not folded)**:
None — 讨论保持在 Phase 1 范围内。

</deferred>

---

*Phase: 01-data-models*
*Context gathered: 2026-04-22*
