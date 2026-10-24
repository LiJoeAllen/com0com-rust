# Spec: kernel-backend-contract — 多内核统一契约

## 功能概述

定义平台无关的 `Backend` 语义契约。Windows 由现有 Rust 内核驱动实现，Linux/macOS 由纯 Rust 用户态 PTY 实现；未来平台只需新增 backend，不修改 SDK/UI。

## 需求

- REQ-1 `Backend` 提供 `info`、`capabilities`、pair CRUD、参数读写、端口枚举、监视器、驱动诊断/日志、事件订阅。
- REQ-2 pair id 范围 `0..=63`；显式 id 冲突返回 `AlreadyExists`；`None` 取最小空闲 id；最多 64 对。
- REQ-3 `set_params` 替换整对参数；`noise_probability` 范围 `0..=10000`；删除不存在的 pair 返回 `NotFound`。
- REQ-4 `BackendInfo` 包含 `platform`、`kernel`（`WindowsKmdfWdm | UnixPty | None`）、`kernel_version`、`protocol_version`（首期 1）。
- REQ-5 `Capabilities` 是位标志；能力位必须与实现一致，未实现的功能必须为 false。
- REQ-6 `SdkError` 统一错误集合，由各 backend 映射；不得让平台错误码泄漏到 UI 接口。
- REQ-7 `list_ports` 返回可直接打开的 `PortInfo`；`managed` 表示是否由本工具管理。
- REQ-8 `subscribe` 支持多个并存订阅、有界队列与丢弃计数；串口数据只通过 `Monitor`，不进入全局事件。
- REQ-9 Windows backend 继续使用 `com0com-common` 的 5 个私有 IOCTL 与既有线协议。
- REQ-10 Unix backend 使用 PTY，`PARAM_EXCLUSIVE`/`PARAM_PLUGIN`/`PARAM_PIN_WIRING`/`DRIVER_DIAGNOSTICS`/`DRIVER_LOGS` 能力位为 false。
- REQ-11 daemon 仅提供本地 IPC（Windows named pipe / Unix socket），不监听网络；Unix 下通过 `SO_PEERCRED`/`getpeereid` 校验对端 UID，`--allow-uid` 非空时 socket 权限为 0660，否则 0600。
- REQ-12 Unix 端口链接：root 写 `/dev/com0com/<label>`（macOS 同时建 `cu.`/`tty.` 别名），非 root 回退到 `$XDG_RUNTIME_DIR/com0com` 或 `~/.local/state/com0com/dev`、`~/Library/Application Support/com0com/ports`；仅替换指向 PTY 的陈旧链接，普通文件绝不覆盖。
- REQ-13 `serial.open` 只接受 `list_ports()` 返回的 managed 端口，由 backend 解析实际路径，不接受调用方传入的任意路径。

## 能力矩阵

| 能力 | Windows | Unix PTY | Stub |
|---|---|---|---|
| `PAIR_MANAGEMENT` | ✓ | ✓ | — |
| `PARAM_BAUD_EMU` | ✓ | ✓（节流） | — |
| `PARAM_NOISE` | ✓ | ✓（注入） | — |
| `PARAM_OVERRUN` | ✓ | ✓（drop-only） | — |
| `PARAM_EXCLUSIVE` | ✓ | — | — |
| `PARAM_PLUGIN` | ✓ | — | — |
| `PARAM_PIN_WIRING` | ✓ | — | — |
| `SERIAL_MONITOR` | ✓ | ✓ | — |
| `DRIVER_DIAGNOSTICS` | ✓ | — | — |
| `DRIVER_LOGS` | ✓ | — | — |
| `STATE_PERSISTENCE` | ✓ | ✓（daemon） | — |
| `EVENTS` | ✓ | ✓（daemon） | — |
| `REMOTE_DAEMON` | ✓ | ✓ | — |

## 验收场景

- AC-1 `cargo test --workspace` 中，fake backend 覆盖 CRUD、参数范围、事件过滤、背压与状态恢复。
- AC-2 Windows VM 31 项 setupc 回归 + SDK/C ABI/RPC 冒烟（实际 47 项断言）逐字通过，驱动哈希一致。
- AC-3 Linux/macOS 上 CRUD、双向传输、Monitor、事件、daemon 恢复通过；不支持的能力位为 false。
- AC-4 `cargo check --workspace` 在 Linux/macOS/Windows 均通过（backend 非目标平台返回 `Unsupported`）。

## 状态

- 现状：Windows backend、Unix PTY backend、SDK/daemon/C ABI 已实现；Linux PTY
  E2E 非 root 51/51、root 60/60（WSL2 静态 musl，root 段覆盖 `/dev` 默认链接），
  Windows VM E2E 47/47（2026-10-24，`target/vm-test-results/20261024-034210/`）；
  仅剩 macOS PTY E2E 实机结果。见 `changes/pure-rust-cross-platform-sdk/tasks.md`。
