# 变更提案：跨平台纯 Rust 分层 + 面向 UI 的 SDK

## 背景与动机

当前仓库的用户态入口只有 `tools/setupc/src/main.rs`：参数解析、Win32 FFI、业务与输出全部混在一个 bin 里，没有可复用的 lib，UI 无法接入；内核侧只有 Windows 的 `com0combus`/`com0comport`，`com0com-common` 的线协议与 IOCTL 也只服务 Windows。

本次变更新增统一的分层契约，使任意语言/框架的 UI 可以像调用 SDK 一样使用全部能力，同时保持 Windows 内核驱动与既有 CLI 行为不变。

## 目标

- **多内核/多后端**：Windows 继续使用现有 Rust 内核驱动；Linux/macOS 使用纯 Rust 用户态 PTY 后端；未知平台返回明确的 `Unsupported`。
- **分层**：`com0com-core` 定义平台无关契约，backend 实现契约，SDK/接入层只依赖 SDK。
- **三种 SDK 形态**：Rust facade、C ABI 动态库、本地 JSON-RPC 守护进程。
- **0 C/C++ 源码**：所有实现代码为 Rust；C ABI 头文件由 Rust 定义生成。
- **不实现具体 GUI**：只提供接口与文档。
- **兼容**：`setupc` 改为 SDK 的薄 CLI，命令、输出文本与退出码保持不变。

## 范围

包含：串口对管理、参数读写、驱动诊断、串口监视/终端数据接口、状态持久化与恢复、事件订阅、可选 daemon。

不包含：Linux/macOS 内核驱动、任何手写 C/C++、任何具体 GUI、网络串口、安装器/自动更新。

## 影响

- 新增 8 个 crate、3 个工具、OpenSpec spec 与文档。
- 根 workspace 成员与 CI 扩展；`.cargo/config.toml` 的 `crt-static` 改为仅 Windows 目标生效。
- Windows 驱动线协议、驱动源码与注册表行为不变。
- `setupc` 的 FFI 搬入 backend，命令输出与退出码保持逐字兼容（VM 31 项回归）。

## 相关文档

- `docs/sdk.md`、`docs/jsonrpc.md`、`docs/c-abi.md`、`docs/platforms.md`
- `openspec/specs/kernel-backend-contract/spec.md`
- `.agent/knowledge/brain/` 架构/决策/风险/环境
