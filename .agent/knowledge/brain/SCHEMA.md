# SCHEMA.md — Project Brain Hub 操作指南（PBH）

PBH 是面向 Agent 的项目知识中枢，分三层，全部是带 frontmatter 的 Markdown，用 `[[wikilinks]]` 互链（不用相对路径）。

## 目录
- `raw/`：不可变源输入（反馈、bug 报告、愿景、原始记录）。写入后不再改。
- `wiki/`：响应式层，一个功能一页，记录“当前状态”，随变化覆盖。
- `brain/`：指令层，跨切面、按时间锚定、append-only：架构、里程碑、决策、风险、开放问题。

## Frontmatter（每页至少）
```yaml
---
type: <feature|bug|feedback|vision|implementation|architecture|milestone|decision|risk|open-question>
status: <见下>
last_updated: YYYY-MM-DD
---
```
指令页（brain/）额外加时间字段：`created`、`target_date`、`last_refreshed`，以及血缘 `supersedes`/`superseded_by`。

## 状态值（Status）
- wiki/feature：`draft | implementing | done | deprecated`
- wiki/bug：`open | fixed | wontfix`
- brain/decision：`proposed | accepted | superseded`
- brain/risk：`active | mitigated | resolved`
- brain/milestone：`planned | in-progress | achieved`

## 规则
1. 单一事实源：同一事实只在一层存真（功能就绪状态放 wiki；里程碑是跨功能视图，放 brain，不存副本）。
2. 决策 append-only：接受后不修改，用 `superseded_by` 指向新决策。
3. 每页用 `[[wikilinks]]` 互链，形成知识图。
4. Agent 每会话先读本文件，再按需读 raw/wiki/brain。
