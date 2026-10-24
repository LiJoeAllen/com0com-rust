# 设计：跨平台纯 Rust 分层 + 面向 UI 的 SDK

## 架构

```text
UI（任意语言/框架，本仓库不实现）
  ├─ Rust SDK（com0com-sdk）
  ├─ C ABI（com0com-capi，dll/so/dylib）
  └─ JSON-RPC（com0com-ipc / com0com-daemon）
        │
  com0com-core::Backend
        │
  backend-win / backend-pty / backend-stub
        │
  Windows Rust 驱动 / Unix PTY 桥
```

- `com0com-core` 只含领域类型、校验、能力、错误、事件、状态模型与 trait；不依赖 `windows-sys`、`libc`、WDK。
- `com0com-backend-win` 使用 SetupAPI + `DeviceIoControl` 对接现有驱动；FFI 仅存在于 transport/diagnostics。
- `com0com-backend-pty` 使用 `libc` 的 POSIX 调用创建 PTY，每对一个 pump 线程，双向有界队列与 `poll`。
- `com0com-sdk` 提供同步阻塞门面；`Sdk` 为 `Send + Sync`，设备操作内部串行化。
- `com0com-ipc` 同时包含客户端 `RemoteBackend` 与服务端分发；Windows 用 named pipe，Unix 用 Unix socket。
- `com0com-capi` 每个 `extern "C"` 包 `catch_unwind`，调用方分配缓冲区，头文件由 Rust 定义生成。

## 关键决策

1. **Unix 不用内核驱动**：Linux C TTY 模块 / macOS DriverKit dext 违反 0 C/C++ 约束，且需要签名、SIP 或内核 ABI 维护。PTY 用户态桥是唯一纯 Rust 路径，代价是缺少 pin wiring/exclusive/plugin/驱动诊断，能力位如实为 false。
2. **能力位驱动 UI**：`Capabilities` 为位标志，UI 先查能力再展示功能；未实现项禁止伪装。
3. **线协议不变**：Windows 继续使用 `com0com-common` 的 160 字节 `Com0comPairParams` 与 5 个私有 IOCTL；核心类型通过 `convert.rs` 转换，并以测试断言 `DEFAULT_PIN_WIRING == PinWiring::NULL_MODEM`。
4. **`setupc` 先搬 backend 再改薄**：每一步都跑 VM 31 项回归，输出金样例单测锁定文案与退出码。
5. **C ABI 头文件生成**：`tools/header-gen` 从 Rust 常量/布局生成快照，CI diff 校验；Rust 侧 `size_of`/`offset_of` 测试双保险。
6. **安全模型**：Windows named pipe ACL；Unix socket peer creds；状态路径固定；RPC 只接受 `list_ports()` 返回的 managed 路径；无网络监听。
7. **持久化**：JSON schema 1，原子写（临时文件 + `sync_all` + rename）；PTY 路径不持久化，恢复时重建。

## 平台语义

| 项 | Windows | Unix PTY |
|---|---|---|
| pair id | 0..=63 | 0..=63 |
| 端口名 | COM 名，最长 31 UTF-16 | 标签 `[A-Za-z0-9._-]{1,31}` |
| 空名 | Name Arbiter 自动分配 | `A{id}` / `B{id}` |
| KernelKind | `WindowsKmdfWdm` | `UnixPty` |
| 监控 | 打开 COM 端口 | 打开 PTY 符号链接/路径 |
| 驱动日志 | wevtapi | 能力位 false，daemon 日志单独提供 |

## 错误模型

`SdkError`：`Unsupported`、`PermissionDenied`、`DriverUnavailable`、`NotFound`、`AlreadyExists`、`InvalidArgument`、`Busy`、`Io`、`Win32 { op, code, hint }`、`Timeout`、`DaemonUnavailable`、`Protocol`、`Internal`。各 backend 负责把平台错误映射到该集合。

## 风险

见提案对应计划的风险表：PTY 可见性、macOS `/dev/cu.*` 兼容性、能力缺失、root daemon 攻击面、Windows 回归、C ABI 布局漂移、范围过大。每项在实施阶段有对应验证或能力位降级。

## 验收

见 `tasks.md` 与计划验证清单：fmt/clippy/test、依赖检查、纯 Rust 源码检查、Windows VM 31 项 + SDK/C ABI/RPC 冒烟、Linux/macOS PTY E2E、安全复查、文档同步。
