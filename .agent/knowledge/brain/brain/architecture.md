---
type: architecture
status: done
last_updated: 2026-10-20
---

# 架构（运行系统活地图）

混合驱动模型：KMDF 总线 FDO 枚举 WDM 端口 PDO。每对共享 `PairContext`（非分页池：2×PortState + KSPIN_LOCK）。数据经环缓冲在两端转发，由自旋锁保护。用户态 setupc 经总线设备接口管理串口对。

- 线格式唯一来源：`com0com-common`。
- 详见：`[[bus-driver]]`、`[[port-driver]]`、`[[common-lib]]`、`[[setupc]]`
- 规范：`openspec/specs/`
