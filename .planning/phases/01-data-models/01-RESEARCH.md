# Phase 1: 数据模型扩展 - Research

**调研日期:** 2026-04-22
**领域:** Rust 数据模型设计，serde 序列化
**置信度:** HIGH

## 总结

Phase 1 的核心任务是在现有 `src/models.rs` 中扩展两个新的数据类型：`AliasSource` 枚举（用于标记 alias 的来源）和 `ProtocolSupport` 结构体（用于控制上游的协议支持）。这需要在不破坏现有配置文件兼容性的前提下完成。

**核心发现：**
1. Rust/serde 对枚举的默认值实现有明确模式：需要手动实现 `Default` trait
2. 嵌套结构的默认值可通过 `#[serde(default)]` + `Default` trait 或自定义默认函数实现
3. 向后兼容的关键是使用 `#[serde(default)]` 属性处理缺失字段

## 用户约束（来自 CONTEXT.md）

### Locked Decisions

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

- **D-03**: `ProtocolSupport` 默认双协议启用
  - 旧配置文件没有 `protocols` 字段时，自动视为 `openai: true, anthropic: true`
  - 使用 `#[serde(default)]` 配合 `default_protocols()` 函数实现

- **D-04**: `ModelAlias.source` 默认值为 `manual`
  - 从 YAML 手动加载的 alias 默认是 manual
  - 使用 `#[serde(default)]` 配合 `Default` trait 实现

- **D-05**: 新增字段使用 `#[serde(default)]` 处理缺失值
  - 旧配置文件可以无缝加载，不会报错
  - 保存时新字段会写入 YAML，逐步更新配置

### Claude's Discretion

- 具体的函数命名风格（`default_protocols()` vs `protocols_default()`）
- `AliasSource` 是否实现 `Default` trait
- 是否需要添加构造函数辅助创建

### Deferred Ideas (OUT OF SCOPE)

- 协议路由检查逻辑（Phase 2）
- Alias 管理 API（Phase 3）
- WebUI 交互（Phase 4-6）

## 标准技术栈

### 核心依赖

| 库 | 版本 | 用途 | 来源 |
|----|------|------|------|
| serde | 1.0 | 序列化框架 | [Cargo.toml][VERIFIED: Cargo.toml] |
| serde_derive | 1.0 | 派生宏 | [Cargo.toml][VERIFIED: Cargo.toml] |
| serde_yaml | 0.9 | YAML 序列化 | [Cargo.toml][VERIFIED: Cargo.toml] |

### 现有模式参考

从 `src/models.rs` 中提取的现有模式：

```rust
// OverrideMode 枚举 - 可复用的序列化模式
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OverrideMode {
    Override,
    #[default]
    Default,
}

// 默认值函数模式
fn default_true() -> bool {
    true
}

// 在结构体字段中使用
#[serde(default = "default_true")]
pub enabled: bool,
```

## 实现模式

### 模式 1: 带默认值的枚举实现

**适用场景:** `AliasSource` 枚举需要默认值 `manual`

**实现方式:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AliasSource {
    Auto,
    Manual,
}

impl Default for AliasSource {
    fn default() -> Self {
        Self::Manual
    }
}
```

**来源:** [serde.rs/attr-default.html][CITED: serde.rs/attr-default.html] 文档展示了三种默认值模式，其中 "Using a custom function" 和 "Using Default trait implementation" 适用于此场景。

**注意:** serde 的 `#[default]` 属性仅对 struct 有效（需要 Rust 1.62+），对 enum 仍需要手动实现 `Default` trait。当前代码中的 `OverrideMode` 使用了 `#[default]`，这要求 Rust 版本 >= 1.62。

### 模式 2: 嵌套结构体的默认值函数

**适用场景:** `ProtocolSupport` 需要默认值 `{openai: true, anthropic: true}`

**实现方式:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProtocolSupport {
    #[serde(default = "default_true")]
    pub openai: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,  // anthropic -> enabled 避免重复
}

fn default_true() -> bool {
    true
}

// 或者为整个结构体提供默认值函数
fn default_protocols() -> ProtocolSupport {
    ProtocolSupport {
        openai: true,
        anthropic: true,
    }
}
```

**来源:** [serde.rs/attr-default.html][CITED: serde.rs/attr-default.html] 展示了使用自定义函数作为默认值的模式。

### 模式 3: 使用 Default trait 处理嵌套结构

**适用场景:** 整个 `ProtocolSupport` 作为字段，缺失时使用完整默认值

**实现方式:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProtocolSupport {
    #[serde(default = "default_true")]
    pub openai: bool,
    #[serde(default = "default_true")]
    pub anthropic: bool,
}

impl Default for ProtocolSupport {
    fn default() -> Self {
        Self {
            openai: true,
            anthropic: true,
        }
    }
}

// 在 UpstreamConfig 中使用
pub struct UpstreamConfig {
    #[serde(default)]  // 调用 ProtocolSupport::default()
    pub protocols: ProtocolSupport,
}
```

**来源:** [serde.rs/attr-default.html][CITED: serde.rs/attr-default.html] 文档说明："Use the type's implementation of std::default::Default if 'timeout' is not included in the input."

### 模式 4: 向后兼容的字段添加

**适用场景:** 向现有结构体添加新字段

