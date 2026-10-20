# project.md — com0com-rust 全景

## 它是什么
用 Rust 全量重写经典 **com0com**（Windows 虚拟 null-modem 串口驱动）。目标：不限数量的动态串口对，完整 `IOCTL_SERIAL_*` 表面（Win32 Comm API 透明可用），两端口经共享内存环缓冲转发，用户态 `setupc` 管理串口对。

## 架构（混合驱动模型）
| 组件 | 模型 | 产物 | 职责 |
|------|------|------|------|
| `com0com-common` | no_std 共享库 | — | `IOCTL_*` 控制码、`SERIAL_*` 结构体、`PairContext`/环缓冲、私有协议。内核态与用户态共用线格式。**100% 安全 Rust。** |
| `com0combus` | KMDF 总线 | `com0combus.sys` | 根 FDO，枚举每对串口的 2 个 PDO；持有每对 `PairContext`（非分页池）；经私有 IOCTL 暴露。 |
| `com0comport` | WDM 端口 | `com0comport.sys` | 运行在 PDO 上；实现 `IOCTL_SERIAL_*`；经 `PORT_LINK` 拿到 `PairContext` VA 并引用计数。 |
| `setupc` | 用户态 | `setupc.exe` | 经 `GUID_COM0COM_BUS` 打开总线，发私有 `IOCTL_COM0COM_*` 增删查串口对。 |

## 关键术语 / 概念
- **PairContext**：每对串口共享的非分页池结构（两个 `PortState` + 一把 `KSPIN_LOCK`），数据转发与取消安全的核心。
- **兼容 ID**：`c0c0_<pair_id_hex8>_<A|B>` 编码角色。
- **METHOD_BUFFERED**：所有 IOCTL 用缓冲方式，返回数据有一次必要内核→用户拷贝。
- **GUID_COM0COM_BUS**：总线设备接口 GUID，setupc 据此定位总线。

## 目录索引
- 源码：`crates/*`, `tools/setupc`
- 文档体系：`.agent/knowledge/`（本目录的上级）
- 技能速查：`.agent/skills/com0com-rust-dev/SKILL.md`

## 当前进度
- 已实现：混合驱动骨架、PDO 枚举、PORT_LINK/UNLINK 引用计数、完整 `IOCTL_SERIAL_*`、两端口转发与取消安全挂起 IRP、setupc、共享协议。
- 待办（P2）：波特率时序仿真、噪声/溢出注入、队列大小、XOFF 计数、注册表持久化、网络扩展。
