---
type: architecture
status: done
last_updated: 2026-10-24
---

# 架构（运行系统活地图）

跨平台分层：`com0com-core` 定义领域契约，平台 backend 实现它，`com0com-sdk` 面向 UI
提供三种接入形态；Windows 数据面仍由 Rust 内核驱动承载，Unix 数据面是用户态 PTY。

```
UI（任意语言，本仓库不做）
  ├─ Rust facade（com0com-sdk）
  ├─ C ABI（com0com-capi，dll/so/dylib + 生成头文件）
  └─ JSON-RPC（com0comd ↔ com0com-ipc）
        │
  com0com-core::Backend
        │
  backend-win / backend-pty / backend-stub
        │
  Windows Rust 驱动 / Unix PTY 桥
```

- Windows：KMDF 总线 FDO 枚举 WDM 端口 PDO。每对共享 `PairContext`（非分页池：
  2×PortState + KSPIN_LOCK）。数据经环缓冲在两端转发，由自旋锁保护。
- Unix：每对一个 PTY 对 + 两个 pump 线程，master 侧非阻塞 `poll`；slave “holder” fd
  防止 hangup 并承载 termios；链路为符号链接，root 写 `/dev`，非 root 回落到用户目录。
- 守护进程：Windows named pipe / Unix socket，仅本地 IPC；状态 JSON schema 1 原子写，
  `--restore`/`--autosave`；`serial.open` 只接受受管端口。
- `setupc`：SDK 之上的薄 CLI，命令/输出/退出码保持兼容。
- 能力位如实反映差异，PTY 不支持 exclusive/plugin/pin wiring/驱动诊断/驱动日志。
- 详见：[[sdk-and-backends]]、[[bus-driver]]、[[port-driver]]、[[common-lib]]、[[setupc]]、[[environment]]
- 规范：`openspec/specs/`

## Cargo 布局：三个 workspace 根（硬约束，勿合并）

```
根            Cargo.toml              members = com0com-common + core/sdk/ipc/daemon/capi/backends + tools，exclude 两个驱动
├── com0combus   crates/com0combus/Cargo.toml    [workspace]，driver-type = KMDF
└── com0comport  crates/com0comport/Cargo.toml   [workspace]，driver-type = WDM
```

**为什么不能合并成一个 workspace**（源码级结论，`wdk-build 0.5.1`）：

`src/metadata/mod.rs` 的 `impl TryFrom<&Metadata> for Wdk` 会把
`metadata.packages`（**整张 cargo metadata，即 workspace 全部成员**）里每个包的
`package.metadata.wdk` 与根的 `workspace.metadata.wdk` 一并收集进 `HashSet<Wdk>`，
然后要求 `len() == 1`，否则直接 `MultipleWdkConfigurationsDetected`。
注意它统计的是**全图**而不是当前包的依赖闭包，所以 `cargo build -p com0combus`
也会看到 port 的 WDM 声明而中止。

后果：
- 两个驱动必须各自是独立 workspace root，**不能**用 `-p` 选中构建，只能用
  `cargo build --manifest-path crates/<driver>/Cargo.toml`
- 各 manifest 里的 `[workspace.package]` / `[workspace.dependencies]` 因此**无法**
  通过 `workspace = true` 继承来消除重复；根 `Cargo.toml` 是唯一手写源，
  `tools/check-deps.ps1`（带 `-Fix` 可同步）强制副本一致，pre-commit + CI 兜底
- 副作用但有利：驱动 manifest 里的 `[profile.*] panic = "abort"` 作为 workspace 根会
  真正生效（在非根只会被忽略并刷告警）
- 若未来上游放开该限制（只按依赖闭包判断），即可合并回单一 workspace

- 线格式唯一来源：`com0com-common`（Windows backend 的 `convert.rs` 负责领域类型转换）。
