# PKF — com0com-rust Project Knowledge File

> 结构化项目知识文件，可跨项目/跨会话复用。格式：背景 / 摘要 / 详情 / 复用条件 / 参考。

## 1. 背景（Background）
- 项目：com0com-rust
- 类型：Windows 内核驱动（虚拟串口）的 Rust 全量重写
- 动机：原版 com0com 为 C/WDM，维护成本高；Rust 可提供内存安全与现代化工具链。
- 起止：2026 年起；当前阶段：骨架 + 核心转发已实现，P2 特性待办。
- 参与方：个人项目（JoeAllen）

## 2. 摘要（Abstract）
用 KMDF 总线 + WDM 端口的混合驱动模型实现虚拟 null-modem 串口对；内核态与用户态共享 `com0com-common`（100% 安全 Rust，定义 `IOCTL_*`/`SERIAL_*`/协议）；用户态 `setupc` 管理串口对。

## 3. 详情（Details）
- 架构：bus(FDO) 枚举 port(PDO)；`PairContext` 非分页池共享，自旋锁保护环缓冲。
- 关键决策：混合模型（KMDF 易枚举、WDM 控端口）；METHOD_BUFFERED；引用计数 PORT_LINK/UNLINK。
- 安全立场：common 零 unsafe；setupc 业务零 unsafe + 输入零拷贝；内核 unsafe 收敛 helper。
- 已知风险：P2 仿真参数仍为 stub；驱动已在 WDK 10.0.28000 + wdk-sys 0.5.1 下构建签名，
  并于 2026-10-23 在 Windows 11 24H2 测试 VM 通过 31 项端到端断言。
- 详见：`openspec/`、`security/`、`brain/`。

## 4. 复用条件（Conditions for Reuse）
- 适用于：Rust 内核驱动、Windows 虚拟设备、spec-driven 文档体系搭建。
- 不适用：非 Windows 平台、纯用户态串口库。
- 引用时请注明项目名与许可。

## 5. 参考（Reference）
- 源码：仓库 `crates/`、`tools/setupc`
- 文档体系：`.agent/knowledge/`
- 技能：`.agent/skills/com0com-rust-dev/SKILL.md`
- 上游：`windows-drivers-rs`、WDK `ntddser.h`
