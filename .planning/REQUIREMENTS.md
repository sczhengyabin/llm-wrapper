# 需求文档

## 功能需求

### F1: 上游模型点击创建 Alias

**用户故事**: 作为用户，我希望点击上游模型名快速创建 alias，这样就不用手动输入配置。

**验收标准**:
- [ ] 上游列表中模型名显示为可点击的灰色框
- [ ] 点击模型名时，自动创建与上游模型同名的 alias
- [ ] 创建的 alias 为透传模式（target_model = 模型名，upstream = 当前上游）
- [ ] 新创建的 alias 默认启用（状态为绿色）
- [ ] 如果重名，则不创建，显示红色状态

**交互流程**:
```
用户点击 "claude-3-5-sonnet"
  ↓
检查是否重名
  ├─ 不重名 → 创建 alias "claude-3-5-sonnet" → 显示绿色 ✓
  └─ 重名 → 不创建 → 显示红色 ✗
```

---

### F2: Alias 标签系统

**用户故事**: 作为用户，我希望区分 auto 和 manual 创建的 alias，这样能更好地理解配置来源。

**验收标准**:
- [ ] ModelAlias 添加 `source` 字段，值为 `auto` 或 `manual`
- [ ] WebUI 中 alias 列表显示 source 标签
- [ ] auto 标签：点击上游模型名自动创建
- [ ] manual 标签：通过手动添加 alias 功能创建

**数据模型变更**:
```rust
pub struct ModelAlias {
    pub alias: String,
    pub target_model: String,
    pub upstream: String,
    pub param_overrides: Vec<ParamOverride>,
    pub source: AliasSource,  // 新增
}

pub enum AliasSource {
    Auto,
    Manual,
}
```

---

### F3: 重名检测与处理

**用户故事**: 作为用户，我希望在重名时有清晰的反馈和处理方式。

**验收标准**:
- [ ] 创建 alias 前检查是否与已有的 alias 或 target_model 重名
- [ ] Auto 创建重名时：
  - 不创建新的 alias
  - 模型名显示红色
  - 鼠标悬停显示提示"名称已存在"
- [ ] Manual 创建重名时：
  - 添加失败
  - 显示错误提示"别名已存在，请更换其他名称"
- [ ] Auto 重名后，如果用户手动删除了冲突的 alias，再点击模型名应能成功创建

**状态映射**:
| 状态 | 颜色 | 说明 |
|------|------|------|
| 可用/已启用 | 绿色 | 可以点击创建或已启用 |
| 重名/禁用 | 红色 | 无法创建或已禁用 |

---

### F4: 协议支持开关

**用户故事**: 作为用户，我希望为每个上游配置支持的协议，这样能避免无效请求。

**验收标准**:
- [ ] UpstreamConfig 添加 `protocols` 字段
- [ ] WebUI 编辑上游时显示协议开关（OpenAI / Anthropic）
- [ ] 添加上游时，默认两个协议都启用
- [ ] 客户端请求不支持的协议时：
  - 返回 422 Unprocessable Entity
  - 错误信息："上游 XXX 不支持 XXX 协议"
  - 不转发请求给上游

**数据模型变更**:
```rust
pub struct UpstreamConfig {
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub enabled: bool,
    pub protocols: ProtocolSupport,  // 新增
}

pub struct ProtocolSupport {
    pub openai: bool,
    pub anthropic: bool,
}
```

**路由逻辑变更**:
```rust
// 在 route() 中检查协议支持
if protocol == Protocol::OpenAI && !upstream.protocols.openai {
    return Some(ProtocolError::Unsupported);
}
```

---

### F5: 侧边栏模型列表

**用户故事**: 作为用户，我希望在侧边栏看到所有可用模型，这样能快速了解当前聚合了哪些模型。

**验收标准**:
- [ ] WebUI 左侧添加固定侧边栏
- [ ] 侧边栏显示 `/v1/models` 返回的模型列表
- [ ] 模型列表可滚动（超出高度时）
- [ ] 可选：点击模型名可快速筛选或跳转

**布局**:
```
┌─────────────┬───────────────────┐
│ 模型列表    │                   │
│ • claude-3  │   主内容区        │
│ • gpt-4     │                   │
│ • llama-3   │                   │
│   ...       │                   │
└─────────────┴───────────────────┘
```

---

## 非功能需求

### N1: 性能
- 模型列表加载应在 100ms 内完成
- 点击创建 alias 的响应时间 < 500ms

### N2: 兼容性
- 保持现有配置文件的向后兼容
- 新增字段使用默认值处理旧配置

### N3: 用户体验
- 所有状态变化有明确的视觉反馈
- 错误提示清晰易懂

---

## 依赖关系

| 需求 | 依赖 |
|------|------|
| F1 | F3（重名检测） |
| F3 | F2（Alias 标签） |
| F4 | 无 |
| F5 | 无 |
