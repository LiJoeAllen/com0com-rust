---
type: milestone
status: in-progress
target_date: 2026-Q4
---

# 里程碑：可构建并加载的最小驱动

- 范围：在 eWDK/测试 VM 成功 `cargo wdk build` 两个内核驱动并签名加载；setupc 可创建一对并互发数据。
- 依赖：`[[risks]]` 中“内核无法在本机构建”与“wdk-sys 符号核对”解决。
- 关联：`openspec/specs/serial-io`、`openspec/specs/pair-management`
