---
type: risk
status: active
last_refreshed: 2026-10-20
---

# 风险：P2 仿真参数为 stub

- 严重度：中
- 影响：`emu_br`/`emu_noise`/`emu_overrun` 参数已存储但数据路径未生效；`SET_QUEUE_SIZE`/`SERIAL_XOFF_COUNTER` 为 no-op。
- 缓解：当前以最小可用驱动为目标；后续迭代实现。
- 触发信号：用户要求波特率时序/噪声注入行为生效。

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