**实现方式:**
```rust
// ModelAlias 添加 source 字段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAlias {
    pub alias: String,
    pub target_model: String,
    pub upstream: String,
    #[serde(default)]  // 缺失时使用 AliasSource::default()
    pub source: AliasSource,
    #[serde(default)]
    pub param_overrides: Vec<ParamOverride>,
}
```

**关键点:** `#[serde(default)]` 会自动调用类型的 `Default` trait 实现，因此 `AliasSource` 必须实现 `Default`。

## 推荐的完整实现

### 数据模型扩展

```rust
// 在 models.rs 中添加

/// Alias 来源标记
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AliasSource {
    /// 自动创建（点击上游模型名）
    Auto,
    /// 手动创建
    Manual,
}

impl Default for AliasSource {
    fn default() -> Self {
        Self::Manual
    }
}

/// 协议支持配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProtocolSupport {
    #[serde(default = "default_true")]
    pub openai: bool,
    #[serde(default = "default_true")]
    pub anthropic: bool,
}

impl Default for ProtocolSupport {
    fn default() -> Self {
        Self {
            openai: true,
            anthropic: true,
        }
    }
}

impl ProtocolSupport {
    #[allow(dead_code)]
    pub fn new(openai: bool, anthropic: bool) -> Self {
        Self { openai, anthropic }
    }

    #[allow(dead_code)]
    pub fn is_protocol_supported(&self, protocol: Protocol) -> bool {
        match protocol {
            Protocol::OpenAI => self.openai,
            Protocol::Anthropic => self.anthropic,
        }
    }
}

/// 协议类型（供后续 Phase 2 使用）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Protocol {
    OpenAI,
    Anthropic,
}

// 更新 ModelAlias
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAlias {
    pub alias: String,
    pub target_model: String,
    pub upstream: String,
    #[serde(default)]
    pub source: AliasSource,  // 新增
    #[serde(default)]
    pub param_overrides: Vec<ParamOverride>,
}

// 更新 UpstreamConfig
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]  // 调用 ProtocolSupport::default()
    pub protocols: ProtocolSupport,  // 新增
}
```

## 测试用例设计

基于现有 `tests/config.rs` 的模式，建议添加以下测试：

```rust
#[test]
fn test_alias_source_default() {
    let yaml = r#"
    alias: test
    target_model: gpt-4
    upstream: test-upstream
    "#;
    let alias: ModelAlias = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(alias.source, AliasSource::Manual);
}

#[test]
fn test_protocol_support_default() {
    let yaml = r#"
    name: test
    base_url: http://localhost:8080
    "#;
    let upstream: UpstreamConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(upstream.protocols.openai, true);
    assert_eq!(upstream.protocols.anthropic, true);
}

#[test]
fn test_protocol_support_explicit() {
    let yaml = r#"
    name: test
    base_url: http://localhost:8080
    protocols:
      openai: true
      anthropic: false
    "#;
    let upstream: UpstreamConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(upstream.protocols.openai, true);
    assert_eq!(upstream.protocols.anthropic, false);
}

#[test]
fn test_backward_compatibility() {
    // 旧配置文件（没有 source 和 protocols 字段）应能正常加载
    let yaml = r#"
    upstreams:
      - name: old-upstream
        base_url: http://old.example.com
    aliases:
      - alias: old-alias
        target_model: old-model
        upstream: old-upstream
    "#;
    let config: AppConfig = serde_yaml::from_str(yaml).unwrap();
    
    assert_eq!(config.upstreams[0].protocols.openai, true);
    assert_eq!(config.upstreams[0].protocols.anthropic, true);
    assert_eq!(config.aliases[0].source, AliasSource::Manual);
}
```

## 常见陷阱

### 陷阱 1: 忘记为枚举实现 Default

**问题:** 在结构体字段上使用 `#[serde(default)]` 但该类型没有实现 `Default`

**错误信息:**
```
error[E0277]: the trait bound `AliasSource: Default` is not satisfied
```

**解决:** 手动实现 `Default` trait

### 陷阱 2: 混淆 #[default] 和 impl Default

**问题:** 试图在 enum 上使用 `#[default]` 属性（仅对 struct 有效，且需要 Rust 1.62+）

**解决:** 为 enum 手动实现 `impl Default` trait

### 陷阱 3: 嵌套结构体的默认值函数参数错误

**问题:** `default_protocols()` 函数签名不正确

**错误示例:**
```rust
#[serde(default = default_protocols)]  // 缺少引号
```

**正确:**
```rust
#[serde(default = "default_protocols")]  // 需要引号
```

## 来源

### Primary (HIGH confidence)
- [serde.rs/attr-default.html][CITED: serde.rs/attr-default.html] - serde 默认值属性文档
- [Cargo.toml][VERIFIED: Cargo.toml] - 项目依赖版本
- `src/models.rs` - 现有代码模式
- `tests/config.rs` - 现有测试模式

### 置信度评估
- **标准栈:** HIGH - 基于项目现有依赖和 serde 官方文档
- **实现模式:** HIGH - 来自 serde 官方文档示例
- **测试用例:** HIGH - 基于项目现有测试模式

## 元数据

**研究完成时间:** 2026-04-22
**有效期至:** 2026-05-22（ serde 稳定特性，30 天）
