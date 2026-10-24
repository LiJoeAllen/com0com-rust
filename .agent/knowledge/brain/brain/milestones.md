---
type: milestone
status: achieved
target_date: 2026-Q4
created: 2026-10-20
last_refreshed: 2026-10-23
---

# 里程碑：可构建并加载的最小驱动

- 范围：在 eWDK/测试 VM 成功 `cargo wdk build` 两个内核驱动并签名加载；`setupc` 可创建一对并互发数据。
- 结果（2026-10-23）：WDK 10.0.28000 + wdk-sys 0.5.1 下两个驱动均构建并签名；
  Windows 11 24H2 测试 VM 中 `check-pairs.ps1` **31 项断言全过**，运行中的
  `com0combus.sys` / `com0comport.sys` SHA-256 与本次构建一致。
- 依赖：`[[risks]]` 中“内核无法在本机构建”与“wdk-sys 符号核对”均已解决。
- 关联：`openspec/specs/serial-io`、`openspec/specs/pair-management`、
  `tools/vm-test/`（可重复验证流程）。

---
type: milestone
status: in-progress
target_date: 2026-Q4
created: 2026-10-24
last_refreshed: 2026-10-24
---

# 里程碑：跨平台分层 SDK（Windows 内核 + Unix PTY）

- 范围：`com0com-core` → backend（win/pty/stub）→ `com0com-sdk`；Rust facade、
  C ABI、本地 JSON-RPC daemon 三种形态；setupc 改薄；Windows 行为不变。
- 现状（2026-10-24）：全部代码与文档已落地。宿主 fmt/clippy/test、Linux-musl 与
  macOS 交叉目标 clippy、头文件漂移检查、`check-deps -Fix` 无 diff 均通过；
  Linux PTY E2E 在 WSL2 跑通非 root 51/51、root 60/60（静态 musl，root 段
  覆盖 `/dev/com0com` 默认链接、双向传输与 0600 状态文件，日志
  `target/vm-test-results/unix-wsl-20261024-0245/`）；Windows VM E2E **47/47、
  0 失败**，两个驱动 SHA-256 与构建产物一致，SDK/C ABI/RPC 冒烟、
  波特率仿真写路径与 `--restore`/`--autosave` 实测通过（日志
  `target/vm-test-results/20261024-034210/`）。macOS 路径/权限审计已完成
  （共享 `/dev` 不 chmod、状态文件 0600、非 root 目录 0700、relabel 回滚不删
  普通文件）。仅剩 macOS PTY E2E 需在 macOS 主机或授权的 CI
  `workflow_dispatch` 上执行。
- 验收：`openspec/changes/pure-rust-cross-platform-sdk/tasks.md` 全绿。
- 关联：`[[sdk-and-backends]]`、`[[architecture]]`、`[[risks]]`。
