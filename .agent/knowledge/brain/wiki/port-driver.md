---
type: feature
status: done
last_updated: 2026-10-20
---

# 功能：WDM 端口驱动（com0comport）

- 运行在总线枚举的 PDO 上，实现 `IOCTL_SERIAL_*`。
- 经 `PORT_LINK` 拿到 `PairContext` VA 并引用计数。
- 读/写可挂起 pending IRP，取消安全。
- 关联：`[[bus-driver]]`、`[[serial-io]]`
- 关键源文件：`crates/com0comport/src/{driver,dispatch,readwrite,serial,comname}.rs`
