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
status: active
last_refreshed: 2026-10-20
---

# 风险：内核无法在本机构建

- 严重度：高（阻塞验证）
- 影响：无 MSVC/WDK、coreutils `link` 遮蔽 `link.exe`，无法链接 `.sys`/`.exe`；只能 GNU 目标类型检查。
- 缓解：类型检查用 `cargo +nightly-x86_64-pc-windows-gnu check -p com0com-setupc`；完整构建/签名须在 eWDK/测试 VM。
- 触发信号：需要跑真实驱动 / 做 HLK 测试。

---
type: risk
status: active
last_refreshed: 2026-10-20
---

# 风险：wdk-sys 符号名待上机核对

- 严重度：中
- 影响：`POOL_FLAG_NON_PAGED`、`WDF_CHILD_RETRIEVE_INFO`、`WdfChildList*`、`IoReleaseCancelSpinLock`、`KeWaitForSingleObject` 等以“按官方拼写”书写，未在上机 WDK 验证。
- 缓解：首次 eWDK 构建逐项核对，以 WDK 头文件为准。
