---
type: decision
status: accepted
created: 2026-10-20
---

# 决策：采用混合驱动模型（KMDF 总线 + WDM 端口）

- 上下文：需要动态枚举任意数量串口对，又要精确控制每个端口的 `IOCTL_SERIAL_*` 行为。
- 决策：总线用 KMDF（易枚举/即插即用），端口用 WDM（精细控制 SERIAL 表面）。
- 理由：KMDF 简化总线/PnP；WDM 直接暴露 SERIAL 控制面，避免 KMDF 串口类驱动的限制。
- 替代：纯 KMDF 串口类驱动（限制多）、纯 WDM（枚举繁琐）。
- 后果：需分别维护两种模型；端口驱动需 WDM 绑定（crates.io `wdk-sys` 仅 KMDF 预生成绑定，须在装 WDK 环境构建）。

---
type: decision
status: accepted
created: 2026-10-20
---

# 决策：用 `.agent` 而非 `.workbuddy` 存放 agent/文档配置

- 上下文：用户可能使用多种 AI 工具，不希望绑定单一工具。
- 决策：技能与文档体系放在仓库根 `.agent/`（工具无关）。
- 理由：可被多种 AI 编码助手复用，不锁定 WorkBuddy。
- 后果：OpenSpec/PBH 原生 CLI 默认找仓库根 `openspec/`、`docs/`；本体系放在 `.agent/knowledge/` 下，需要时用软链或移动。
