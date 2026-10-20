---
type: feature
status: done
last_updated: 2026-10-20
---

# 功能：KMDF 总线驱动（com0combus）

- 创建根 FDO，枚举每对串口的 2 个 PDO。
- 持有每对 `PairContext`（非分页池）。
- 经私有 `IOCTL_COM0COM_*` 暴露创建/删除/枚举/链接。
- 关联：`[[port-driver]]`、`[[pair-management]]`
- 关键源文件：`crates/com0combus/src/{bus,driver,pdo,ioctl}.rs`
