---
type: feature
status: implementing
last_updated: 2026-10-24
---

# 功能：跨平台 SDK 与 backend 分层

- `com0com-core`：领域类型、参数校验、13 个能力位、事件、状态模型、
  `Backend`/`MonitorHandle` trait；不依赖平台库。
- `com0com-backend-win`：SetupAPI + `DeviceIoControl` 对接现有驱动；线协议不变。
- `com0com-backend-pty`：Unix 用户态 PTY（`posix_openpt`/`grantpt`/`unlockpt`），
  raw termios、双向非阻塞泵、有界队列、波特率节流、噪声注入、溢出丢弃。
- `com0com-backend-stub`：非目标平台返回 `Unsupported`。
- `com0com-sdk`：同步 facade，`Send + Sync`；Unix `open()` 自动发现守护进程，
  `open_native()` 强制进程内 PTY。
- `com0com-ipc` + `com0comd`：本地 JSON-RPC（Windows named pipe / Unix socket），
  单实例、peer 凭据校验、`--restore`/`--autosave`。
- `com0com-capi`：12 个 C ABI 导出，头文件由 `tools/header-gen` 生成并被 CI 校验。
- 能力差异：PTY 的 exclusive/plugin/pin wiring/驱动诊断/驱动日志为 false；
  `setupc set` 整块替换参数，显式标签会被重置为默认 `A{id}`/`B{id}`。
- 验证：`tools/sdk-smoke`（fake/native/daemon）、`tools/capi-smoke`、
  `tools/vm-test/guest/check-pairs-unix.sh`、CI `unix-e2e`（手动触发）。
- 关联：[[architecture]]、[[setupc]]、[[environment]]
- 文档：`docs/sdk.md`、`docs/jsonrpc.md`、`docs/c-abi.md`、`docs/platforms.md`
