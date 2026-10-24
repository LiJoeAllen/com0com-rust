---
type: risk
status: resolved
last_refreshed: 2026-10-24
---

# 风险：P2 仿真参数为 stub

- 严重度：中 → 已解决
- 原影响：`emu_br`/`emu_noise`/`emu_overrun` 参数已存储但数据路径未生效；`SET_QUEUE_SIZE`/`SERIAL_XOFF_COUNTER` 为 no-op。
- 解决：Windows 驱动已实现波特率预算、确定性噪声位翻转、溢出计数与队列上限；
  Unix PTY 实现节流、噪声注入与 drop-only 溢出。均有单测/VM 覆盖。

---
type: risk
status: resolved
last_refreshed: 2026-10-23
---

# 风险：内核无法在本机构建

- 严重度：高（阻塞验证）→ 已解决
- 原影响：无 MSVC/WDK、coreutils `link` 遮蔽 `link.exe`，无法链接 `.sys`/`.exe`；只能 GNU 目标类型检查。
- 解决：构建机已安装 WDK 10.0.28000 + Windows SDK 10.0.26100 + libclang 18，
  路径统一由 `.env` / `.env.local` 提供；两个驱动均可用 `cargo wdk build` 构建并签名。
  `build-package.ps1` 还会把 SDK 的 `signtool` 前置到 `PATH`，避免 MSYS 同名工具遮蔽。
- 现状：无 WDK 的机器仍只能构建 `com0com-common` / `setupc`，但这已不是本项目的验证阻塞项。

---
type: risk
status: resolved
last_refreshed: 2026-10-23
---

# 风险：wdk-sys 符号名待上机核对

- 严重度：中 → 已解决
- 原影响：`POOL_FLAG_NON_PAGED`、`WDF_CHILD_RETRIEVE_INFO`、`WdfChildList*`、
  `IoReleaseCancelSpinLock`、`KeWaitForSingleObject` 等以“按官方拼写”书写，未在上机 WDK 验证。
- 解决：wdk-sys 0.5.1 下两个驱动编译通过，并在 Windows 11 24H2 测试 VM 中加载运行；
  `check-pairs.ps1` 的 31 项断言全过，运行中驱动哈希与构建产物一致。

---
type: risk
status: active
last_refreshed: 2026-10-24
---

# 风险：macOS PTY 运行时尚未实机验证

- 严重度：中
- 影响：Linux（WSL2 + 静态 musl，非 root 51/51、root 60/60，含
  `/dev` 默认链接与 0600 状态文件，日志
  `target/vm-test-results/unix-wsl-20261024-0245/`）与 Windows VM（47/47，含
  SDK/C ABI/RPC、波特率仿真写路径与 `--restore`/`--autosave`，日志
  `target/vm-test-results/20261024-034210/`）均已实机通过；macOS 目前只做了
  交叉目标编译检查，`ptsname`/符号链接权限/termios 运行时行为未执行。
- 缓解：macOS 交叉 clippy 通过；共享 `/dev` 不再 chmod，用户/root 的默认状态与
  socket 路径已分离，状态文件 0600、用户目录 0700；CI `unix-e2e`
  （workflow_dispatch + `RUN_UNIX_E2E=true`）准备就绪，需在 macOS 主机或
  经授权的 CI 上运行 `check-pairs-unix.sh`。
- 触发信号：macOS `ptsname`/符号链接权限或 termios 行为偏离。

---
type: risk
status: resolved
last_refreshed: 2026-10-24
---

# 风险：KeQueryInterruptTimePrecise 传 NULL 触发 0xA

- 严重度：高 → 已解决
- 原影响：波特率仿真路径把 `KeQueryInterruptTimePrecise` 的 `_Out_ PULONG64`
  实参写成空指针，在 DISPATCH_LEVEL 写地址 0，VM 实测 BSOD `0xA`。
- 解决：改为传入栈上 `u64`；反汇编确认调用点已是 `lea rsp+0x208, %rcx`。
  VM 回归新增 `setupc set 5 --baud-emu --overrun` + 实际写读断言，
  47/47 全过、无 BSOD（日志 `target/vm-test-results/20261024-034210/`）。

---
type: risk
status: resolved
last_refreshed: 2026-10-24
---

# 风险：读/事件 IRP 发布与取消竞态

- 严重度：高 → 已解决
- 原影响：`handle_read` 与 `IOCTL_SERIAL_WAIT_ON_MASK` 在“挂起 IRP”与
  `IoCancelIrp` 之间存在窗口，可能造成双重完成或 UAF。
- 解决：统一为“pair 锁内再取 cancel 锁”的发布路径（固定锁序 pair → cancel），
  取消例程先释放 cancel 锁再取 pair 锁；同步快路径不再安装取消例程。
  VM 回归覆盖 `PurgeComm`/取消/长读。

---
type: risk
status: active
last_refreshed: 2026-10-24
---

# 风险：能力位与实际实现漂移

- 严重度：中
- 影响：UI 依赖 `Capabilities` 决定展示；若 backend 虚报能力，会在运行时失败。
- 缓解：PTY backend 对 exclusive/plugin/pin wiring 直接返回 `Unsupported`；
  E2E 断言拒绝路径；新增能力必须先改 `com0com-core` 并补测试。
- 触发信号：新增 backend 或新增能力位。

---
type: risk
status: resolved
last_refreshed: 2026-10-24
---

# 风险：C ABI 布局漂移

- 严重度：中 → 已解决
- 解决：`tools/header-gen` 从 Rust 常量/布局生成头文件快照，CI 执行 `--check`
  diff 校验；capi-smoke 自检导出函数与结构体布局。

---
type: risk
status: active
last_refreshed: 2026-10-24
---

# 风险：守护进程攻击面（本地 IPC）

- 严重度：中
- 影响：daemon 以 root 运行时若 socket 权限或对端校验有误，可能被本机其他用户操纵串口。
- 缓解：不监听网络；Unix socket 默认 0600、`--allow-uid` 时 0660 并由
  `SO_PEERCRED`/`getpeereid` 校验 UID；`serial.open` 只接受受管端口；
  Windows named pipe 保留 ACL 与单实例。
- 触发信号：新增 RPC 方法或放开 socket 权限。

---
type: risk
status: resolved
last_refreshed: 2026-10-24
---

# 风险：macOS 共享 `/dev` 被 chmod

- 严重度：中 → 已解决
- 原影响：`LinkConfig::at` 对已存在的目录也执行 chmod；root 在 macOS 上会把 `/dev`
  的权限改成 0755。目录创建逻辑现只对本次新建的目录设权限，`/dev` 与其它既有目录
  完全不动；用户端口目录按 0700 创建。

---
type: risk
status: resolved
last_refreshed: 2026-10-24
---

# 风险：relabel 回滚误删普通文件

- 严重度：高（数据丢失）→ 已解决
- 原影响：`PtyPair::relabel` 失败回滚用 `remove_file` 直删新路径；macOS 的
  `tty.com0com-*` 别名位置若被普通文件占用，会删掉用户数据。
- 解决：回滚改用 `remove_link`，只删除仍指向本端口 PTY 的符号链接；新增单测
  `relabel_failure_rolls_back_without_touching_regular_files`。
