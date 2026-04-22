# Phase 1: 数据模型扩展 - 讨论日志

> **审计追踪仅**。不作为规划、研究或执行代理的输入。
> 决策已记录在 CONTEXT.md 中 — 此日志保留考虑的替代方案。

**日期:** 2026-04-22
**Phase:** 01 - 数据模型扩展
**讨论区域:** 序列化格式、默认值策略、向后兼容

---

## AliasSource 序列化格式

| 选项 | 描述 | 选中 |
|------|------|------|
| 小写字符串 | auto / manual（与现有 OverrideMode 一致） | ✓ |
| 大写字母 | AUTO / MANUAL | |
| 数字标记 | 0 / 1（紧凑存储） | |

**用户选择:** 小写字符串
**笔记:** 与现有 `OverrideMode` 保持一致，YAML 可读性好。

---

## 协议默认值策略

| 选项 | 描述 | 选中 |
|------|------|------|
| 双协议启用 | openai: true, anthropic: true（最宽松，向后兼容） | ✓ |
| 智能推断 | 根据 base_url 推断（如 anthropic.com → 只启用 anthropic） | |
| 显式警告 | 加载旧配置时打印警告，要求用户显式配置 | |

**用户选择:** 双协议启用
**笔记:** 最宽松的向后兼容策略，旧配置无缝工作。

---

## Source 默认值

| 选项 | 描述 | 选中 |
|------|------|------|
| 默认为 manual | 从 YAML 手动加载的 alias 默认是 manual | ✓ |
| 默认为 auto | 所有 alias 默认 auto，用户可改 manual | |

**用户选择:** 默认为 manual
**笔记:** 符合直觉，auto 应该只由系统自动创建。

---

## Claude's Discretion

以下领域用户表示由 Claude 自行决定：
- 具体的函数命名风格（`default_protocols()` vs `protocols_default()`）
- `AliasSource` 是否实现 `Default` trait
- 是否需要添加构造函数辅助创建

---

## 延迟的想法

None — 讨论保持在 Phase 1 范围内。
