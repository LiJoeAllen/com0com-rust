---
type: milestone
status: achieved
target_date: 2026-Q4
created: 2026-10-20
last_refreshed: 2026-10-23
---

# 里程碑：可构建并加载的最小驱动

- 范围：在 eWDK/测试 VM 成功 `cargo wdk build` 两个内核驱动并签名加载；`setupc` 可创建一对并互发数据。
- 结果（2026-10-23）：WDK 10.0.28000 + wdk-sys 0.5.1 下两个驱动均构建并签名；
  Windows 11 24H2 测试 VM 中 `check-pairs.ps1` **31 项断言全过**，运行中的
  `com0combus.sys` / `com0comport.sys` SHA-256 与本次构建一致。
- 依赖：`[[risks]]` 中“内核无法在本机构建”与“wdk-sys 符号核对”均已解决。
- 关联：`openspec/specs/serial-io`、`openspec/specs/pair-management`、
  `tools/vm-test/`（可重复验证流程）。
