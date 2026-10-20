---
type: feature
status: done
last_updated: 2026-10-20
---

# 功能：共享库（com0com-common）

- `no_std` 共享库；内核态与用户态共用 `IOCTL_*`/`SERIAL_*`/协议，`100% 安全 Rust`。
- 单测校验控制码顺序（`SET_BAUD_RATE==0x001B0004` 等）。
- 关键源文件：`crates/com0com-common/src/{ioctls,serial_structs,constants,protocol,pair}.rs`
